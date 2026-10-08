#!/usr/bin/env python3
"""Generate local vanilla 1.21.1 collision/outline data from user-provided inputs.

Requires Python 3.11+ and a Java 21 JDK. Does not download files, start a server,
load a world, publish data, or overwrite an existing output directory. See
SHAPE_REFERENCE.md for provenance, invocation, schema, and explicit limitations.
"""
from __future__ import annotations

import argparse
import hashlib
import json
import math
import os
from pathlib import Path, PurePosixPath
import re
import subprocess
import sys
import zipfile

SERVER_SHA256 = "e3bc55693e93cda0188f2e60aea28113fc647c5e85a15fa3d1b347349231b4bb"
INNER_SHA256 = "c301de10f575027d13eac18c7f34409d60648cf56a35d566aa1f530ff617840a"
MAPPINGS_SHA256 = "9d0b04bead421c8229aff14b534432bbc927bea642e7c8593d1276b8df8ba53f"
CATALOG_SHA256 = "0dde7f869588905763ea6ff2e7e01bec1db740a58d27477fd27c2f07fb029f73"
MAX_FILE = 128 * 1024 * 1024
MAX_EXTRACTED = 512 * 1024 * 1024
HEX = re.compile(r"[0-9a-f]{64}\Z")


def require(condition, message):
    if not condition:
        raise ValueError(message)


def sha256(path):
    with Path(path).open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def checked_input(path, expected, label, limit):
    path = path.resolve(strict=True)
    require(path.is_file() and path.stat().st_size <= limit, "Invalid bounded " + label)
    require(sha256(path) == expected, label + " differs from the pinned original vanilla 1.21.1 reference")
    return path


def safe_relative(value):
    path = PurePosixPath(value)
    require(value and "\\" not in value and ":" not in value and not path.is_absolute()
            and all(p not in ("", ".", "..") for p in value.split("/")), "Unsafe bundle path")
    return path


def bundle_list(jar, kind):
    name = "META-INF/" + kind + ".list"
    require(jar.getinfo(name).file_size <= 128 * 1024, "Oversized bundle list")
    result, seen = [], set()
    for line in jar.read(name).decode("utf8").splitlines():
        parts = line.split("\t")
        require(len(parts) == 3 and HEX.fullmatch(parts[0]), "Invalid bundle list entry")
        digest, identifier, relative = parts
        path = safe_relative(relative)
        require(str(path) not in seen, "Duplicate bundle entry")
        seen.add(str(path))
        result.append((digest, identifier, path))
    require(0 < len(result) <= 256, "Unexpected bundle entry count")
    return result


def extract_reference(archive, output):
    """Extract only exact listed files, validating paths, bounds and SHA-256."""
    total, libraries = 0, []
    with zipfile.ZipFile(archive) as jar:
        names = [entry.filename for entry in jar.infolist()]
        require(len(names) == len(set(names)), "Duplicate archive member")
        require(jar.getinfo("version.json").file_size <= 65536, "Oversized version metadata")
        version = json.loads(jar.read("version.json"))
        require(version.get("id") == "1.21.1" and version.get("protocol_version") == 767,
                "Expected Minecraft 1.21.1 protocol 767")
        versions = bundle_list(jar, "versions")
        require(len(versions) == 1 and versions[0][1] == "1.21.1"
                and versions[0][0] == INNER_SHA256, "Unexpected inner server reference")
        entries = [("versions", item) for item in versions]
        entries += [("libraries", item) for item in bundle_list(jar, "libraries")]
        inner = None
        for kind, (expected, _identifier, relative) in entries:
            name = "META-INF/" + kind + "/" + str(relative)
            info = jar.getinfo(name)
            total += info.file_size
            require(0 < info.file_size <= MAX_FILE and total <= MAX_EXTRACTED,
                    "Bundle extraction resource bound exceeded")
            destination = output / "reference" / kind / Path(*relative.parts)
            require(destination.resolve().is_relative_to(output.resolve()), "Extracted path escaped output")
            destination.parent.mkdir(parents=True, exist_ok=True)
            digest, copied = hashlib.sha256(), 0
            with jar.open(info) as source, destination.open("xb") as target:
                while data := source.read(1024 * 1024):
                    copied += len(data)
                    require(copied <= info.file_size, "Archive length changed during extraction")
                    digest.update(data)
                    target.write(data)
            require(copied == info.file_size and digest.hexdigest() == expected,
                    "Extracted reference digest mismatch")
            if kind == "versions": inner = destination
            else: libraries.append(destination)
    return inner, libraries


def validate_report(path, catalog_path):
    require(path.stat().st_size <= 32 * 1024 * 1024, "Oversized shape report")
    report = json.loads(path.read_text(encoding="utf8"))
    require(report.get("schema_version") == 1 and report.get("minecraft_version") == "1.21.1",
            "Unexpected generated shape schema")
    require(report.get("block_catalog_sha256") == CATALOG_SHA256
            and report.get("official_server_sha256") == INNER_SHA256
            and report.get("official_server_mappings_sha256") == MAPPINGS_SHA256,
            "Generated shape provenance mismatch")
    catalog = json.loads(catalog_path.read_text(encoding="utf8"))
    expected = {s["id"]: (name, s.get("properties", {}))
                for name, block in catalog.items() for s in block["states"]}
    states = report.get("states")
    require(type(states) is list and len(states) == len(expected) == 26684,
            "Generated state domain mismatch")
    known = {"collision": 0, "outline": 0}
    for index, state in enumerate(states):
        require(state.get("id") == index and (state.get("name"), state.get("properties")) == expected[index],
                "Generated state identity differs from exact catalog")
        for key in known:
            require(key in state and key + "_unresolved" in state, "Missing explicit shape field")
            boxes, reason = state[key], state[key + "_unresolved"]
            if boxes is None:
                require(type(reason) is str and (reason in ("dynamic shape", "position-dependent offset")
                        or reason.startswith("requires world lookup ")
                        or reason.startswith("requires player collision context ")),
                        "Unexpected reference extraction failure")
            else:
                require(reason is None and type(boxes) is list and len(boxes) <= 4096,
                        "Invalid resolved shape metadata")
                for box in boxes:
                    require(type(box) is list and len(box) == 6
                            and all(type(v) in (int, float) and math.isfinite(v) for v in box)
                            and all(box[i] < box[i+3] for i in range(3)), "Invalid shape AABB")
                known[key] += 1
    require(known == {"collision": 26473, "outline": 26379}
            and report.get("collision_resolved") == known["collision"]
            and report.get("outline_resolved") == known["outline"],
            "Resolution counts differ from the tested vanilla reference")
    return known


def run(args):
    archive = checked_input(args.server_jar, SERVER_SHA256, "Bundled server JAR", MAX_FILE)
    mappings = checked_input(args.mappings, MAPPINGS_SHA256, "Server mappings", 16 * 1024 * 1024)
    catalog = checked_input(args.blocks, CATALOG_SHA256, "Generated blocks.json", 32 * 1024 * 1024)
    source = Path(__file__).with_name("ShapeReference.java").resolve(strict=True)
    output = args.output.resolve()
    require(not args.output.is_symlink() and not output.exists(), "Output directory must be new")
    # No deletion or cleanup of user paths. Failed runs retain their fresh folder/log.
    output.mkdir(parents=True, exist_ok=False)
    inner, libraries = extract_reference(archive, output)
    classes = output / "classes"
    classes.mkdir()
    classpath = os.pathsep.join(str(p) for p in [inner, *libraries])
    flags = getattr(subprocess, "CREATE_NO_WINDOW", 0)
    with (output / "generator.log").open("xb") as log:
        subprocess.run([args.javac, "-proc:none", "-cp", classpath, "-d", str(classes), str(source)],
                       cwd=output, stdin=subprocess.DEVNULL, stdout=log, stderr=subprocess.STDOUT,
                       timeout=90, check=True, creationflags=flags)
        subprocess.run([args.java, "-Xmx1G", "-cp", str(classes) + os.pathsep + classpath,
                        "leafishreference.ShapeReference", str(mappings), str(catalog), str(inner),
                        str(output / "shapes-v1.json")], cwd=output, stdin=subprocess.DEVNULL,
                       stdout=log, stderr=subprocess.STDOUT, timeout=120, check=True, creationflags=flags)
    counts = validate_report(output / "shapes-v1.json", catalog)
    for path, expected, label in [(archive, SERVER_SHA256, "Server JAR"),
                                  (mappings, MAPPINGS_SHA256, "Mappings"),
                                  (catalog, CATALOG_SHA256, "Catalog")]:
        require(sha256(path) == expected, label + " changed during generation")
    summary = {"success": True, "schema_version": 1, "minecraft_version": "1.21.1", "protocol": 767,
               "state_count": 26684, "collision_resolved": counts["collision"], "outline_resolved": counts["outline"],
               "bundled_server_sha256": SERVER_SHA256, "inner_server_sha256": INNER_SHA256,
               "server_mappings_sha256": MAPPINGS_SHA256, "block_catalog_sha256": CATALOG_SHA256,
               "shape_report_sha256": sha256(output / "shapes-v1.json"),
               "java_generator_sha256": sha256(source), "python_helper_sha256": sha256(Path(__file__)),
               "source_inputs_unchanged": True, "game_server_started": False, "world_loaded": False,
               "generated_data_published": False}
    (output / "summary.json").write_text(json.dumps(summary, indent=2) + "\n", encoding="utf8")
    return summary


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--server-jar", type=Path, required=True, help="Original bundled vanilla 1.21.1 server.jar")
    parser.add_argument("--mappings", type=Path, required=True, help="Original official 1.21.1 server mappings")
    parser.add_argument("--blocks", type=Path, required=True, help="Unmodified blocks.json from generate_vanilla_reference.py")
    parser.add_argument("--output", type=Path, required=True, help="New local output directory, preferably outside checkout")
    parser.add_argument("--java", default="java", help="Java 21 executable or command on PATH")
    parser.add_argument("--javac", default="javac", help="Java 21 compiler executable or command on PATH")
    args = parser.parse_args(argv)
    try:
        result = run(args)
    except (ValueError, OSError, subprocess.SubprocessError, zipfile.BadZipFile, KeyError) as error:
        print(json.dumps({"success": False, "error": str(error)}), file=sys.stderr)
        return 1
    print(json.dumps(result, indent=2))
    return 0


if __name__ == "__main__":
    sys.exit(main())
