#!/usr/bin/env python3
"""Regression tests for the tracked repository hygiene guard."""

from __future__ import annotations

import hashlib
import struct
import subprocess
import tempfile
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parent.parent
CHECKER = ROOT / "scripts/check_repository_hygiene.py"
HEADER = "path\tsha256\tsize\tplatform\tlicense\tprovenance\tgeneration_reason\n"


class HygieneRepository:
    def __init__(self) -> None:
        self.temporary = tempfile.TemporaryDirectory()
        self.root = Path(self.temporary.name)
        subprocess.run(["git", "init", "-q", str(self.root)], check=True)
        self.write(".github/repository-hygiene-allowlist.tsv", HEADER.encode())
        self.track(".github/repository-hygiene-allowlist.tsv")

    def close(self) -> None:
        self.temporary.cleanup()

    def write(self, name: str, contents: bytes, executable: bool = False) -> Path:
        path = self.root / name
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_bytes(contents)
        if executable:
            path.chmod(path.stat().st_mode | 0o111)
        return path

    def track(self, *names: str) -> None:
        subprocess.run(["git", "-C", str(self.root), "add", "-f", "--", *names], check=True)

    def run(self) -> subprocess.CompletedProcess[str]:
        return subprocess.run(
            ["python3", str(CHECKER), "--root", str(self.root)],
            check=False,
            capture_output=True,
            text=True,
        )


class RepositoryHygieneTests(unittest.TestCase):
    def setUp(self) -> None:
        self.repo = HygieneRepository()

    def tearDown(self) -> None:
        self.repo.close()

    def test_rejects_tracked_mach_o_elf_and_pe(self) -> None:
        pe = bytearray(128)
        pe[:2] = b"MZ"
        struct.pack_into("<I", pe, 0x3C, 64)
        pe[64:68] = b"PE\0\0"
        self.repo.write("tmp/mach-o", bytes.fromhex("feedfacf") + bytes(32))
        self.repo.write("tmp/elf", b"\x7fELF" + bytes(32))
        self.repo.write("tmp/windows", bytes(pe))
        self.repo.track("tmp/mach-o", "tmp/elf", "tmp/windows")

        result = self.repo.run()

        self.assertEqual(result.returncode, 1, result.stdout + result.stderr)
        self.assertIn("Mach-O magic", result.stderr)
        self.assertIn("ELF magic", result.stderr)
        self.assertIn("PE magic", result.stderr)

    def test_rejects_tracked_transient_artifacts(self) -> None:
        self.repo.write("src/lib.rs.bak", b"temporary source")
        self.repo.write("debug/session.log", b"temporary log")
        self.repo.track("src/lib.rs.bak", "debug/session.log")

        result = self.repo.run()

        self.assertEqual(result.returncode, 1, result.stdout + result.stderr)
        self.assertIn("src/lib.rs.bak", result.stderr)
        self.assertIn("debug/session.log", result.stderr)

    def test_accepts_executable_text_script(self) -> None:
        self.repo.write("scripts/example.sh", b"#!/bin/sh\nprintf '%s\\n' ok\n", executable=True)
        self.repo.track("scripts/example.sh")

        result = self.repo.run()

        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)

    def test_accepts_documented_allowlisted_binary_fixture(self) -> None:
        contents = bytes.fromhex("feedfacf") + bytes(32)
        fixture = self.repo.write("tests/fixtures/arm64-header.bin", contents)
        digest = hashlib.sha256(contents).hexdigest()
        row = (
            f"tests/fixtures/arm64-header.bin\t{digest}\t{fixture.stat().st_size}\t"
            "macOS arm64\tCC0-1.0\tsynthetic test data\t"
            "prebuilt bytes required at parser boundary\n"
        )
        self.repo.write(
            ".github/repository-hygiene-allowlist.tsv", (HEADER + row).encode()
        )
        self.repo.track(
            ".github/repository-hygiene-allowlist.tsv", "tests/fixtures/arm64-header.bin"
        )

        result = self.repo.run()

        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)

    def test_rejects_allowlist_without_complete_provenance(self) -> None:
        contents = bytes.fromhex("feedfacf") + bytes(32)
        fixture = self.repo.write("tests/fixtures/undocumented.bin", contents)
        digest = hashlib.sha256(contents).hexdigest()
        row = f"tests/fixtures/undocumented.bin\t{digest}\t{fixture.stat().st_size}\tmacOS\n"
        self.repo.write(
            ".github/repository-hygiene-allowlist.tsv", (HEADER + row).encode()
        )
        self.repo.track(
            ".github/repository-hygiene-allowlist.tsv", "tests/fixtures/undocumented.bin"
        )

        result = self.repo.run()

        self.assertEqual(result.returncode, 1, result.stdout + result.stderr)
        self.assertIn("every provenance field is required", result.stderr)


class CurrentTreeHygieneTests(unittest.TestCase):
    def test_current_tracked_tree_passes(self) -> None:
        result = subprocess.run(
            ["python3", str(CHECKER), "--root", str(ROOT)],
            check=False,
            capture_output=True,
            text=True,
        )
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)


if __name__ == "__main__":
    unittest.main()
