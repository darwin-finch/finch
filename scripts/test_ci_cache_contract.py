#!/usr/bin/env python3
"""Mutation-sensitive regressions for the canonical CI cache contract."""

from __future__ import annotations

import shutil
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parent.parent
CHECKER = ROOT / "scripts/check_ci_cache_contract.py"


class WorkflowRepository:
    def __init__(self) -> None:
        self.temporary = tempfile.TemporaryDirectory()
        self.root = Path(self.temporary.name)
        workflows = self.root / ".github/workflows"
        workflows.mkdir(parents=True)
        for name in ("ci.yml", "release.yml"):
            shutil.copy2(ROOT / ".github/workflows" / name, workflows / name)

    def close(self) -> None:
        self.temporary.cleanup()

    def mutate_once(self, workflow: str, old: str, new: str) -> None:
        path = self.root / ".github/workflows" / workflow
        contents = path.read_text(encoding="utf-8")
        if old not in contents:
            raise AssertionError(
                f"mutation precondition could not find {old!r} in {workflow}"
            )
        path.write_text(contents.replace(old, new, 1), encoding="utf-8")

    def mutate_all(self, workflow: str, old: str, new: str, expected: int) -> None:
        path = self.root / ".github/workflows" / workflow
        contents = path.read_text(encoding="utf-8")
        actual = contents.count(old)
        if actual != expected:
            raise AssertionError(
                f"mutation precondition expected {expected} copies of {old!r} in {workflow}; found {actual}"
            )
        path.write_text(contents.replace(old, new), encoding="utf-8")

    def run(self) -> subprocess.CompletedProcess[str]:
        return subprocess.run(
            [sys.executable, str(CHECKER), "--root", str(self.root)],
            check=False,
            capture_output=True,
            text=True,
        )


class CiCacheContractTests(unittest.TestCase):
    def setUp(self) -> None:
        self.repo = WorkflowRepository()

    def tearDown(self) -> None:
        self.repo.close()

    def assert_rejected(self, diagnostic: str) -> None:
        result = self.repo.run()
        self.assertEqual(result.returncode, 1, result.stdout + result.stderr)
        self.assertIn(diagnostic, result.stderr, result.stdout + result.stderr)

    def test_current_canonical_workflows_pass(self) -> None:
        result = self.repo.run()
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn("canonical Cargo jobs", result.stdout)

    def test_rejects_uncovered_expensive_job_with_job_and_path(self) -> None:
        self.repo.mutate_once(
            "ci.yml",
            "          target/**/.fingerprint\n          target/**/build\n          target/**/deps\n        key: cargo-target-v1-${{ runner.os }}-${{ matrix.target }}-rust-1.98.0-profile-dev-test",
            "          removed-target\n        key: cargo-target-v1-${{ runner.os }}-${{ matrix.target }}-rust-1.98.0-profile-dev-test",
        )
        self.assert_rejected(
            ".github/workflows/ci.yml job 'test' needs exactly one compatible target-artifact cache"
        )

    def test_rejects_new_uncovered_expensive_job(self) -> None:
        path = self.repo.root / ".github/workflows/ci.yml"
        with path.open("a", encoding="utf-8") as workflow:
            workflow.write(
                "\n  newly-expensive:\n"
                "    runs-on: ubuntu-24.04\n"
                "    steps:\n"
                "    - run: cargo test --all-targets\n"
            )
        self.assert_rejected(
            "job 'newly-expensive' compiles Finch with Cargo but has no declared target/profile/feature compatibility dimensions"
        )

    def test_rejects_each_missing_target_compatibility_dimension(self) -> None:
        test_prefix = (
            "cargo-target-v1-${{ runner.os }}-${{ matrix.target }}-rust-1.98.0-"
            "profile-dev-test-features-${{ matrix.cache_feature_set }}-cfg-"
        )
        mutations = (
            (
                test_prefix,
                test_prefix.replace("${{ matrix.target }}", "target-missing"),
                "matrix.target",
            ),
            (
                test_prefix,
                test_prefix.replace("profile-dev-test", "profile-missing"),
                "profile-dev-test",
            ),
            (
                test_prefix,
                test_prefix.replace(
                    "features-${{ matrix.cache_feature_set }}", "features-missing"
                ),
                "features-${{ matrix.cache_feature_set }}",
            ),
            (
                test_prefix,
                test_prefix.replace("rust-1.98.0", "rust-missing"),
                "rust-1.98.0",
            ),
        )
        for old, new, diagnostic in mutations:
            with self.subTest(dimension=diagnostic):
                repo = WorkflowRepository()
                try:
                    repo.mutate_all("ci.yml", old, new, expected=2)
                    result = repo.run()
                    self.assertEqual(result.returncode, 1, result.stdout + result.stderr)
                    self.assertIn(diagnostic, result.stderr, result.stdout + result.stderr)
                finally:
                    repo.close()

        repo = WorkflowRepository()
        try:
            config_hash = (
                "${{ hashFiles('rust-toolchain.toml', '.cargo/config.toml', "
                "'Cargo.toml', '.github/workflows/ci.yml') }}"
            )
            repo.mutate_all("ci.yml", config_hash, "rust-config-missing", expected=6)
            result = repo.run()
            self.assertEqual(result.returncode, 1, result.stdout + result.stderr)
            self.assertIn("hashFiles('rust-toolchain.toml'", result.stderr)
        finally:
            repo.close()

    def test_rejects_lock_insensitive_dependency_key(self) -> None:
        self.repo.mutate_once(
            "release.yml",
            "key: cargo-deps-v1-${{ runner.os }}-rust-1.98.0-${{ hashFiles('**/Cargo.lock', '**/Cargo.toml') }}",
            "key: cargo-deps-v1-${{ runner.os }}-rust-1.98.0-${{ hashFiles('**/Cargo.toml') }}",
        )
        self.assert_rejected("**/Cargo.lock")

    def test_rejects_lockfile_resolution_after_cache_key_evaluation(self) -> None:
        self.repo.mutate_once(
            "release.yml",
            "      - name: Resolve Cargo dependency graph\n        run: cargo generate-lockfile\n\n",
            "",
        )
        self.repo.mutate_once(
            "release.yml",
            "      - name: Install Linux dependencies",
            "      - name: Resolve Cargo dependency graph\n        run: cargo generate-lockfile\n\n      - name: Install Linux dependencies",
        )
        self.assert_rejected("must resolve Cargo.lock before its first cache key")

    def test_rejects_cache_restore_after_compilation(self) -> None:
        self.repo.mutate_once(
            "ci.yml",
            "    - name: Build binary\n      run: cargo build --bin finch ${{ matrix.cargo_args }} --verbose\n",
            "",
        )
        self.repo.mutate_once(
            "ci.yml",
            "    - name: Cache Cargo dependency sources\n",
            "    - name: Build binary\n      run: cargo build --bin finch ${{ matrix.cargo_args }} --verbose\n\n    - name: Cache Cargo dependency sources\n",
        )
        self.assert_rejected("before its first expensive Cargo command")

    def test_rejects_restore_fallback_across_incompatible_features(self) -> None:
        self.repo.mutate_once(
            "ci.yml",
            "restore-keys: |\n          cargo-target-v1-${{ runner.os }}-${{ matrix.target }}-rust-1.98.0-profile-dev-test-features-${{ matrix.cache_feature_set }}-cfg-",
            "restore-keys: |\n          cargo-target-v1-${{ runner.os }}-${{ matrix.target }}-rust-1.98.0-profile-dev-test-features-any-cfg-",
        )
        self.assert_rejected("features-${{ matrix.cache_feature_set }}")

    def test_rejects_feature_key_context_missing_from_matrix(self) -> None:
        self.repo.mutate_once(
            "ci.yml",
            "            cache_feature_set: default-and-all-features-clippy\n",
            "",
        )
        self.assert_rejected("matrix is missing actual compiled feature-set identity")

    def test_rejects_workflow_config_collision_between_ci_and_release(self) -> None:
        self.repo.mutate_all(
            "release.yml",
            ".github/workflows/release.yml",
            ".github/workflows/ci.yml",
            expected=2,
        )
        self.assert_rejected(".github/workflows/release.yml")

    def test_rejects_cached_incremental_compiler_state(self) -> None:
        self.repo.mutate_once("release.yml", "  CARGO_INCREMENTAL: 0\n", "")
        self.assert_rejected("must set CARGO_INCREMENTAL: 0")

    def test_rejects_cache_miss_as_correctness_prerequisite(self) -> None:
        self.repo.mutate_once(
            "release.yml",
            "          path: |\n            ~/.cargo/registry",
            "          fail-on-cache-miss: true\n          path: |\n            ~/.cargo/registry",
        )
        self.assert_rejected("must not make an ordinary cache miss fail the job")

    def test_rejects_cache_outage_masking_cargo_results(self) -> None:
        self.repo.mutate_once(
            "release.yml",
            "        continue-on-error: true\n",
            "        continue-on-error: false\n",
        )
        self.assert_rejected("cache outage cannot hide Cargo results")

    def test_rejects_pull_request_target_cache_scope(self) -> None:
        self.repo.mutate_once("ci.yml", "  pull_request:\n", "  pull_request_target:\n")
        self.assert_rejected("must not use pull_request_target")


if __name__ == "__main__":
    unittest.main()
