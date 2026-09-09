#!/usr/bin/env python3
"""Production-boundary regressions for the root Cargo lockfile checker."""

from __future__ import annotations

import os
import importlib.util
import shlex
import shutil
import subprocess
import sys
import tempfile
import time
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parent.parent
CHECKER = ROOT / "scripts/check_root_cargo_lock.py"
TOOLCHAIN_CONTRACT = ROOT / "tests/toolchain_contract.sh"
CHECKER_TIMEOUT_SECONDS = 30
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
            timeout=10,
        )

    def run(
        self,
        *,
        timeout: float = CHECKER_TIMEOUT_SECONDS,
        environment: dict[str, str] | None = None,
    ) -> subprocess.CompletedProcess[str]:
        return subprocess.run(
            [sys.executable, str(CHECKER), "--root", str(self.root)],
            env=environment or self.environment,
            check=False,
            capture_output=True,
            text=True,
            timeout=timeout,
        )


class RootCargoLockTests(unittest.TestCase):
    def setUp(self) -> None:
        self.repository = LockRepository()

    def tearDown(self) -> None:
        self.repository.close()

    def assert_contract_failure(self, expected: str) -> subprocess.CompletedProcess[str]:
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
        return result

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

    def test_force_tracked_nested_lock_fails_actionably(self) -> None:
        nested_lock = NESTED_MANIFESTS[0].parent / "Cargo.lock"
        lock_path = self.repository.root / nested_lock
        lock_path.write_text("# force-tracked fixture\nversion = 4\n", encoding="utf-8")
        self.repository.git("add", "--force", str(nested_lock))
        result = self.assert_contract_failure("only root Cargo.lock may be tracked")
        self.assertIn(
            str(nested_lock),
            result.stderr,
            "tracked nested lock rejection must name the offending path: "
            f"path={nested_lock} stderr={result.stderr!r}",
        )

    def test_nested_manifest_count_is_bounded(self) -> None:
        additional = []
        for index in range(15):
            manifest = Path(f"fixtures/probe-{index}/Cargo.toml")
            manifest_path = self.repository.root / manifest
            manifest_path.parent.mkdir(parents=True)
            manifest_path.write_text("[workspace]\n", encoding="utf-8")
            additional.append(str(manifest))
        self.repository.git("add", *additional)
        self.assert_contract_failure("count=17 maximum=16")

    def test_git_queries_are_batched_within_outer_deadline(self) -> None:
        additional = []
        for index in range(14):
            manifest = Path(f"fixtures/batched-{index}/Cargo.toml")
            manifest_path = self.repository.root / manifest
            manifest_path.parent.mkdir(parents=True)
            manifest_path.write_text("[workspace]\n", encoding="utf-8")
            additional.append(str(manifest))
        self.repository.git("add", *additional)

        real_git = shutil.which("git")
        self.assertIsNotNone(real_git, "test fixture requires git on PATH")
        wrapper_directory = self.repository.root / "delayed-bin"
        wrapper_directory.mkdir()
        invocation_count = self.repository.root / "delayed-git-invocations"
        wrapper = wrapper_directory / "git"
        wrapper.write_text(
            "#!/bin/sh\ncount=0\n"
            f"test -f {shlex.quote(str(invocation_count))} && "
            f"count=$(cat {shlex.quote(str(invocation_count))})\n"
            "count=$((count + 1))\n"
            f"printf '%s\\n' \"$count\" > {shlex.quote(str(invocation_count))}\n"
            "sleep 0.5\nexec "
            f"{shlex.quote(real_git or 'git')} \"$@\"\n",
            encoding="utf-8",
        )
        wrapper.chmod(0o700)
        environment = self.repository.environment.copy()
        environment["PATH"] = f"{wrapper_directory}{os.pathsep}{environment['PATH']}"

        result = self.repository.run(timeout=30, environment=environment)
        self.assertEqual(
            result.returncode,
            0,
            "four delayed batch queries must fit inside the checker deadline: "
            f"stdout={result.stdout!r} stderr={result.stderr!r}",
        )
        self.assertEqual(
            invocation_count.read_text(encoding="utf-8").strip(),
            "4",
            "checker must use exactly two inventory and two ignore queries regardless of "
            "nested manifest count",
        )

    def test_git_query_timeout_fits_inside_outer_deadline(self) -> None:
        specification = importlib.util.spec_from_file_location("root_lock_checker", CHECKER)
        self.assertIsNotNone(specification, f"checker module must be loadable: {CHECKER}")
        assert specification is not None
        self.assertIsNotNone(specification.loader, f"checker loader must exist: {CHECKER}")
        module = importlib.util.module_from_spec(specification)
        assert specification.loader is not None
        sys.modules[specification.name] = module
        try:
            specification.loader.exec_module(module)
        finally:
            sys.modules.pop(specification.name, None)
        inner_deadline = module.MAX_GIT_QUERIES * module.GIT_TIMEOUT_SECONDS
        self.assertLess(
            inner_deadline,
            CHECKER_TIMEOUT_SECONDS,
            "all bounded Git child deadlines must fit within the outer checker deadline: "
            f"queries={module.MAX_GIT_QUERIES} per_query={module.GIT_TIMEOUT_SECONDS}s "
            f"inner={inner_deadline}s outer={CHECKER_TIMEOUT_SECONDS}s",
        )

    def test_timed_out_git_query_reaps_child(self) -> None:
        wrapper_directory = self.repository.root / "blocking-bin"
        wrapper_directory.mkdir()
        child_pid_path = self.repository.root / "child.pid"
        wrapper = wrapper_directory / "git"
        wrapper.write_text(
            "#!/bin/sh\nprintf '%s\\n' \"$$\" > "
            f"{shlex.quote(str(child_pid_path))}\nexec sleep 60\n",
            encoding="utf-8",
        )
        wrapper.chmod(0o700)
        environment = self.repository.environment.copy()
        environment["PATH"] = f"{wrapper_directory}{os.pathsep}{environment['PATH']}"

        result = self.repository.run(environment=environment)
        self.assertEqual(
            result.returncode,
            1,
            "a blocked Git query must fail at its bounded deadline: "
            f"stdout={result.stdout!r} stderr={result.stderr!r}",
        )
        child_pid = int(child_pid_path.read_text(encoding="utf-8").strip())
        deadline = time.monotonic() + 2
        while time.monotonic() < deadline:
            try:
                os.kill(child_pid, 0)
            except ProcessLookupError:
                break
            time.sleep(0.05)
        else:
            self.fail(
                "timed-out Git query must not leave its child alive: "
                f"child_pid={child_pid} stderr={result.stderr!r}"
            )

    def test_git_query_remains_in_callers_process_group(self) -> None:
        real_git = shutil.which("git")
        self.assertIsNotNone(real_git, "test fixture requires git on PATH")
        wrapper_directory = self.repository.root / "process-group-bin"
        wrapper_directory.mkdir()
        observed_group = self.repository.root / "git-process-group"
        wrapper = wrapper_directory / "git"
        wrapper.write_text(
            "#!/bin/sh\nps -o pgid= -p $$ | tr -d ' ' > "
            f"{shlex.quote(str(observed_group))}\n"
            f"exec {shlex.quote(real_git or 'git')} \"$@\"\n",
            encoding="utf-8",
        )
        wrapper.chmod(0o700)
        environment = self.repository.environment.copy()
        environment["PATH"] = f"{wrapper_directory}{os.pathsep}{environment['PATH']}"
        result = self.repository.run(environment=environment)
        self.assertEqual(
            result.returncode,
            0,
            "Git queries must pass without escaping caller supervision: "
            f"stdout={result.stdout!r} stderr={result.stderr!r}",
        )
        self.assertEqual(
            int(observed_group.read_text(encoding="utf-8").strip()),
            os.getpgrp(),
            "Git query must stay in the caller process group so supervisor cancellation "
            "reaches both checker and child",
        )

    def test_git_output_is_bounded_before_manifest_materialization(self) -> None:
        real_git = shutil.which("git")
        self.assertIsNotNone(real_git, "test fixture requires git on PATH")
        wrapper_directory = self.repository.root / "overflow-bin"
        wrapper_directory.mkdir()
        wrapper = wrapper_directory / "git"
        wrapper.write_text(
            "#!/bin/sh\ncase \"$*\" in\n"
            "  *Cargo.toml*) python3 -c 'import sys; sys.stdout.write(\"x\" * 70000)' ;;\n"
            f"  *) exec {shlex.quote(real_git or 'git')} \"$@\" ;;\n"
            "esac\n",
            encoding="utf-8",
        )
        wrapper.chmod(0o700)
        environment = self.repository.environment.copy()
        environment["PATH"] = f"{wrapper_directory}{os.pathsep}{environment['PATH']}"
        result = self.repository.run(environment=environment)
        self.assertEqual(
            result.returncode,
            1,
            "oversized Git output must fail without full materialization: "
            f"stdout={result.stdout!r} stderr={result.stderr!r}",
        )
        self.assertIn(
            "Git query output exceeded the checker resource bound",
            result.stderr,
            "output-bound failure must identify the exhausted resource: "
            f"stderr={result.stderr!r}",
        )

    def test_inventory_change_during_check_is_rejected(self) -> None:
        real_git = shutil.which("git")
        self.assertIsNotNone(real_git, "test fixture requires git on PATH")
        wrapper_directory = self.repository.root / "racing-bin"
        wrapper_directory.mkdir()
        invocation_count = self.repository.root / "git-invocations"
        nested_lock = NESTED_MANIFESTS[0].parent / "Cargo.lock"
        nested_lock_path = self.repository.root / nested_lock
        wrapper = wrapper_directory / "git"
        wrapper.write_text(
            "#!/bin/sh\ncount=0\n"
            f"test -f {shlex.quote(str(invocation_count))} && "
            f"count=$(cat {shlex.quote(str(invocation_count))})\n"
            "count=$((count + 1))\n"
            f"printf '%s\\n' \"$count\" > {shlex.quote(str(invocation_count))}\n"
            "if test \"$count\" -eq 2; then\n"
            f"  printf '%s\\n' 'version = 4' > {shlex.quote(str(nested_lock_path))}\n"
            f"  {shlex.quote(real_git or 'git')} add --force "
            f"{shlex.quote(str(nested_lock))}\n"
            "fi\n"
            f"exec {shlex.quote(real_git or 'git')} \"$@\"\n",
            encoding="utf-8",
        )
        wrapper.chmod(0o700)
        environment = self.repository.environment.copy()
        environment["PATH"] = f"{wrapper_directory}{os.pathsep}{environment['PATH']}"
        result = self.repository.run(environment=environment)
        self.assertEqual(
            result.returncode,
            1,
            "repository mutation between bounded queries must invalidate the snapshot: "
            f"stdout={result.stdout!r} stderr={result.stderr!r}",
        )
        self.assertIn(
            "inventory changed during the root-lock check",
            result.stderr,
            "racing repository failure must name the inconsistent snapshot: "
            f"stderr={result.stderr!r}",
        )

    def test_ignore_policy_change_during_check_is_rejected(self) -> None:
        real_git = shutil.which("git")
        self.assertIsNotNone(real_git, "test fixture requires git on PATH")
        wrapper_directory = self.repository.root / "policy-racing-bin"
        wrapper_directory.mkdir()
        invocation_count = self.repository.root / "policy-git-invocations"
        wrapper = wrapper_directory / "git"
        wrapper.write_text(
            "#!/bin/sh\ncount=0\n"
            f"test -f {shlex.quote(str(invocation_count))} && "
            f"count=$(cat {shlex.quote(str(invocation_count))})\n"
            "count=$((count + 1))\n"
            f"printf '%s\\n' \"$count\" > {shlex.quote(str(invocation_count))}\n"
            "if test \"$count\" -eq 4; then\n"
            f"  printf '%s\\n' 'Cargo.lock' > "
            f"{shlex.quote(str(self.repository.root / '.gitignore'))}\n"
            "fi\n"
            f"exec {shlex.quote(real_git or 'git')} \"$@\"\n",
            encoding="utf-8",
        )
        wrapper.chmod(0o700)
        environment = self.repository.environment.copy()
        environment["PATH"] = f"{wrapper_directory}{os.pathsep}{environment['PATH']}"
        result = self.repository.run(environment=environment)
        self.assertEqual(
            result.returncode,
            1,
            "ignore policy mutation between snapshots must invalidate the check: "
            f"stdout={result.stdout!r} stderr={result.stderr!r}",
        )
        self.assertIn(
            "ignore policy changed during the root-lock check",
            result.stderr,
            "policy-race failure must name the inconsistent ignore decision: "
            f"stderr={result.stderr!r}",
        )

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

    def test_untracked_gitignore_cannot_supply_reviewed_policy(self) -> None:
        self.repository.git("rm", "--cached", "--quiet", ".gitignore")
        self.assert_contract_failure(".gitignore must be tracked")

    def test_inherited_alternate_index_cannot_certify_tracking(self) -> None:
        alternate_index = self.repository.root / "alternate.index"
        shutil.copy2(self.repository.root / ".git/index", alternate_index)
        self.repository.git("rm", "--cached", "--quiet", "Cargo.lock")
        self.repository.environment["GIT_INDEX_FILE"] = str(alternate_index)
        self.assert_contract_failure("Cargo.lock must be tracked")

    def test_inherited_repository_selectors_cannot_redirect_tracking(self) -> None:
        authority = LockRepository()
        try:
            self.repository.git("rm", "--cached", "--quiet", "Cargo.lock")
            self.repository.environment["GIT_DIR"] = str(authority.root / ".git")
            self.repository.environment["GIT_WORK_TREE"] = str(authority.root)
            self.assert_contract_failure("Cargo.lock must be tracked")
        finally:
            authority.close()

    def test_non_utf8_tracked_manifest_path_is_handled_without_traceback(self) -> None:
        real_git = os.fsencode(shutil.which("git") or "git")
        blob = subprocess.run(
            [real_git, b"hash-object", b"-w", b"--stdin"],
            cwd=self.repository.root,
            env=self.repository.environment,
            input=b"[workspace]\n",
            check=True,
            capture_output=True,
            timeout=10,
        ).stdout.strip()
        raw_path = b"probe-\xff/Cargo.toml"
        subprocess.run(
            [real_git, b"update-index", b"--add", b"-z", b"--index-info"],
            cwd=self.repository.root,
            env=self.repository.environment,
            input=b"100644 " + blob + b"\t" + raw_path + b"\0",
            check=True,
            capture_output=True,
            timeout=10,
        )
        result = self.repository.run()
        self.assertEqual(
            result.returncode,
            0,
            "Git path bytes must round-trip through ignore checks without a Unicode "
            f"traceback: stdout={result.stdout!r} stderr={result.stderr!r}",
        )

    def test_toolchain_contract_requires_locked_metadata(self) -> None:
        wrapper_directory = self.repository.root / "toolchain-bin"
        wrapper_directory.mkdir()
        cargo_invocations = self.repository.root / "cargo-invocations"
        for name, body in (
            ("python3", "#!/bin/sh\nexit 0\n"),
            ("rustc", "#!/bin/sh\necho rustc 1.98.0 fixture\n"),
            (
                "cargo",
                "#!/bin/sh\nprintf '%s\\n' \"$*\" >> "
                f"{shlex.quote(str(cargo_invocations))}\n",
            ),
        ):
            executable = wrapper_directory / name
            executable.write_text(body, encoding="utf-8")
            executable.chmod(0o700)
        environment = self.repository.environment.copy()
        environment["PATH"] = f"{wrapper_directory}{os.pathsep}{environment['PATH']}"
        result = subprocess.run(
            ["bash", str(TOOLCHAIN_CONTRACT), "--metadata-only"],
            cwd=ROOT,
            env=environment,
            check=False,
            capture_output=True,
            text=True,
            timeout=30,
        )
        self.assertEqual(
            result.returncode,
            0,
            "toolchain metadata-only gate must execute with controlled tool boundaries: "
            f"stdout={result.stdout!r} stderr={result.stderr!r}",
        )
        self.assertEqual(
            cargo_invocations.read_text(encoding="utf-8").splitlines(),
            ["metadata --locked --no-deps --format-version 1"],
            "toolchain gate must execute exactly one locked metadata resolution before "
            "accepting Cargo.toml and Cargo.lock",
        )

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
