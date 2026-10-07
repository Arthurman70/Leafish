#!/usr/bin/env python3
"""Compare a protocol-767 probe with a stopped vanilla 1.21.1 saved world.

This independent, standard-library oracle reads only Overworld region files,
the existing session lock, and a caller-supplied generated blocks.json report.
It neither starts Minecraft nor writes a world. Its intentionally narrow format
support is not a general Anvil importer or evidence of mod compatibility.
"""

from __future__ import annotations

import argparse
from contextlib import contextmanager
import hashlib
import json
import os
from pathlib import Path
import re
import struct
import sys
import zlib


MAX_JSON = 32 * 1024 * 1024
MAX_NBT = 8 * 1024 * 1024
MAX_CHUNKS = 256
MAX_NODES = 262144
RESOURCE = re.compile(r"[a-z0-9_.-]+:[a-z0-9_./-]+\Z")
SHA1 = re.compile(r"[0-9a-f]{40}\Z")


class VerificationError(ValueError):
    """Missing, malformed, unsupported, or unequal reference evidence."""


def require(condition, message):
    if not condition:
        raise VerificationError(message)


def integer(value, lo, hi, label):
    require(type(value) is int and lo <= value <= hi, "invalid " + label)
    return value


def unique_object(pairs):
    result = {}
    for key, value in pairs:
        require(key not in result, "duplicate JSON key")
        result[key] = value
    return result


def read_json(path, limit=MAX_JSON):
    with Path(path).open("rb") as stream:
        raw = stream.read(limit + 1)
    require(len(raw) <= limit, "JSON input exceeds size bound")
    try:
        value = json.loads(raw, object_pairs_hook=unique_object)
    except (ValueError, UnicodeError, RecursionError) as error:
        raise VerificationError("invalid bounded JSON input") from error
    return value, hashlib.sha256(raw).hexdigest()


class Tag:
    __slots__ = ("kind", "value")

    def __init__(self, kind, value):
        self.kind, self.value = kind, value


class NbtReader:
    """Typed big-endian disk NBT, including Java modified UTF-8 strings."""

    def __init__(self, data):
        require(len(data) <= MAX_NBT, "NBT exceeds size bound")
        self.data, self.pos, self.nodes = memoryview(data), 0, 0

    def take(self, size):
        require(0 <= size <= len(self.data) - self.pos, "truncated NBT")
        start = self.pos
        self.pos += size
        return self.data[start:self.pos]

    def number(self, fmt):
        return struct.unpack(fmt, self.take(struct.calcsize(fmt)))[0]

    def string(self):
        raw = self.take(self.number(">H"))
        units = bytearray()
        index = 0
        while index < len(raw):
            first = raw[index]
            index += 1
            if first < 0x80:
                code = first
            elif first & 0xE0 == 0xC0:
                require(index < len(raw) and raw[index] & 0xC0 == 0x80,
                        "invalid NBT modified UTF-8")
                code = ((first & 31) << 6) | (raw[index] & 63)
                index += 1
            elif first & 0xF0 == 0xE0:
                require(index + 1 < len(raw) and raw[index] & 0xC0 == 0x80
                        and raw[index + 1] & 0xC0 == 0x80,
                        "invalid NBT modified UTF-8")
                code = ((first & 15) << 12) | ((raw[index] & 63) << 6) | (raw[index + 1] & 63)
                index += 2
            else:
                raise VerificationError("invalid NBT modified UTF-8")
            units.extend(struct.pack(">H", code))
        return units.decode("utf-16-be", errors="surrogatepass")

    def payload(self, kind, depth=0):
        self.nodes += 1
        require(self.nodes <= MAX_NODES and depth <= 64, "NBT structural bound exceeded")
        formats = {1: ">b", 2: ">h", 3: ">i", 4: ">q", 5: ">f", 6: ">d"}
        if kind in formats:
            value = self.number(formats[kind])
        elif kind in (7, 11, 12):
            count = self.number(">i")
            require(0 <= count <= MAX_NBT, "invalid NBT array length")
            width = {7: 1, 11: 4, 12: 8}[kind]
            value = (count, self.take(count * width))
        elif kind == 8:
            value = self.string()
        elif kind == 9:
            subtype = self.number(">B")
            count = self.number(">i")
            require(0 <= count <= MAX_NODES - self.nodes, "invalid NBT list length")
            require(0 <= subtype <= 12 and (subtype != 0 or count == 0), "invalid NBT list type")
            value = (subtype, [self.payload(subtype, depth + 1) for _ in range(count)])
        elif kind == 10:
            value = {}
            while True:
                subtype = self.number(">B")
                if subtype == 0:
                    break
                require(1 <= subtype <= 12, "invalid NBT compound tag")
                name = self.string()
                require(name not in value, "duplicate NBT compound key")
                value[name] = self.payload(subtype, depth + 1)
        else:
            raise VerificationError("invalid NBT tag type")
        return Tag(kind, value)

    def root(self):
        require(self.number(">B") == 10, "disk NBT root must be a compound")
        self.string()
        result = self.payload(10)
        require(self.pos == len(self.data), "trailing disk NBT bytes")
        return result.value


def field(compound, name, kind):
    require(type(compound) is dict and name in compound, "missing NBT field: " + name)
    tag = compound[name]
    require(tag.kind == kind, "wrong NBT field type: " + name)
    return tag.value


def resource(name):
    require(type(name) is str and len(name) <= 512 and RESOURCE.fullmatch(name),
            "invalid registry resource name")
    return name


def properties(value):
    require(type(value) is dict and len(value) <= 64, "invalid block state properties")
    for key, item in value.items():
        require(type(key) is str and type(item) is str and len(key) <= 128 and len(item) <= 128,
                "invalid block property string")
    return tuple(sorted(value.items()))


def block_catalog(path):
    catalog, digest = read_json(path)
    require(type(catalog) is dict and 0 < len(catalog) <= 8192, "invalid block catalog")
    lookup, ids = {}, set()
    for name, block in catalog.items():
        resource(name)
        require(type(block) is dict, "invalid block descriptor")
        states = block.get("states")
        require(type(states) is list and 0 < len(states) <= 65536, "invalid catalog state list")
        for state in states:
            require(type(state) is dict, "invalid catalog state")
            state_id = integer(state.get("id"), 0, 131071, "catalog state ID")
            key = (name, properties(state.get("properties", {})))
            require(key not in lookup and state_id not in ids, "duplicate catalog state or ID")
            lookup[key] = state_id
            ids.add(state_id)
            require(len(ids) <= 131072, "catalog state limit exceeded")
    require(ids == set(range(len(ids))), "block catalog IDs are not contiguous from zero")
    return lookup, digest


def safe_file(world, relative):
    path = world
    for component in Path(relative).parts:
        require(component not in ("..", ".") and not Path(component).is_absolute(), "unsafe reference path")
        path = path / component
        require(not path.is_symlink(), "linked world reference path is unsupported")
    require(path.resolve().is_relative_to(world), "world reference path escaped root")
    require(path.is_file(), "required reference file is missing")
    return path


@contextmanager
def stopped_world_lock(world):
    """Read-lock the existing Java session.lock; never create or change it."""
    path = safe_file(world, "session.lock")
    with path.open("rb") as stream:
        require(os.fstat(stream.fileno()).st_size >= 1, "empty world session lock")
        try:
            if os.name == "nt":
                import msvcrt
                msvcrt.locking(stream.fileno(), msvcrt.LK_NBRLCK, 1)
            else:
                import fcntl
                fcntl.lockf(stream.fileno(), fcntl.LOCK_SH | fcntl.LOCK_NB, 0, 0, os.SEEK_SET)
        except OSError as error:
            raise VerificationError("world is locked; stop the reference server before verification") from error
        try:
            yield
        finally:
            stream.seek(0)
            if os.name == "nt":
                msvcrt.locking(stream.fileno(), msvcrt.LK_UNLCK, 1)
            else:
                fcntl.lockf(stream.fileno(), fcntl.LOCK_UN, 0, 0, os.SEEK_SET)


def identity(stat):
    return (stat.st_dev, stat.st_ino, stat.st_size, stat.st_mtime_ns)


class Region:
    def __init__(self, path):
        self.path = path
        self.stream = path.open("rb")
        try:
            self.before = identity(os.fstat(self.stream.fileno()))
            size = self.before[2]
            require(8192 <= size <= 512 * 1024 * 1024 and size % 4096 == 0,
                    "invalid bounded region size")
            header = self.stream.read(8192)
            self.locations, extents = [], []
            for index in range(1024):
                packed = struct.unpack_from(">I", header, index * 4)[0]
                offset, sectors = packed >> 8, packed & 255
                require((offset == 0) == (sectors == 0), "invalid empty region location")
                if offset:
                    require(offset >= 2 and (offset + sectors) * 4096 <= size,
                            "region location is outside its file")
                    extents.append((offset, offset + sectors))
                self.locations.append((offset, sectors))
            extents.sort()
            require(all(a[1] <= b[0] for a, b in zip(extents, extents[1:])),
                    "overlapping region allocations")
        except BaseException:
            self.stream.close()
            raise

    def read(self, x, z):
        offset, sectors = self.locations[(x % 32) + (z % 32) * 32]
        require(offset != 0, "requested chunk is absent from saved region")
        self.stream.seek(offset * 4096)
        prefix = self.stream.read(5)
        require(len(prefix) == 5, "truncated region chunk prefix")
        length = struct.unpack_from(">I", prefix)[0]
        require(2 <= length <= sectors * 4096 - 4, "invalid region chunk length")
        require(prefix[4] == 2, "reference oracle requires internal zlib region chunks")
        compressed = self.stream.read(length - 1)
        require(len(compressed) == length - 1, "truncated compressed chunk")
        decoder = zlib.decompressobj()
        try:
            data = decoder.decompress(compressed, MAX_NBT + 1)
        except zlib.error as error:
            raise VerificationError("invalid zlib chunk") from error
        require(len(data) <= MAX_NBT and decoder.eof and not decoder.unconsumed_tail
                and not decoder.unused_data, "oversized, incomplete, or trailing zlib chunk")
        return NbtReader(data).root()

    def unchanged(self):
        require(identity(os.fstat(self.stream.fileno())) == self.before
                and identity(self.path.stat()) == self.before,
                "saved region changed during verification")

    def close(self):
        self.stream.close()


def saved_palette(compound, entries, block_ids, biome_ids, is_blocks):
    require(set(compound).issubset({"palette", "data"}), "unexpected saved palette fields")
    subtype, palette = field(compound, "palette", 9)
    require(subtype == (10 if is_blocks else 8) and 1 <= len(palette) <= entries,
            "invalid saved palette type or length")
    mapped, unique = [], set()
    for item in palette:
        if is_blocks:
            require(set(item.value).issubset({"Name", "Properties"}), "unexpected block palette fields")
            name = resource(field(item.value, "Name", 8))
            props = {}
            if "Properties" in item.value:
                raw_props = field(item.value, "Properties", 10)
                for key, value in raw_props.items():
                    require(value.kind == 8, "block palette property is not a string")
                    props[key] = value.value
            key = (name, properties(props))
            require(key in block_ids, "saved block state has no exact generated catalog match")
            state_id = block_ids[key]
        else:
            key = resource(item.value)
            require(key in biome_ids, "saved biome has no negotiated registry ID")
            state_id = biome_ids[key]
        require(key not in unique, "duplicate saved palette value")
        unique.add(key)
        mapped.append(state_id)
    if len(mapped) == 1:
        if "data" in compound:
            require(field(compound, "data", 12)[0] == 0, "singleton palette has unexpected packed data")
        return struct.pack("<I", mapped[0]) * entries
    bits = max(4 if is_blocks else 1, (len(mapped) - 1).bit_length())
    per_long = 64 // bits
    count, raw = field(compound, "data", 12)
    require(count == (entries + per_long - 1) // per_long, "wrong saved packed-long count")
    mask, output, position = (1 << bits) - 1, bytearray(entries * 4), 0
    for index in range(count):
        value = struct.unpack_from(">Q", raw, index * 8)[0]
        used = min(per_long, entries - position)
        for slot in range(used):
            palette_index = (value >> (slot * bits)) & mask
            require(palette_index < len(mapped), "saved packed palette index is out of bounds")
            struct.pack_into("<I", output, position * 4, mapped[palette_index])
            position += 1
        require(value >> (used * bits) == 0, "nonzero saved palette padding")
    return output


def verify(report, worldPath, blocksPath):
    """Return an audit dictionary or raise VerificationError; never modify inputs."""
    require(type(report) is dict and report.get("protocol") == 767, "requires protocol 767 probe report")
    require(report.get("configuration_complete") is True and report.get("play_conformance_complete") is True,
            "probe did not complete configuration and Play conformance")
    require(report.get("production_connected") is False, "requires isolated reference probe report")
    dimension = report.get("joined_dimension")
    require(type(dimension) is dict and dimension.get("name") == "minecraft:overworld",
            "reference oracle supports only Overworld region files")
    min_y = integer(dimension.get("min_y"), -2048, 2032, "dimension minimum Y")
    height = integer(dimension.get("height"), 16, 4096, "dimension height")
    require(min_y % 16 == 0 and height % 16 == 0, "dimension is not section aligned")
    names = report.get("biome_registry_names")
    require(type(names) is list and 0 < len(names) <= 65536, "invalid biome registry name list")
    biome_ids = {}
    for index, name in enumerate(names):
        resource(name)
        require(name not in biome_ids, "duplicate negotiated biome name")
        biome_ids[name] = index
    chunks = report.get("chunk_fingerprints")
    require(type(chunks) is list and 0 < len(chunks) <= MAX_CHUNKS, "invalid bounded chunk fingerprint list")
    block_ids, catalog_sha256 = block_catalog(blocksPath)
    require(integer(report.get("block_state_count"), 1, 131072, "probe block state count") == len(block_ids),
            "probe block state count differs from generated catalog")
    require(integer(report.get("retained_chunks"), 1, MAX_CHUNKS, "retained chunk count") == len(chunks),
            "retained chunk count differs from fingerprints")
    seen, total_sections = set(), 0
    for chunk in chunks:
        require(type(chunk) is dict, "invalid chunk fingerprint")
        x = integer(chunk.get("x"), -1875000, 1875000, "chunk X")
        z = integer(chunk.get("z"), -1875000, 1875000, "chunk Z")
        require((x, z) not in seen, "duplicate chunk fingerprint coordinate")
        seen.add((x, z))
        start = integer(chunk.get("min_section_y"), -128, 127, "minimum section Y")
        count = integer(chunk.get("section_count"), 1, 256, "section count")
        require(start == min_y // 16 and count == height // 16 and start + count - 1 <= 127,
                "chunk section range differs from dimension")
        require(integer(chunk.get("block_state_values"), 1, 1048576, "block value count") == count * 4096
                and integer(chunk.get("biome_values"), 1, 16384, "biome value count") == count * 64,
                "incorrect fingerprint numeric value count")
        for key in ("block_states_sha1", "biomes_sha1"):
            require(type(chunk.get(key)) is str and SHA1.fullmatch(chunk[key]), "invalid fingerprint SHA-1")
        total_sections += count
    require(report.get("decoded_sections") == total_sections
            and report.get("preserved_numeric_block_states") == total_sections * 4096,
            "probe aggregate section or block value count mismatch")
    world = Path(worldPath).resolve(strict=True)
    require(world.is_dir(), "world reference must be a directory")
    regions, results, boundary_light_sections = {}, [], 0
    with stopped_world_lock(world):
        try:
            for expected in sorted(chunks, key=lambda item: (item["x"], item["z"])):
                x, z = expected["x"], expected["z"]
                region_key = (x // 32, z // 32)
                if region_key not in regions:
                    path = safe_file(world, "region/r.%d.%d.mca" % region_key)
                    regions[region_key] = Region(path)
                root = regions[region_key].read(x, z)
                require(field(root, "DataVersion", 3) == 3955, "reference chunk is not Minecraft 1.21.1 data version")
                require(field(root, "xPos", 3) == x and field(root, "zPos", 3) == z,
                        "saved chunk coordinates differ from region location")
                require(field(root, "Status", 8) == "minecraft:full", "saved reference chunk is not full")
                subtype, raw_sections = field(root, "sections", 9)
                require(subtype == 10 and len(raw_sections) <= 256, "invalid saved section list")
                sections = {}
                for raw_section in raw_sections:
                    y = field(raw_section.value, "Y", 1)
                    require(y not in sections, "duplicate saved section Y")
                    sections[y] = raw_section.value
                start, count = expected["min_section_y"], expected["section_count"]
                terrain_ys = set(range(start, start + count))
                require(terrain_ys.issubset(sections), "missing saved terrain sections")
                # ChunkSerializer also saves light layers one section beyond
                # either terrain boundary. These have no block or biome values.
                for y in set(sections) - terrain_ys:
                    light_section = sections[y]
                    require(y in (start - 1, start + count)
                            and set(light_section).issubset({"Y", "SkyLight", "BlockLight"})
                            and len(light_section) >= 2, "extra saved non-light section")
                    for layer in set(light_section) - {"Y"}:
                        require(field(light_section, layer, 7)[0] == 2048,
                                "invalid boundary light array length")
                    boundary_light_sections += 1
                blocks_hash, biomes_hash = hashlib.sha1(), hashlib.sha1()
                for y in range(start, start + count):
                    section = sections[y]
                    blocks_hash.update(saved_palette(field(section, "block_states", 10), 4096, block_ids, biome_ids, True))
                    biomes_hash.update(saved_palette(field(section, "biomes", 10), 64, block_ids, biome_ids, False))
                actual_blocks, actual_biomes = blocks_hash.hexdigest(), biomes_hash.hexdigest()
                require(actual_blocks == expected["block_states_sha1"], "block state fingerprint mismatch at chunk (%d,%d)" % (x, z))
                require(actual_biomes == expected["biomes_sha1"], "biome fingerprint mismatch at chunk (%d,%d)" % (x, z))
                results.append({"x": x, "z": z, "min_section_y": start, "section_count": count,
                                "block_state_values": count * 4096, "biome_values": count * 64,
                                "block_states_sha1": actual_blocks, "biomes_sha1": actual_biomes})
            for region in regions.values():
                region.unchanged()
        finally:
            for region in regions.values():
                region.close()
    return {"success": True, "scope": "Independent saved Overworld block/biome comparison; vanilla 1.21.1 reference only",
            "protocol": 767, "data_version": 3955, "chunks_verified": len(results),
            "sections_verified": total_sections, "block_state_values_verified": total_sections * 4096,
            "biome_values_verified": total_sections * 64, "block_state_catalog_count": len(block_ids),
            "block_catalog_sha256": catalog_sha256, "biome_registry_count": len(biome_ids),
            "region_files_read": len(regions), "region_metadata_unchanged": True,
            "boundary_light_only_sections_validated": boundary_light_sections,
            "stopped_world_session_lock_held": True, "world_files_written": False,
            "missing_or_unknown_state_substitutions": 0, "hash_encoding": "SHA-1 of unsigned u32 little endian; section Y ascending, then local X fastest, Z, Y",
            "chunk_fingerprints": results}


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--report", required=True, type=Path)
    parser.add_argument("--world", required=True, type=Path)
    parser.add_argument("--blocks", required=True, type=Path)
    args = parser.parse_args(argv)
    try:
        report, _ = read_json(args.report, 4 * 1024 * 1024)
        if type(report) is dict and "probe_result" in report:
            report = report["probe_result"]
        result = verify(report, args.world, args.blocks)
    except (VerificationError, OSError, ValueError, OverflowError, RecursionError) as error:
        # OS error details may contain private paths; do not echo them in the audit.
        message = str(error) if isinstance(error, VerificationError) else "reference input could not be read or decoded"
        print(json.dumps({"success": False, "error": message}))
        return 1
    print(json.dumps(result, sort_keys=True))
    return 0


if __name__ == "__main__":
    sys.exit(main())
