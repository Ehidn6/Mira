#!/usr/bin/env python3
"""Export a committed source draft from a MIRAVOL2 v2 QEMU state image."""

import argparse
from dataclasses import dataclass
import os
from pathlib import Path
import stat
import struct
import sys
import tempfile


DISK_BYTES = 2 * 1024 * 1024
SOURCE_BYTES = 512 * 1024
SECTOR_BYTES = 512
SOURCE_BANKS = (1283, 2308)
SOURCE_MAGIC = 0x334352534152494D
HASH_SEED = 0x14650FB0739D0383
HASH_PRIME = 1099511628211
MASK64 = (1 << 64) - 1


class ExportError(Exception):
    pass


@dataclass(frozen=True)
class Source:
    payload: bytes
    bank: int
    generation: int
    cursor: int
    revision: int


def checksum(units):
    value = HASH_SEED
    for unit in units:
        value = ((value ^ unit) * HASH_PRIME) & MASK64
    return value


def committed_source(image):
    if len(image) != DISK_BYTES:
        raise ExportError("state image must be exactly 2 MiB")
    magic, version = struct.unpack_from("<QQ", image)
    if magic != 0x324C4F564152494D:
        raise ExportError("foreign disk: missing MIRAVOL2 volume header")
    if version != 2:
        raise ExportError(f"unsupported MIRAVOL2 version {version}")

    candidates = []
    for bank in SOURCE_BANKS:
        offset = bank * SECTOR_BYTES
        header = struct.unpack_from("<64Q", image, offset)
        if not any(header):
            continue
        if header[0] != SOURCE_MAGIC:
            continue
        if header[1] != 3:
            raise ExportError(f"unsupported MIRASRC3 version {header[1]} in bank {bank}")
        if header[11] != checksum(header[:11]):
            continue
        length, generation, digest, cursor, revision = header[2:7]
        if not (length <= SOURCE_BYTES and 0 < generation < (1 << 63)
                and cursor <= length and revision < (1 << 62)):
            continue
        payload = image[offset + SECTOR_BYTES:offset + SECTOR_BYTES + length]
        if checksum(payload) == digest:
            candidates.append(Source(payload, bank, generation, cursor, revision))
    if not candidates:
        raise ExportError("no committed source draft with valid header and payload checksums")
    # Mira chooses the first bank when both generations are equal.
    return max(candidates, key=lambda source: source.generation)


def same_file(input_path, output_path):
    return (input_path.resolve() == output_path.resolve()
            or (output_path.exists() and os.path.samefile(input_path, output_path)))


def export_source(input_path, output_path):
    if same_file(input_path, output_path):
        raise ExportError("output must not refer to the state disk itself")
    with input_path.open("rb") as disk:
        info = os.fstat(disk.fileno())
        if not stat.S_ISREG(info.st_mode):
            raise ExportError("input must be a regular QEMU state image file")
        if info.st_size != DISK_BYTES:
            raise ExportError("state image must be exactly 2 MiB")
        source = committed_source(disk.read(DISK_BYTES + 1))

    temporary = None
    try:
        with tempfile.NamedTemporaryFile(mode="wb", dir=output_path.parent,
                                         prefix=f".{output_path.name}.", delete=False) as output:
            temporary = Path(output.name)
            output.write(source.payload)
            output.flush()
            os.fsync(output.fileno())
        if same_file(input_path, output_path):
            raise ExportError("output must not refer to the state disk itself")
        os.replace(temporary, output_path)
        temporary = None
    finally:
        if temporary is not None:
            temporary.unlink(missing_ok=True)
    return source


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("disk", type=Path, help="existing 2 MiB MIRAVOL2 v2 state image")
    parser.add_argument("output", type=Path, help="source output file (atomically replaced)")
    args = parser.parse_args()
    try:
        source = export_source(args.disk, args.output)
    except (ExportError, OSError) as error:
        print(f"Source export failed: {error}", file=sys.stderr)
        return 1
    print(f"Exported {len(source.payload)} bytes to {args.output} "
          f"(bank {source.bank}, generation {source.generation}, "
          f"cursor {source.cursor}, revision {source.revision}).")
    return 0


if __name__ == "__main__":
    sys.exit(main())
