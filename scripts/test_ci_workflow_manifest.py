#!/usr/bin/env python3
"""Production-boundary regressions for the semantic workflow checker."""

from __future__ import annotations

import shutil
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
CHECKER = ROOT / "scripts/check_ci_workflow_manifest.py"
sys.path.insert(0, str(ROOT / "scripts"))
from check_ci_workflow_manifest import (  # noqa: E402
    EXPECTED_PATHS,
    event_contract,
    load_yaml,
    workflow_activates,
)


class Repository:
    def __init__(self) -> None:
        self.temporary = tempfile.TemporaryDirectory()
        self.root = Path(self.temporary.name)
        shutil.copytree(ROOT / ".github/workflows", self.root / ".github/workflows")

    def close(self) -> None:
        self.temporary.cleanup()

    def workflow(self, name: str) -> Path:
        return self.root / ".github/workflows" / name

    def replace(self, name: str, old: str, new: str) -> None:
        path = self.workflow(name)
        contents = path.read_text()
        if old not in contents:
            raise AssertionError(f"fixture mutation target missing from {name}: {old!r}")
        path.write_text(contents.replace(old, new, 1))

    def check(self) -> subprocess.CompletedProcess[str]:
        return subprocess.run(
            [sys.executable, str(CHECKER), "--root", str(self.root)],
            text=True, capture_output=True, check=False,
        )


class WorkflowContractTests(unittest.TestCase):
    def setUp(self) -> None:
        self.repository = Repository()

    def tearDown(self) -> None:
        self.repository.close()

    def assert_passes(self) -> None:
        result = self.repository.check()
        self.assertEqual(0, result.returncode, f"semantic workflow check should pass: {result.stderr}")

    def assert_fails(self, *diagnostics: str) -> None:
        result = self.repository.check()
        self.assertNotEqual(0, result.returncode, "mutated workflow contract unexpectedly passed")
        for diagnostic in diagnostics:
            self.assertIn(diagnostic, result.stderr, f"missing actionable diagnostic in: {result.stderr}")

    def test_current_workflows_pass(self) -> None:
        self.assert_passes()

    def test_comment_and_format_only_edits_pass_without_refresh(self) -> None:
        self.repository.replace("ci.yml", "jobs:\n", "# harmless review comment\n\njobs:\n")
        self.repository.replace("ci.yml", "branches: [ main ]", "branches:\n      - main")
        self.assert_passes()

    def test_workflow_inventory_addition_and_removal_fail(self) -> None:
        shutil.copy2(self.repository.workflow("docs.yml"), self.repository.workflow("surprise.yml"))
        self.assert_fails("workflow inventory changed", "surprise.yml")
        self.repository.workflow("surprise.yml").unlink()
        self.repository.workflow("release.yml").unlink()
        self.assert_fails("workflow inventory changed", "release.yml")

    def test_path_filter_drift_fails(self) -> None:
        self.repository.replace(
            "issue-201-chatgpt-auth.yml",
            '      - "src/providers/mod.rs"\n',
            '      - "src/providers/mod.rs"\n      - "src/new/**"\n',
        )
        self.assert_fails("issue-201-chatgpt-auth.yml", "paths changed", "src/new/**")

    def test_path_order_is_not_behavior_but_duplicates_fail(self) -> None:
        pair = '      - "Cargo.toml"\n      - "src/lib.rs"\n'
        self.repository.replace(
            "issue-201-chatgpt-auth.yml", pair,
            '      - "src/lib.rs"\n      - "Cargo.toml"\n',
        )
        self.assert_passes()
        self.repository.replace(
            "issue-201-chatgpt-auth.yml", '      - "src/oauth/**"\n',
            '      - "src/oauth/**"\n      - "src/oauth/**"\n',
        )
        self.assert_fails("on.push.paths contains duplicates")

    def test_negative_path_filters_are_applied_per_changed_path(self) -> None:
        self.assertTrue(
            workflow_activates(
                {"paths": ("docs/**", "!docs/generated/**")},
                ("docs/generated/index.md", "docs/guide.md"),
            ),
            "one excluded file must not hide a different included changed path",
        )

    def test_every_auth_path_activates_pull_request_and_push(self) -> None:
        document = load_yaml(ROOT / ".github/workflows/issue-201-chatgpt-auth.yml")
        for event in ("pull_request", "push"):
            contract = event_contract(document, "issue-201-chatgpt-auth.yml", event)
            self.assertIsInstance(contract, dict, f"{event} contract must be path-filtered")
            for path in EXPECTED_PATHS["issue-201-chatgpt-auth.yml"] or ():
                self.assertTrue(
                    workflow_activates(contract, (path.replace("**", "probe"),)),
                    f"{event} must activate for contracted path {path}",
                )
            self.assertFalse(
                workflow_activates(contract, ("src/models/mod.rs",)),
                f"{event} must stay idle for an ordinary non-auth path",
            )

    def test_branch_filter_drift_fails_actionably(self) -> None:
        self.repository.replace(
            "ci.yml", "  pull_request:\n    branches: [ main ]",
            "  pull_request:\n    branches: [ release ]",
        )
        self.assert_fails("ci.yml: pull_request.branches changed", "expected=('main',)", "actual=('release',)")

    def test_pull_request_type_drift_fails_actionably(self) -> None:
        self.repository.replace(
            "ci.yml", "  pull_request:\n    branches: [ main ]",
            "  pull_request:\n    branches: [ main ]\n    types: [closed]",
        )
        self.assert_fails("ci.yml: pull_request.types changed", "expected=None", "actual=('closed',)")

    def test_conflicting_branch_filters_fail_actionably(self) -> None:
        self.repository.replace(
            "ci.yml", "  pull_request:\n    branches: [ main ]",
            "  pull_request:\n    branches: [ main ]\n    branches-ignore: [ release ]",
        )
        self.assert_fails("ci.yml: on.pull_request cannot combine branches and branches-ignore")

    def test_pull_request_target_fails_closed(self) -> None:
        self.repository.replace("release.yml", "  push:\n", "  pull_request_target:\n\n  push:\n")
        self.assert_fails("release.yml: on.pull_request_target is unsupported")

    def test_psych_key_conflation_and_duplicates_fail_before_conversion(self) -> None:
        mutations = (
            ("ci.yml", "on:\n", '"true":\n', "missing the actual 'on' key"),
            ("ci.yml", "on:\n", "on: {}\non:\n", 'duplicate YAML key "on"'),
            ("docs.yml", "jobs:\n", "jobs: {}\njobs:\n", 'duplicate YAML key "jobs"'),
            ("docs.yml", "  current-docs:\n", "  current-docs: {}\n  current-docs:\n", 'duplicate YAML key "current-docs"'),
            ("docs.yml", "    name: Current docs", "    name: duplicate\n    name: Current docs", 'duplicate YAML key "name"'),
            ("docs.yml", "  pull_request:\n", "  pull_request: {}\n  pull_request:\n", 'duplicate YAML key "pull_request"'),
            ("docs.yml", "    paths:\n", "    paths: []\n    paths:\n", 'duplicate YAML key "paths"'),
        )
        for name, old, new, diagnostic in mutations:
            with self.subTest(diagnostic=diagnostic):
                repository = Repository()
                try:
                    repository.replace(name, old, new)
                    result = repository.check()
                    self.assertNotEqual(0, result.returncode, f"duplicate-key mutant passed: {diagnostic}")
                    self.assertIn(diagnostic, result.stderr, f"diagnostic was not actionable: {result.stderr}")
                finally:
                    repository.close()

    def test_job_name_drift_reports_missing_and_unexpected_checks(self) -> None:
        self.repository.replace("docs.yml", "Current docs links, claims, and shell syntax", "Renamed docs check")
        self.assert_fails(
            "docs.yml: expanded check allocation changed", "fixture 'readme_only'",
            "Current docs links, claims, and shell syntax", "Renamed docs check",
        )

    def test_matrix_fanout_drift_is_caught_outside_representative_fixtures(self) -> None:
        self.repository.replace(
            "ci.yml", "          - os: macos-14\n",
            "          - os: windows-2025\n            feature_name: default\n            cargo_args: \"\"\n            timeout_minutes: 45\n          - os: macos-14\n",
        )
        self.assert_fails("ci.yml: expanded check allocation changed", "Test (windows-2025, default)")

    def test_migrated_preflights_must_run_before_cargo_setup(self) -> None:
        path = self.repository.workflow("ci.yml")
        source = path.read_text()
        start = source.index("    - name: Verify shared skill discovery\n")
        end = source.index("    - name: Install capnproto (Ubuntu)\n", start)
        preflights = source[start:end]
        source = source[:start] + source[end:]
        insertion = source.index(
            "    - name: Restore compatible Cargo dependencies and build artifacts\n"
        )
        path.write_text(source[:insertion] + preflights + source[insertion:])
        self.assert_fails(
            "ci.yml: job 'test' preflight steps must precede "
            "'Install repository Rust toolchain'",
            "Verify shared skill discovery", "Check and exercise the Cargo slot",
        )

    def test_duplicate_expanded_check_names_fail_actionably(self) -> None:
        self.repository.replace(
            "ci.yml", "          - os: macos-14\n",
            "          - os: ubuntu-24.04\n            feature_name: default\n            cargo_args: \"\"\n            timeout_minutes: 45\n          - os: macos-14\n",
        )
        self.assert_fails("ci.yml: duplicate expanded check names", "Test (ubuntu-24.04, default)")

    def test_unsupported_allocation_syntax_fails_actionably(self) -> None:
        self.repository.replace(
            "ci.yml", "      matrix:\n        include:",
            "      matrix:\n        exclude: []\n        include:",
        )
        self.assert_fails("ci.yml", "unsupported matrix.exclude")

    def test_migrated_canonical_boundaries_reject_inert_or_changed_steps(self) -> None:
        mutations = (
            (
                "cargo test --doc -- ValidatedProviderRequest",
                "cargo test --lib -- ValidatedProviderRequest",
                "Prove validated request tokens cannot be forged", "commands changed",
            ),
            (
                "cargo test --release --lib cli::conversation::tests -- --nocapture",
                "cargo test --lib cli::conversation::tests -- --nocapture",
                "Run release-mode atomic history regression", "commands changed",
            ),
            (
                "      if: matrix.feature_name == 'default'\n      shell: bash\n      run: |\n        test -L",
                "      if: false\n      shell: bash\n      run: |\n        test -L",
                "Verify shared skill discovery", "condition changed",
            ),
            (
                "        .agents/skills/finch-backlog/scripts/test-with-cargo-slot\n",
                "        .agents/skills/finch-backlog/scripts/with-cargo-slot\n",
                "Check and exercise the Cargo slot", "commands changed",
            ),
        )
        for old, new, *diagnostics in mutations:
            with self.subTest(diagnostics=diagnostics):
                repository = Repository()
                try:
                    repository.replace("ci.yml", old, new)
                    result = repository.check()
                    self.assertNotEqual(0, result.returncode, "migrated-boundary mutant passed")
                    for diagnostic in diagnostics:
                        self.assertIn(diagnostic, result.stderr, result.stderr)
                finally:
                    repository.close()

    def test_migrated_owner_jobs_must_be_active_and_gating(self) -> None:
        mutations = (
            (
                "ci.yml",
                "  test:\n    name: Test",
                "  test:\n    if: false\n    name: Test",
                "owner job 'test' must run actively",
            ),
            (
                "ci.yml",
                "  test:\n    name: Test",
                "  test:\n    continue-on-error: true\n    name: Test",
                "owner job 'test' must gate failure",
            ),
            (
                "issue-201-chatgpt-auth.yml",
                "  windows-verifier-compile:\n    runs-on:",
                "  windows-verifier-compile:\n    continue-on-error: true\n    runs-on:",
                "owner job 'windows-verifier-compile' must gate failure",
            ),
        )
        for name, old, new, diagnostic in mutations:
            with self.subTest(name=name, diagnostic=diagnostic):
                repository = Repository()
                try:
                    repository.replace(name, old, new)
                    result = repository.check()
                    self.assertNotEqual(0, result.returncode, "inactive/non-gating owner job passed")
                    self.assertIn(diagnostic, result.stderr, result.stderr)
                finally:
                    repository.close()

    def test_windows_boundary_rejects_wrong_owner_os_inertness_and_manifest(self) -> None:
        mutations = (
            ("runs-on: windows-2022", "runs-on: ubuntu-24.04", "must run actively on windows-2022"),
            ("    timeout-minutes: 30", "    if: false\n    timeout-minutes: 30", "must run actively"),
            (
                ".github/issue-105-windows-probe/Cargo.toml",
                ".github/issue-201-windows-probe/Cargo.toml",
                "commands changed",
            ),
        )
        for old, new, diagnostic in mutations:
            with self.subTest(diagnostic=diagnostic):
                repository = Repository()
                try:
                    repository.replace("issue-201-chatgpt-auth.yml", old, new)
                    result = repository.check()
                    self.assertNotEqual(0, result.returncode, "Windows-boundary mutant passed")
                    self.assertIn(diagnostic, result.stderr, result.stderr)
                finally:
                    repository.close()

    def test_migrated_commands_cannot_be_duplicated_or_made_nongating(self) -> None:
        duplicate = """\n    - name: Prove validated request tokens cannot be forged\n      run: cargo test --doc -- ValidatedProviderRequest\n"""
        self.repository.replace(
            "ci.yml", "\n  runtime-authority:\n", duplicate + "\n  runtime-authority:\n",
        )
        self.assert_fails(
            "must occur exactly once in ci.yml:test",
            "Prove validated request tokens cannot be forged",
        )

        repository = Repository()
        try:
            repository.replace(
                "ci.yml", "    - name: Run release-mode atomic history regression\n",
                "    - name: Run release-mode atomic history regression\n      continue-on-error: true\n",
            )
            result = repository.check()
            self.assertNotEqual(0, result.returncode, "non-gating migrated step passed")
            self.assertIn("must gate failure", result.stderr, result.stderr)
        finally:
            repository.close()

    def test_windows_push_trigger_and_both_probe_paths_are_required(self) -> None:
        self.repository.replace("issue-201-chatgpt-auth.yml", "  push:\n", "  deleted_push:\n")
        self.assert_fails("path-filtered push to main is required")

    def test_cache_pin_provider_mode_and_family_are_bound(self) -> None:
        mutations = (
            (
                "ci.yml",
                "Swatinem/rust-cache@6323deb102c322ba6fcbdcafc7e3dddab59af2b6",
                "Swatinem/rust-cache@v2",
                "must pin the reviewed rust-cache action",
            ),
            (
                "ci.yml", "cache-provider: github", "cache-provider: warpbuild",
                "cache inputs changed",
            ),
            (
                "ci.yml", "    cache-mode: read\n", "    cache-mode: write\n",
                "cache-mode changed",
            ),
            (
                "ci.yml",
                "debug-default_all-features-clippy_release-default",
                "debug-default",
                "test cache compatibility matrix changed",
            ),
        )
        for workflow, old, new, diagnostic in mutations:
            with self.subTest(workflow=workflow, diagnostic=diagnostic):
                repository = Repository()
                try:
                    repository.replace(workflow, old, new)
                    result = repository.check()
                    self.assertNotEqual(0, result.returncode, "unsafe cache configuration passed")
                    self.assertIn(diagnostic, result.stderr, result.stderr)
                finally:
                    repository.close()

    def test_cache_pruning_save_authority_and_nonfatality_are_bound(self) -> None:
        mutations = (
            (
                "save-if: ${{ github.event_name == 'push' && github.ref == 'refs/heads/main' }}",
                "save-if: true", "cache inputs changed",
            ),
            (
                "cache-workspace-crates: false", "cache-workspace-crates: true",
                "cache inputs changed",
            ),
            (
                "      continue-on-error: true\n      with:\n        cache-provider: github",
                "      with:\n        cache-provider: github",
                "cache failure must remain nonfatal",
            ),
        )
        for old, new, diagnostic in mutations:
            with self.subTest(diagnostic=diagnostic):
                repository = Repository()
                try:
                    repository.replace("ci.yml", old, new)
                    result = repository.check()
                    self.assertNotEqual(0, result.returncode, "unsafe cache behavior passed")
                    self.assertIn(diagnostic, result.stderr, result.stderr)
                finally:
                    repository.close()

    def test_cache_must_precede_the_first_owned_cargo_boundary(self) -> None:
        path = self.repository.workflow("ci.yml")
        source = path.read_text()
        start = source.index(
            "    - name: Restore compatible Cargo dependencies and build artifacts\n"
        )
        end = source.index(
            "    - name: Run clippy (binary only, warnings allowed for now)\n", start
        )
        cache = source[start:end]
        source = source[:start] + source[end:]
        insertion = source.index("    - name: Build binary\n")
        path.write_text(source[:insertion] + cache + source[insertion:])
        self.assert_fails("cache must run after the pinned toolchain", "Run clippy")

    def test_alternate_cache_action_and_release_permission_drift_fail(self) -> None:
        self.repository.replace(
            "issue-201-chatgpt-auth.yml",
            "      - name: Compile exact authentication sources on Windows\n",
            "      - uses: actions/cache@v4\n"
            "        with:\n"
            "          path: target\n"
            "          key: unsafe\n"
            "      - name: Compile exact authentication sources on Windows\n",
        )
        self.assert_fails("Cargo cache allocation changed", "windows-verifier-compile")

        repository = Repository()
        try:
            repository.replace(
                "release.yml", "    permissions:\n      contents: read\n",
                "    permissions:\n      contents: write\n",
            )
            result = repository.check()
            self.assertNotEqual(0, result.returncode, "release build write authority passed")
            self.assertIn("job 'build-release' permissions changed", result.stderr, result.stderr)
        finally:
            repository.close()

    def test_cargo_audit_version_and_miss_install_are_bound(self) -> None:
        self.repository.replace(
            "ci.yml", "cargo install cargo-audit --version 0.22.2 --locked",
            "cargo install cargo-audit --locked",
        )
        self.assert_fails("Install cargo-audit 0.22.2 on cache miss", "commands changed")

    def test_malformed_yaml_fails_actionably(self) -> None:
        self.repository.workflow("docs.yml").write_text("jobs: [\n")
        self.assert_fails("docs.yml: invalid workflow YAML")


if __name__ == "__main__":
    unittest.main()
