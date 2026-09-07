#!/usr/bin/env python3
"""Mutation-sensitive tests for the canonical CI cache production contract."""

from __future__ import annotations

import os
import shutil
import subprocess
import tempfile
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parent.parent
CHECKER = ROOT / "scripts/check_ci_cache_contract.rb"


class ContractRepository:
    def __init__(self) -> None:
        self.temporary = tempfile.TemporaryDirectory()
        self.root = Path(self.temporary.name)
        for relative in (
            ".github/workflows/ci.yml",
            ".github/workflows/release.yml",
            "scripts/configure_ci_cargo_home.sh",
            "scripts/configure_ci_sccache.sh",
        ):
            destination = self.root / relative
            destination.parent.mkdir(parents=True, exist_ok=True)
            shutil.copy2(ROOT / relative, destination)

    def close(self) -> None:
        self.temporary.cleanup()

    def replace(self, relative: str, old: str, new: str) -> None:
        path = self.root / relative
        contents = path.read_text(encoding="utf-8")
        if old not in contents:
            raise AssertionError(f"mutation source {old!r} is absent from {relative}")
        path.write_text(contents.replace(old, new, 1), encoding="utf-8")

    def mutate_job(self, workflow: str, job: str, old: str, new: str) -> None:
        path = self.root / ".github/workflows" / workflow
        contents = path.read_text(encoding="utf-8")
        marker = f"  {job}:\n"
        start = contents.index(marker)
        next_job = contents.find("\n  ", start + len(marker))
        while next_job != -1:
            candidate = contents[next_job + 3 :].split(":", 1)[0]
            if candidate and not candidate.startswith(" ") and "\n" not in candidate:
                break
            next_job = contents.find("\n  ", next_job + 3)
        end = len(contents) if next_job == -1 else next_job + 1
        block = contents[start:end]
        if old not in block:
            raise AssertionError(f"mutation source {old!r} is absent from {workflow}:{job}")
        path.write_text(contents[:start] + block.replace(old, new, 1) + contents[end:], encoding="utf-8")

    def append_job(self, body: str) -> None:
        path = self.root / ".github/workflows/ci.yml"
        with path.open("a", encoding="utf-8") as workflow:
            workflow.write(body)

    def run(self) -> subprocess.CompletedProcess[str]:
        return subprocess.run(
            ["ruby", str(CHECKER), str(self.root)],
            check=False,
            capture_output=True,
            text=True,
        )


class CiCacheContractTests(unittest.TestCase):
    def setUp(self) -> None:
        self.repo = ContractRepository()

    def tearDown(self) -> None:
        self.repo.close()

    def assert_rejected(self, diagnostic: str) -> None:
        result = self.repo.run()
        output = result.stdout + result.stderr
        self.assertEqual(result.returncode, 1, output)
        self.assertIn(diagnostic, output, output)

    def test_current_workflows_pass_semantic_contract(self) -> None:
        result = self.repo.run()
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn("four trusted sccache writer lanes", result.stdout)
        self.assertIn("PR/release read-only", result.stdout)

    def test_rejects_cumulative_dependency_restore_prefix(self) -> None:
        self.repo.mutate_job(
            "ci.yml",
            "test",
            "        key: cargo-downloads-v1-",
            "        restore-keys: cargo-downloads-v1-\n        key: cargo-downloads-v1-",
        )
        self.assert_rejected("must not have cumulative restore-keys")

    def test_rejects_second_cold_cargo_audit_compiler(self) -> None:
        self.repo.append_job(
            "\n  duplicate-audit:\n"
            "    runs-on: ubuntu-24.04\n"
            "    steps:\n"
            "    - run: cargo install cargo-audit --version 0.22.2 --locked\n"
        )
        self.assert_rejected("cargo-audit must have exactly one cold compiler; found 2")

    def test_rejects_mutable_cache_action_reference(self) -> None:
        self.repo.replace(
            ".github/workflows/ci.yml",
            "actions/cache/restore@0057852bfaa89a56745cba8c7296529d2fc39830",
            "actions/cache/restore@v4",
        )
        self.assert_rejected("not an approved full-SHA-pinned cache action")

    def test_rejects_workflow_wide_release_write_permission(self) -> None:
        self.repo.replace(
            ".github/workflows/release.yml", "permissions:\n  contents: read", "permissions:\n  contents: write"
        )
        self.assert_rejected("workflow permissions must be exactly contents: read")

    def test_rejects_mutable_action_with_release_authority(self) -> None:
        self.repo.replace(
            ".github/workflows/release.yml",
            "actions/download-artifact@d3f86a106a0bac45b974a628896c90dbdf5c8093",
            "actions/download-artifact@v4",
        )
        self.assert_rejected("executes with release authority and must use a full commit SHA")

    def test_rejects_alternate_writable_cache_action(self) -> None:
        self.repo.mutate_job(
            "ci.yml",
            "test",
            "    - name: Build binary\n",
            "    - name: Hidden target writer\n"
            "      uses: Swatinem/rust-cache@v2\n"
            "    - name: Build binary\n",
        )
        self.assert_rejected("Swatinem/rust-cache@v2")

    def test_rejects_disabled_required_restore(self) -> None:
        self.repo.mutate_job(
            "ci.yml",
            "runtime-authority",
            "    - name: Restore exact Cargo downloads\n",
            "    - name: Restore exact Cargo downloads\n      if: false\n",
        )
        self.assert_rejected("dependency restore is disabled")

    def test_rejects_disabled_complete_fetch(self) -> None:
        self.repo.mutate_job(
            "ci.yml",
            "cargo-download-cache",
            "      if: steps.cargo-downloads.outputs.cache-hit != 'true'\n      run: cargo fetch",
            "      if: false\n      run: cargo fetch",
        )
        self.assert_rejected("clean complete fetch is disabled")

    def test_rejects_disabled_dependency_save(self) -> None:
        self.repo.mutate_job(
            "ci.yml",
            "cargo-download-cache",
            "    - name: Save complete exact Cargo downloads\n      if:",
            "    - name: Save complete exact Cargo downloads\n      if: false #",
        )
        self.assert_rejected("save must be executable only after a trusted-main cache miss")

    def test_rejects_logically_impossible_dependency_save(self) -> None:
        self.repo.mutate_job(
            "ci.yml",
            "cargo-download-cache",
            "      if: steps.cargo-downloads.outputs.cache-hit != 'true' && github.event_name == 'push'",
            "      if: github.event_name == 'pull_request' && steps.cargo-downloads.outputs.cache-hit != 'true' && github.event_name == 'push'",
        )
        self.assert_rejected("save must be executable only after a trusted-main cache miss")

    def test_comments_cannot_mask_missing_macos_producer(self) -> None:
        self.repo.mutate_job(
            "ci.yml",
            "cargo-download-cache",
            "        os: [ubuntu-24.04, macos-14]",
            "        # os: [ubuntu-24.04, macos-14]\n        os: [ubuntu-24.04]",
        )
        self.assert_rejected("active OS matrix must be [ubuntu-24.04, macos-14]")

    def test_discovers_variable_indirect_cargo_compilation(self) -> None:
        self.repo.append_job(
            "\n  indirect-compile:\n"
            "    runs-on: ubuntu-24.04\n"
            "    steps:\n"
            "    - run: |\n"
            "        runner=cargo\n"
            "        \"$runner\" test --all-targets\n"
        )
        self.assert_rejected("new compiling Cargo job is outside the explicit cache-consumer contract")

    def test_harmless_echo_of_cargo_text_is_not_a_compile(self) -> None:
        self.repo.append_job(
            "\n  harmless-text:\n"
            "    runs-on: ubuntu-24.04\n"
            "    steps:\n"
            "    - run: echo cargo test --all-targets\n"
        )
        result = self.repo.run()
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)

    def test_rejects_missing_cache_on_canonical_expensive_job(self) -> None:
        self.repo.mutate_job(
            "ci.yml",
            "build",
            "          ${{ runner.temp }}/finch-cargo-home/registry/cache\n",
            "",
        )
        self.assert_rejected("dependency restore paths must be exactly")

    def test_rejects_compile_before_dependency_restore(self) -> None:
        self.repo.mutate_job(
            "ci.yml",
            "test",
            "    - name: Resolve exact Cargo dependency graph\n",
            "    - name: Premature compile\n      run: cargo test --no-run\n\n"
            "    - name: Resolve exact Cargo dependency graph\n",
        )
        self.assert_rejected("then run the first Cargo compile")

    def test_rejects_key_without_both_cargo_config_names(self) -> None:
        self.repo.mutate_job("ci.yml", "test", ", '.cargo/config.toml'", "")
        self.assert_rejected("missing an exact compatible lock identity")

    def test_rejects_target_tree_in_dependency_archive(self) -> None:
        self.repo.mutate_job(
            "ci.yml",
            "test",
            "          ${{ runner.temp }}/finch-cargo-home/git/db\n",
            "          ${{ runner.temp }}/finch-cargo-home/git/db\n          target\n",
        )
        self.assert_rejected("dependency restore paths must be exactly")

    def test_rejects_target_tree_in_dependency_save(self) -> None:
        self.repo.mutate_job(
            "ci.yml",
            "cargo-download-cache",
            "        key: ${{ steps.cargo-downloads.outputs.cache-primary-key }}",
            "        path: target\n        key: ${{ steps.cargo-downloads.outputs.cache-primary-key }}",
        )
        self.assert_rejected("save paths must be exactly")

    def test_rejects_mutable_sccache_action(self) -> None:
        self.repo.replace(
            ".github/workflows/ci.yml",
            "mozilla-actions/sccache-action@fc920bf0ec8de6ee65d409111f7ec508035751ba",
            "mozilla-actions/sccache-action@v0.0.11",
        )
        self.assert_rejected("not an approved full-SHA-pinned cache action")

    def test_rejects_disabled_sccache_setup(self) -> None:
        self.repo.mutate_job(
            "ci.yml",
            "runtime-authority",
            "    - name: Install optional quota-LRU sccache\n",
            "    - name: Install optional quota-LRU sccache\n      if: false\n",
        )
        self.assert_rejected("sccache setup is disabled")

    def test_rejects_extra_no_default_feature_writer_lane(self) -> None:
        self.repo.mutate_job(
            "ci.yml",
            "test",
            "            feature_name: no-default-features\n            cargo_args: --no-default-features\n            sccache_writer: false",
            "            feature_name: no-default-features\n            cargo_args: --no-default-features\n            sccache_writer: true",
        )
        self.assert_rejected("two trusted writer lanes to default features")

    def test_rejects_feature_name_not_bound_to_cargo_arguments(self) -> None:
        self.repo.mutate_job(
            "ci.yml",
            "test",
            "            feature_name: no-default-features\n            cargo_args: --no-default-features",
            '            feature_name: no-default-features\n            cargo_args: ""',
        )
        self.assert_rejected("two trusted writer lanes to default features")

    def test_rejects_release_platform_hidden_in_comment(self) -> None:
        self.repo.mutate_job(
            "release.yml",
            "build-release",
            "          - os: macos-14\n            target: aarch64-apple-darwin\n            asset_name: finch-macos-arm64",
            "          # - os: macos-14\n"
            "          #   target: aarch64-apple-darwin\n"
            "          #   asset_name: finch-macos-arm64",
        )
        self.assert_rejected("active release matrix must bind each supported OS")

    def test_rejects_release_compiler_cache_writer(self) -> None:
        self.repo.mutate_job(
            "release.yml",
            "build-release",
            '          FINCH_SCCACHE_WRITER: "false"',
            '          FINCH_SCCACHE_WRITER: "true"',
        )
        self.assert_rejected('compiler-cache writer marker must be "false"')

    def test_rejects_sccache_without_incremental_disabled(self) -> None:
        self.repo.replace(
            "scripts/configure_ci_sccache.sh", '  echo "CARGO_INCREMENTAL=0"\n', ""
        )
        self.assert_rejected("CARGO_INCREMENTAL=0")

    def test_rejects_sccache_helper_without_pull_request_default_read_only(self) -> None:
        self.repo.replace(
            "scripts/configure_ci_sccache.sh", "cache_mode=READ_ONLY", "cache_mode=READ_WRITE"
        )
        self.assert_rejected("cache_mode=READ_ONLY")

    def test_rejects_dependency_cache_failure_as_fatal(self) -> None:
        self.repo.mutate_job(
            "ci.yml", "test", "      continue-on-error: true", "      continue-on-error: false"
        )
        self.assert_rejected("dependency restore must be nonfatal")

    def test_rejects_direct_sccache_authority_override(self) -> None:
        self.repo.mutate_job(
            "ci.yml",
            "test",
            "      env:\n        FINCH_SCCACHE_WRITER:",
            "      env:\n        SCCACHE_GHA_RW_MODE: READ_WRITE\n        FINCH_SCCACHE_WRITER:",
        )
        self.assert_rejected("sccache authority must come only from configure_ci_sccache.sh")

    def test_rejects_extra_ci_job_write_permission(self) -> None:
        self.repo.mutate_job(
            "ci.yml",
            "build",
            "    runs-on: ${{ matrix.os }}\n",
            "    runs-on: ${{ matrix.os }}\n    permissions:\n      contents: write\n",
        )
        self.assert_rejected("must not elevate token permissions")

    def test_rejects_release_build_write_permission(self) -> None:
        self.repo.mutate_job(
            "release.yml",
            "build-release",
            "    runs-on: ${{ matrix.os }}\n",
            "    runs-on: ${{ matrix.os }}\n    permissions:\n      contents: write\n",
        )
        self.assert_rejected("must not receive release write authority")

    def test_rejects_cargo_home_derived_from_home(self) -> None:
        self.repo.replace(
            "scripts/configure_ci_cargo_home.sh",
            'cargo_home="${RUNNER_TEMP}/finch-cargo-home"',
            'cargo_home="${HOME}/.cargo"',
        )
        self.assert_rejected("missing isolated-home contract")

    def test_rejects_unpinned_cargo_audit_install(self) -> None:
        self.repo.mutate_job(
            "ci.yml",
            "security",
            "cargo install cargo-audit --version 0.22.2 --locked",
            "cargo install cargo-audit --locked",
        )
        self.assert_rejected("needs one pinned cargo-audit restore/install/save path")


class CacheAuthorityHelperTests(unittest.TestCase):
    def run_sccache_helper(
        self, *, event: str, ref: str, writer: str, available: bool = True
    ) -> tuple[subprocess.CompletedProcess[str], dict[str, str]]:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            github_env = root / "github-env"
            binary = root / "sccache"
            if available:
                binary.write_text("#!/bin/sh\nexit 0\n", encoding="utf-8")
                binary.chmod(0o755)
            env = os.environ.copy()
            env.update(
                {
                    "SCCACHE_PATH": str(binary),
                    "GITHUB_ENV": str(github_env),
                    "GITHUB_EVENT_NAME": event,
                    "GITHUB_REF": ref,
                    "FINCH_SCCACHE_WRITER": writer,
                }
            )
            result = subprocess.run(
                [str(ROOT / "scripts/configure_ci_sccache.sh")],
                env=env,
                check=False,
                capture_output=True,
                text=True,
            )
            values = {}
            if github_env.exists():
                values = dict(
                    line.split("=", 1)
                    for line in github_env.read_text(encoding="utf-8").splitlines()
                )
            return result, values

    def test_pull_request_writer_lane_is_actually_read_only(self) -> None:
        result, values = self.run_sccache_helper(
            event="pull_request", ref="refs/pull/384/merge", writer="true"
        )
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertEqual(values["SCCACHE_GHA_RW_MODE"], "READ_ONLY", values)
        self.assertEqual(values["CARGO_INCREMENTAL"], "0", values)

    def test_trusted_main_writer_lane_is_read_write(self) -> None:
        result, values = self.run_sccache_helper(
            event="push", ref="refs/heads/main", writer="true"
        )
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertEqual(values["SCCACHE_GHA_RW_MODE"], "READ_WRITE", values)

    def test_nonwriter_main_lane_is_read_only(self) -> None:
        result, values = self.run_sccache_helper(
            event="push", ref="refs/heads/main", writer="false"
        )
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertEqual(values["SCCACHE_GHA_RW_MODE"], "READ_ONLY", values)

    def test_writer_lane_on_non_main_branch_is_read_only(self) -> None:
        result, values = self.run_sccache_helper(
            event="push", ref="refs/heads/feature", writer="true"
        )
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertEqual(values["SCCACHE_GHA_RW_MODE"], "READ_ONLY", values)

    def test_missing_sccache_preserves_ordinary_compilation(self) -> None:
        result, values = self.run_sccache_helper(
            event="push", ref="refs/heads/main", writer="true", available=False
        )
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertEqual(values, {}, values)
        self.assertIn("continuing with ordinary rustc compilation", result.stdout)

    def test_cargo_home_is_scoped_to_runner_temp_and_adds_bin_path(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            github_env = root / "github-env"
            github_path = root / "github-path"
            runner_temp = root / "runner-temp"
            runner_temp.mkdir()
            env = os.environ.copy()
            env.update(
                {
                    "RUNNER_TEMP": str(runner_temp),
                    "GITHUB_ENV": str(github_env),
                    "GITHUB_PATH": str(github_path),
                }
            )
            result = subprocess.run(
                [str(ROOT / "scripts/configure_ci_cargo_home.sh")],
                env=env,
                check=False,
                capture_output=True,
                text=True,
            )
            expected = runner_temp / "finch-cargo-home"
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
            self.assertEqual(github_env.read_text().strip(), f"CARGO_HOME={expected}")
            self.assertEqual(github_path.read_text().strip(), str(expected / "bin"))
            self.assertTrue((expected / "bin").is_dir(), expected)


if __name__ == "__main__":
    unittest.main()
