#!/usr/bin/env python3
"""Production-boundary regressions for the root Cargo lockfile checker."""

from __future__ import annotations

import os
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parent.parent
CHECKER = ROOT / "scripts/check_root_cargo_lock.py"


class LockRepository:
    def __init__(self) -> None:
        self.temporary = tempfile.TemporaryDirectory(prefix="finch-root-lock-")
        self.root = Path(self.temporary.name)
        self.environment = os.environ.copy()
        self.environment["GIT_CONFIG_NOSYSTEM"] = "1"
        self.environment["HOME"] = str(self.root / "home")
        (self.root / "home").mkdir()
        self.git("init", "--quiet")
        (self.root / ".gitignore").write_text(
            "Cargo.lock\n!/Cargo.lock\n", encoding="utf-8"
        )
        (self.root / "Cargo.lock").write_text(
            "# generated fixture\nversion = 4\n", encoding="utf-8"
        )
        self.git("add", ".gitignore", "Cargo.lock")

    def close(self) -> None:
        self.temporary.cleanup()

    def git(self, *arguments: str) -> subprocess.CompletedProcess[str]:
        return subprocess.run(
            [
                "git",
                "-c",
                "core.hooksPath=/dev/null",
                "-c",
                "core.excludesFile=/dev/null",
                *arguments,
            ],
            cwd=self.root,
            env=self.environment,
            check=True,
            capture_output=True,
            text=True,
            timeout=10,
        )

    def run(self) -> subprocess.CompletedProcess[str]:
        return subprocess.run(
            [sys.executable, str(CHECKER), "--root", str(self.root)],
            env=self.environment,
            check=False,
            capture_output=True,
            text=True,
            timeout=10,
        )


class RootCargoLockTests(unittest.TestCase):
    def setUp(self) -> None:
        self.repository = LockRepository()

    def tearDown(self) -> None:
        self.repository.close()

    def assert_contract_failure(self, expected: str) -> None:
        result = self.repository.run()
        self.assertEqual(
            result.returncode,
            1,
            "mutated root lock repository must fail the production checker: "
            f"expected={expected!r} stdout={result.stdout!r} stderr={result.stderr!r}",
        )
        self.assertIn(
            expected,
            result.stderr,
            "root lock failure must name the violated invariant: "
            f"expected={expected!r} stderr={result.stderr!r}",
        )

    def test_valid_root_lock_and_nested_ignore_policy_passes(self) -> None:
        result = self.repository.run()
        self.assertEqual(
            result.returncode,
            0,
            "tracked root lock and reviewed nested ignores must pass: "
            f"stdout={result.stdout!r} stderr={result.stderr!r}",
        )

    def test_untracked_root_lock_fails_actionably(self) -> None:
        self.repository.git("rm", "--cached", "--quiet", "Cargo.lock")
        self.assert_contract_failure("Cargo.lock must be tracked")

    def test_missing_but_indexed_root_lock_fails_actionably(self) -> None:
        (self.repository.root / "Cargo.lock").unlink()
        self.assert_contract_failure("Cargo.lock is tracked but missing from the worktree")

    def test_ignored_root_lock_fails_actionably(self) -> None:
        (self.repository.root / ".gitignore").write_text("Cargo.lock\n", encoding="utf-8")
        self.assert_contract_failure("root /Cargo.lock must be admitted")

    def test_exposed_nested_probe_lock_fails_actionably(self) -> None:
        (self.repository.root / ".gitignore").write_text(
            "Cargo.lock\n!/Cargo.lock\n!**/Cargo.lock\n", encoding="utf-8"
        )
        self.assert_contract_failure(
            "nested standalone-workspace lockfile must remain ignored"
        )

    def test_symlink_root_lock_is_rejected(self) -> None:
        lock = self.repository.root / "Cargo.lock"
        target = self.repository.root / "elsewhere.lock"
        lock.rename(target)
        lock.symlink_to(target.name)
        self.assert_contract_failure("Cargo.lock must be a regular file")


if __name__ == "__main__":
    unittest.main()
