#!/usr/bin/env python3
"""Synthetic state images only; never opens a real VM state disk."""

from pathlib import Path
import struct
import subprocess
import sys
import tempfile
import unittest


EXPORTER = Path(__file__).with_name("export-miraos-source.py")


def checksum(units):
    result = 0x14650FB0739D0383
    for unit in units:
        result = ((result ^ unit) * 1099511628211) & ((1 << 64) - 1)
    return result


def image():
    result = bytearray(2 * 1024 * 1024)
    struct.pack_into("<QQ", result, 0, 0x324C4F564152494D, 2)
    return result


def save(result, bank, payload, generation, version=3):
    header = [0] * 64
    header[:11] = [0x334352534152494D, version, len(payload), generation,
                   checksum(payload), len(payload), 7, 10, 20, 30, 40]
    header[11] = checksum(header[:11])
    offset = bank * 512
    struct.pack_into("<64Q", result, offset, *header)
    result[offset + 512:offset + 512 + len(payload)] = payload


class ExportTests(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        self.disk = Path(self.directory.name) / "state.img"
        self.output = Path(self.directory.name) / "draft.mira"

    def export(self, content, output=None):
        self.disk.write_bytes(content)
        before = self.disk.read_bytes()
        result = subprocess.run([sys.executable, str(EXPORTER), str(self.disk),
                                 str(output or self.output)], capture_output=True, text=True)
        self.assertEqual(self.disk.read_bytes(), before)
        return result

    def test_newest_exact_bytes_and_report(self):
        content = image()
        save(content, 1283, b"old", 1)
        payload = b"line\r\n\x00\xff\n"
        save(content, 2308, payload, 2)
        self.output.write_bytes(b"previous export")
        result = self.export(content)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(self.output.read_bytes(), payload)
        self.assertIn("generation 2, cursor 9, revision 7", result.stdout)

    def test_corrupt_newest_payload_falls_back(self):
        content = image()
        save(content, 1283, b"older valid", 3)
        save(content, 2308, b"newer invalid", 4)
        content[2309 * 512] ^= 1
        result = self.export(content)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(self.output.read_bytes(), b"older valid")
        self.assertIn("bank 1283, generation 3", result.stdout)

    def test_full_512_kib_payload_in_second_bank(self):
        content = image()
        save(content, 1283, b"older", 1)
        payload = bytes(range(256)) * 2048
        save(content, 2308, payload, 2)
        result = self.export(content)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(self.output.read_bytes(), payload)
        self.assertIn("524288 bytes", result.stdout)
        self.assertIn("bank 2308, generation 2", result.stdout)

    def test_oversized_payload_is_rejected(self):
        content = image()
        save(content, 2308, b"x" * (512 * 1024 + 1), 1)
        self.output.write_bytes(b"keep")
        result = self.export(content)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("no committed source", result.stderr)
        self.assertEqual(self.output.read_bytes(), b"keep")

    def test_old_volume_and_source_formats_are_not_migrated(self):
        old_disk = bytearray(512 * 1024)
        struct.pack_into("<QQ", old_disk, 0, 0x314C4F564152494D, 1)
        padded_old_disk = old_disk + bytearray(1536 * 1024)
        old_version = image()
        struct.pack_into("<Q", old_version, 8, 1)
        old_source = image()
        save(old_source, 1283, b"legacy source", 1, version=1)
        struct.pack_into("<Q", old_source, 1283 * 512, 0x314352534152494D)
        for content, message in [(old_disk, "exactly 2 MiB"),
                                 (padded_old_disk, "foreign disk"),
                                 (old_version, "unsupported MIRAVOL2 version 1"),
                                 (old_source, "no committed source")]:
            with self.subTest(message=message):
                self.output.write_bytes(b"keep")
                result = self.export(content)
                self.assertNotEqual(result.returncode, 0)
                self.assertIn(message, result.stderr)
                self.assertEqual(self.output.read_bytes(), b"keep")

    def test_foreign_and_no_save_leave_output_unchanged(self):
        for content, message in [(bytearray(2 * 1024 * 1024), "foreign disk"),
                                 (image(), "no committed source")]:
            with self.subTest(message=message):
                self.output.write_bytes(b"keep")
                result = self.export(content)
                self.assertNotEqual(result.returncode, 0)
                self.assertIn(message, result.stderr)
                self.assertEqual(self.output.read_bytes(), b"keep")

    def test_unsupported_source_format(self):
        content = image()
        save(content, 1283, b"future", 1, version=3)
        result = self.export(content)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("unsupported MIRASRC2 version 3", result.stderr)
        self.assertFalse(self.output.exists())

    def test_cannot_replace_input_or_hardlink(self):
        content = image()
        save(content, 1283, b"valid", 1)
        self.disk.write_bytes(content)
        self.output.hardlink_to(self.disk)
        for output in [self.disk, self.output]:
            with self.subTest(output=output):
                result = self.export(content, output)
                self.assertNotEqual(result.returncode, 0)
                self.assertIn("state disk itself", result.stderr)


if __name__ == "__main__":
    unittest.main()
