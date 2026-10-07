"""Generate local 1.21.1 reference reports using a user-provided server archive.

Requires Python 3.11+ and Java 21. This invokes Minecraft's data generator, not
the game server. No archive, reports, worlds or credentials are downloaded or
published. The output directory must be new, so extraction and report cleanup
cannot touch an existing installation. Generated reports stay local.
"""
from __future__ import annotations

import argparse
import hashlib
import json
from pathlib import Path
import subprocess
import zipfile


def sha256(path: Path) -> str:
    with path.open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--server-jar", required=True, type=Path)
    parser.add_argument("--java", default="java")
    parser.add_argument("--output", required=True, type=Path)
    args = parser.parse_args()
    archive = args.server_jar.resolve(strict=True)
    with zipfile.ZipFile(archive) as jar:
        version = json.loads(jar.read("version.json"))
    if version.get("id") != "1.21.1" or version.get("protocol_version") != 767:
        raise ValueError("Expected an official Minecraft 1.21.1 / protocol 767 server archive")
    before_hash = sha256(archive)
    output = args.output.resolve()
    # Refuse even an empty existing directory: this is a fresh extraction only.
    output.mkdir(parents=True, exist_ok=False)
    with (output / "generator.log").open("wb") as log:
        subprocess.run(
            [args.java, "-DbundlerMainClass=net.minecraft.data.Main", "-Xmx1024M",
             "-jar", str(archive), "--reports", "--output", "generated"],
            cwd=output, stdin=subprocess.DEVNULL, stdout=log,
            stderr=subprocess.STDOUT, timeout=180, check=True,
            creationflags=getattr(subprocess, "CREATE_NO_WINDOW", 0),
        )
    reports = output / "generated/reports"
    blocks = json.loads((reports / "blocks.json").read_text(encoding="utf8"))
    states = sorted(state["id"] for block in blocks.values() for state in block["states"])
    if not states or states != list(range(len(states))):
        raise ValueError("Reference block-state IDs are not a contiguous zero-based catalog")
    packets = json.loads((reports / "packets.json").read_text(encoding="utf8"))
    expected_play_ids = {
        "clientbound": {
            "minecraft:login": 0x2B, "minecraft:player_position": 0x40,
            "minecraft:keep_alive": 0x26, "minecraft:level_chunk_with_light": 0x27,
            "minecraft:light_update": 0x2A, "minecraft:chunk_batch_start": 0x0D,
            "minecraft:chunk_batch_finished": 0x0C,
        },
        "serverbound": {
            "minecraft:accept_teleportation": 0x00, "minecraft:move_player_pos_rot": 0x1B,
            "minecraft:keep_alive": 0x18, "minecraft:chunk_batch_received": 0x08,
        },
    }
    for direction, values in expected_play_ids.items():
        for name, expected in values.items():
            actual = packets["play"][direction][name]["protocol_id"]
            if actual != expected:
                raise ValueError(f"Reference packet mismatch: {direction}/{name}: {actual}")
    if sha256(archive) != before_hash:
        raise ValueError("Source archive changed during reference generation")
    summary = {
        "minecraft_version": "1.21.1", "protocol_version": 767,
        "generator": "Minecraft server archive net.minecraft.data.Main --reports",
        "server_jar_sha256": before_hash,
        "block_count": len(blocks), "block_state_count": len(states),
        "block_states_contiguous": True, "play_packet_ids": expected_play_ids,
        "reports_sha256": {name: sha256(reports / name)
                           for name in ("blocks.json", "registries.json", "packets.json")},
        "source_archive_unchanged": True,
    }
    (output / "summary.json").write_text(json.dumps(summary, indent=2) + "\n", encoding="utf8")
    print(json.dumps(summary, indent=2))


if __name__ == "__main__":
    main()
