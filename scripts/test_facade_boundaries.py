#!/usr/bin/env python3
"""Regression tests for the root-subsystem facade checker."""

from __future__ import annotations

import subprocess
import tempfile
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parent.parent
CHECKER = ROOT / "scripts/check_facade_boundaries.py"


class FacadeRepository:
    def __init__(self) -> None:
        self.temporary = tempfile.TemporaryDirectory()
        self.root = Path(self.temporary.name)
        for facade in ("server", "cli", "local"):
            directory = self.root / "src" / facade
            directory.mkdir(parents=True)
            (directory / "AGENTS.md").write_text(f"# {facade} capsule\n")
            (directory / "mod.rs").write_text("mod implementation;\npub use implementation::Api;\n")

    def close(self) -> None:
        self.temporary.cleanup()

    def run(self) -> subprocess.CompletedProcess[str]:
        return subprocess.run(
            ["python3", str(CHECKER), "--root", str(self.root)],
            check=False,
            capture_output=True,
            text=True,
        )


class FacadeBoundaryTests(unittest.TestCase):
    def setUp(self) -> None:
        self.repo = FacadeRepository()

    def tearDown(self) -> None:
        self.repo.close()

    def test_accepts_private_children_with_flat_reexports(self) -> None:
        (self.repo.root / "src/server/mod.rs").write_text(
            "mod handlers;\npub(crate) mod schedule_delivery {}\npub use handlers::create_router;\n"
        )
        result = self.repo.run()
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)

    def test_rejects_public_child_module(self) -> None:
        (self.repo.root / "src/server/mod.rs").write_text(
            "pub mod handlers;\npub use handlers::create_router;\n"
        )
        result = self.repo.run()
        self.assertEqual(result.returncode, 1)
        self.assertIn("public child module `handlers`", result.stderr)

    def test_rejects_missing_capsule_or_flat_surface(self) -> None:
        (self.repo.root / "src/cli/AGENTS.md").unlink()
        (self.repo.root / "src/local/mod.rs").write_text("mod generator;\n")
        result = self.repo.run()
        self.assertEqual(result.returncode, 1)
        self.assertIn("src/cli/AGENTS.md: missing facade capsule", result.stderr)
        self.assertIn("src/local/mod.rs: facade has no explicit flat re-exports", result.stderr)


if __name__ == "__main__":
    unittest.main()
