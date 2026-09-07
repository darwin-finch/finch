#!/usr/bin/env python3
"""Mutation-sensitive regressions for Finch's CI cache contract."""

from __future__ import annotations

import re
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

    def path(self, workflow: str) -> Path:
        return self.root / ".github/workflows" / workflow

    def mutate_once(self, workflow: str, old: str, new: str) -> None:
        path = self.path(workflow)
        contents = path.read_text(encoding="utf-8")
        if old not in contents:
            raise AssertionError(f"could not find mutation {old!r} in {workflow}")
        path.write_text(contents.replace(old, new, 1), encoding="utf-8")

    def mutate_job(self, workflow: str, job: str, old: str, new: str) -> None:
        path = self.path(workflow)
        contents = path.read_text(encoding="utf-8")
        match = re.search(
            rf"(?ms)^  {re.escape(job)}:\n.*?(?=^  [A-Za-z0-9_-]+:\n|\Z)",
            contents,
        )
        if match is None or old not in match.group(0):
            raise AssertionError(f"could not find mutation {old!r} in {workflow} job {job}")
        block = match.group(0).replace(old, new, 1)
        path.write_text(contents[: match.start()] + block + contents[match.end() :], encoding="utf-8")

    def append_ci_job(self, command: str) -> None:
        with self.path("ci.yml").open("a", encoding="utf-8") as workflow:
            workflow.write(
                "\n  newly-expensive:\n"
                "    runs-on: ubuntu-24.04\n"
                "    steps:\n"
                f"    - run: {command}\n"
            )

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
        self.assertIn("5 bounded keys per dependency generation", result.stdout)
        self.assertIn("older generations are quota-LRU", result.stdout)

    def test_rejects_missing_cache_on_known_expensive_job(self) -> None:
        self.repo.mutate_job(
            "ci.yml",
            "runtime-authority",
            "          ~/.cargo/registry/cache\n",
            "",
        )
        self.assert_rejected(
            "job 'runtime-authority' needs exactly one resolved Cargo download restore"
        )

    def test_discovers_new_expensive_job_behind_wrapper_and_cargo_options(self) -> None:
        self.repo.append_ci_job(
            "timeout 2m env CARGO_NET_RETRY=2 cargo --config net.retry=2 test --lib"
        )
        self.assert_rejected("job 'newly-expensive' newly compiles with Cargo")

    def test_does_not_mistake_cargo_fmt_check_flag_for_compilation(self) -> None:
        result = self.repo.run()
        self.assertNotIn("job 'windows-format-contract' newly compiles", result.stderr)

    def test_rejects_combined_restore_and_save_action(self) -> None:
        self.repo.mutate_job(
            "ci.yml", "test", "actions/cache/restore@v4", "actions/cache@v4"
        )
        self.assert_rejected("must use explicit actions/cache/restore@v4 or save@v4")

    def test_rejects_consumer_cache_save(self) -> None:
        self.repo.mutate_job(
            "release.yml",
            "build-release",
            "actions/cache/restore@v4",
            "actions/cache/save@v4",
        )
        self.assert_rejected("is a consumer and must be restore-only")

    def test_rejects_untrusted_producer_job(self) -> None:
        self.repo.mutate_job(
            "ci.yml",
            "cargo-download-cache",
            "if: github.event_name == 'push' && github.ref == 'refs/heads/main'",
            "if: github.event_name == 'pull_request'",
        )
        self.assert_rejected("must run only for a trusted main push")

    def test_rejects_producer_save_without_explicit_trust(self) -> None:
        self.repo.mutate_job(
            "ci.yml",
            "cargo-download-cache",
            "if: steps.cargo-downloads.outputs.cache-hit != 'true' && github.event_name == 'push' && github.ref == 'refs/heads/main'",
            "if: steps.cargo-downloads.outputs.cache-hit != 'true'",
        )
        self.assert_rejected("save must require a miss and an explicit trusted main push")

    def test_rejects_partial_target_dependency_producer(self) -> None:
        self.repo.mutate_job(
            "ci.yml",
            "cargo-download-cache",
            "run: cargo fetch --verbose",
            "run: cargo fetch --verbose --target x86_64-unknown-linux-gnu",
        )
        self.assert_rejected("all-target fetch")

    def test_rejects_extra_or_missing_producer_matrix_variant(self) -> None:
        self.repo.mutate_job(
            "ci.yml",
            "cargo-download-cache",
            "os: [ubuntu-24.04, macos-14]",
            "os: [ubuntu-24.04, macos-14, ubuntu-latest]",
        )
        self.assert_rejected("exactly one Linux and one macOS download key")

    def test_rejects_duplicated_extracted_registry_sources(self) -> None:
        self.repo.mutate_job(
            "ci.yml",
            "test",
            "          ~/.cargo/registry/cache\n",
            "          ~/.cargo/registry/cache\n          ~/.cargo/registry/src\n",
        )
        self.assert_rejected("registry/src")

    def test_rejects_target_and_native_ort_outputs(self) -> None:
        for forbidden in ("target/**/deps", "~/.cache/ort.pyke.io"):
            with self.subTest(path=forbidden):
                repo = WorkflowRepository()
                try:
                    repo.mutate_job(
                        "release.yml",
                        "build-release",
                        "            ~/.cargo/git/db\n",
                        f"            ~/.cargo/git/db\n            {forbidden}\n",
                    )
                    result = repo.run()
                    self.assertEqual(result.returncode, 1, result.stdout + result.stderr)
                    self.assertIn(forbidden, result.stderr, result.stdout + result.stderr)
                finally:
                    repo.close()

    def test_rejects_lock_resolution_before_registry_restore(self) -> None:
        self.repo.mutate_job(
            "ci.yml",
            "test",
            "    - name: Resolve Cargo dependency graph\n      run: cargo generate-lockfile\n",
            "",
        )
        self.repo.mutate_job(
            "ci.yml",
            "test",
            "    - name: Restore Cargo registry index bootstrap\n",
            "    - name: Resolve Cargo dependency graph\n      run: cargo generate-lockfile\n\n    - name: Restore Cargo registry index bootstrap\n",
        )
        self.assert_rejected("restore the index, resolve Cargo.lock")

    def test_rejects_restore_after_compilation_begins(self) -> None:
        self.repo.mutate_job(
            "ci.yml",
            "test",
            "    - name: Build binary\n      run: cargo build --bin finch ${{ matrix.cargo_args }} --verbose\n",
            "",
        )
        self.repo.mutate_job(
            "ci.yml",
            "test",
            "    - name: Restore resolved Cargo downloads\n",
            "    - name: Build binary\n      run: cargo build --bin finch ${{ matrix.cargo_args }} --verbose\n\n    - name: Restore resolved Cargo downloads\n",
        )
        self.assert_rejected("restores resolved Cargo downloads after compilation starts")

    def test_rejects_each_download_key_identity_omission(self) -> None:
        omissions = (
            ("${{ runner.os }}", "os-missing", "bind OS"),
            ("rust-1.98.0", "rust-missing", "bind OS"),
            ("'.cargo/config', ", "", "both Cargo config names"),
            ("'**/Cargo.lock', ", "", "Cargo.lock"),
            ("'**/Cargo.toml'", "'Cargo.toml'", "every Cargo.toml"),
        )
        for old, new, diagnostic in omissions:
            with self.subTest(identity=diagnostic):
                repo = WorkflowRepository()
                try:
                    repo.mutate_job("release.yml", "build-release", old, new)
                    result = repo.run()
                    self.assertEqual(result.returncode, 1, result.stdout + result.stderr)
                    self.assertIn(diagnostic, result.stderr, result.stdout + result.stderr)
                finally:
                    repo.close()

    def test_rejects_manifest_key_in_place_of_resolved_lock_key(self) -> None:
        self.repo.mutate_job(
            "release.yml",
            "build-release",
            "lock-${{ hashFiles('**/Cargo.lock') }}",
            "lock-${{ hashFiles('**/Cargo.toml') }}",
        )
        self.assert_rejected("generated Cargo.lock")

    def test_rejects_uncontracted_third_consumer_cache(self) -> None:
        self.repo.mutate_job(
            "ci.yml",
            "test",
            "    - name: Run clippy",
            "    - name: Restore unrelated cache\n      uses: actions/cache/restore@v4\n      continue-on-error: true\n      with:\n        path: ~/.cache/unrelated\n        key: unrelated\n\n    - name: Run clippy",
        )
        self.assert_rejected("may contain only index and resolved-download restores")

    def test_rejects_extra_producer_save_and_cache_cardinality(self) -> None:
        self.repo.mutate_job(
            "ci.yml",
            "cargo-download-cache",
            "    - name: Save complete Cargo downloads\n",
            "    - name: Save extra cache\n      if: github.event_name == 'push' && github.ref == 'refs/heads/main'\n      uses: actions/cache/save@v4\n      continue-on-error: true\n      with:\n        path: ~/.cache/extra\n        key: extra\n\n    - name: Save complete Cargo downloads\n",
        )
        self.assert_rejected("one trusted save per layer")

    def test_rejects_fallback_across_cargo_configuration(self) -> None:
        self.repo.mutate_job(
            "ci.yml",
            "test",
            "restore-keys: |\n          cargo-resolution-v1-${{ runner.os }}-rust-1.98.0-config-${{ hashFiles('rust-toolchain.toml', '.cargo/config', '.cargo/config.toml') }}-manifests-",
            "restore-keys: |\n          cargo-resolution-v1-${{ runner.os }}-rust-1.98.0-config-any-manifests-",
        )
        self.assert_rejected("resolution fallback may vary only the manifest/lock fingerprint")

    def test_rejects_cache_miss_as_a_correctness_prerequisite(self) -> None:
        self.repo.mutate_job(
            "release.yml",
            "build-release",
            "        path: |\n",
            "        fail-on-cache-miss: true\n        path: |\n",
        )
        self.assert_rejected("cache miss as an ordinary uncached build")

    def test_rejects_cache_failure_masking_real_cargo_result(self) -> None:
        self.repo.mutate_job(
            "ci.yml",
            "test",
            "      continue-on-error: true",
            "      continue-on-error: false",
        )
        self.assert_rejected("cache failures must not prevent the real Cargo command")

    def test_rejects_unpinned_cargo_audit_fallback(self) -> None:
        self.repo.mutate_job(
            "ci.yml",
            "security",
            "cargo install cargo-audit --version 0.22.2 --locked",
            "cargo install cargo-audit --locked",
        )
        self.assert_rejected("pinned cargo-audit fallback/verification")

    def test_rejects_wrong_cargo_audit_cache_version(self) -> None:
        self.repo.mutate_job(
            "ci.yml",
            "security",
            "cargo-audit-0.22.2",
            "cargo-audit-latest",
        )
        self.assert_rejected("cache key must pin reviewed version 0.22.2")

    def test_rejects_cargo_audit_restore_after_install(self) -> None:
        self.repo.mutate_job(
            "ci.yml",
            "security",
            "    - name: Restore pinned cargo-audit\n",
            "    - run: cargo install cargo-audit --version 0.22.2 --locked\n\n    - name: Restore pinned cargo-audit\n",
        )
        self.assert_rejected("restores cargo-audit after fallback compilation starts")


if __name__ == "__main__":
    unittest.main()
