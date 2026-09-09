#!/usr/bin/env python3
"""Production-boundary regressions for the root Cargo lockfile checker."""

from __future__ import annotations

import os
import shutil
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parent.parent
CHECKER = ROOT / "scripts/check_root_cargo_lock.py"
NESTED_MANIFESTS = (
    Path(".github/issue-105-windows-probe/Cargo.toml"),
    Path(".github/issue-201-windows-probe/Cargo.toml"),
)


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
        for manifest in NESTED_MANIFESTS:
            manifest_path = self.root / manifest
            manifest_path.parent.mkdir(parents=True, exist_ok=True)
            manifest_path.write_text("[workspace]\n", encoding="utf-8")
        self.git(
            "add",
            ".gitignore",
            "Cargo.lock",
            *(str(manifest) for manifest in NESTED_MANIFESTS),
        )

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
            timeout=120,
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
        for manifest in NESTED_MANIFESTS:
            with self.subTest(manifest=manifest):
                nested_lock = manifest.parent / "Cargo.lock"
                (self.repository.root / ".gitignore").write_text(
                    f"Cargo.lock\n!/Cargo.lock\n!/{nested_lock}\n", encoding="utf-8"
                )
                self.assert_contract_failure(str(nested_lock))

    def test_new_tracked_nested_manifest_is_discovered(self) -> None:
        manifest = Path(".github/new-probe/Cargo.toml")
        manifest_path = self.repository.root / manifest
        manifest_path.parent.mkdir(parents=True)
        manifest_path.write_text("[workspace]\n", encoding="utf-8")
        self.repository.git("add", str(manifest))
        nested_lock = manifest.parent / "Cargo.lock"
        (self.repository.root / ".gitignore").write_text(
            f"Cargo.lock\n!/Cargo.lock\n!/{nested_lock}\n", encoding="utf-8"
        )
        self.assert_contract_failure(str(nested_lock))

    def test_private_exclude_cannot_replace_reviewed_nested_ignore(self) -> None:
        (self.repository.root / ".gitignore").write_text(
            "!/Cargo.lock\n", encoding="utf-8"
        )
        (self.repository.root / ".git/info/exclude").write_text(
            "Cargo.lock\n", encoding="utf-8"
        )
        self.assert_contract_failure(
            "nested lockfile ignore must come from the reviewed .gitignore"
        )

    def test_inherited_alternate_index_cannot_certify_tracking(self) -> None:
        alternate_index = self.repository.root / "alternate.index"
        shutil.copy2(self.repository.root / ".git/index", alternate_index)
        self.repository.git("rm", "--cached", "--quiet", "Cargo.lock")
        self.repository.environment["GIT_INDEX_FILE"] = str(alternate_index)
        self.assert_contract_failure("Cargo.lock must be tracked")

    def test_configured_fsmonitor_is_not_executed(self) -> None:
        marker = self.repository.root / "fsmonitor-ran"
        hook = self.repository.root / "fsmonitor-hook"
        hook.write_text(f"#!/bin/sh\ntouch {marker}\n", encoding="utf-8")
        hook.chmod(0o700)
        self.repository.git("config", "core.fsmonitor", str(hook))
        result = self.repository.run()
        self.assertEqual(
            result.returncode,
            0,
            "configured fsmonitor must be disabled without breaking the checker: "
            f"stdout={result.stdout!r} stderr={result.stderr!r}",
        )
        self.assertFalse(
            marker.exists(),
            "root lock checker must not execute repository-configured fsmonitor code: "
            f"hook={hook} marker={marker}",
        )

    def test_symlink_root_lock_is_rejected(self) -> None:
        lock = self.repository.root / "Cargo.lock"
        target = self.repository.root / "elsewhere.lock"
        lock.rename(target)
        lock.symlink_to(target.name)
        self.assert_contract_failure("Cargo.lock must be a regular file")


if __name__ == "__main__":
    unittest.main()
