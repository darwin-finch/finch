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
    ESCAPE_API_ALLOWLIST,
    EXPECTED_PATHS,
    ISOLATION_WORKFLOW,
    MAIN_SAVE_IF,
    escape_api_errors,
    event_contract,
    load_yaml,
    workflow_activates,
)

MACOS_JOB = "  isolation-boundaries-macos:\n"


class Repository:
    def __init__(self) -> None:
        self.temporary = tempfile.TemporaryDirectory()
        self.root = Path(self.temporary.name)
        shutil.copytree(ROOT / ".github/workflows", self.root / ".github/workflows")
        # Minimal sources holding exactly the allowlisted escape-API uses; the real tree is
        # covered by test_real_tree_escape_api_uses_match_the_allowlist.
        for entry in ESCAPE_API_ALLOWLIST:
            relative, line = entry.split(":", 1)
            path = self.root / relative
            path.parent.mkdir(parents=True, exist_ok=True)
            with path.open("a") as source:
                source.write(f"    {line}\n")

    def close(self) -> None:
        self.temporary.cleanup()

    def workflow(self, name: str) -> Path:
        return self.root / ".github/workflows" / name

    def replace(self, name: str, old: str, new: str, after: str | None = None) -> None:
        """Replace the first occurrence of old, or the first one following the after anchor."""
        path = self.workflow(name)
        contents = path.read_text()
        start = 0
        if after is not None:
            if after not in contents:
                raise AssertionError(f"fixture anchor missing from {name}: {after!r}")
            start = contents.index(after)
        if old not in contents[start:]:
            raise AssertionError(f"fixture mutation target missing from {name}: {old!r}")
        path.write_text(contents[:start] + contents[start:].replace(old, new, 1))

    def write_source(self, relative: str, text: str) -> None:
        path = self.root / relative
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(text)

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

    def test_every_contracted_path_activates_pull_request_and_push(self) -> None:
        ordinary = {
            "issue-201-chatgpt-auth.yml": ("src/models/mod.rs",),
            ISOLATION_WORKFLOW: (
                "src/models/mod.rs", "src/cli/query.rs", "src/cli/tui/mod.rs",
                "src/providers/anthropic.rs", "src/lib.rs", "README.md",
            ),
        }
        for workflow, idle_paths in ordinary.items():
            document = load_yaml(ROOT / ".github/workflows" / workflow)
            for event in ("pull_request", "push"):
                contract = event_contract(document, workflow, event)
                self.assertIsInstance(contract, dict, f"{workflow} {event} contract must be path-filtered")
                for path in EXPECTED_PATHS[workflow] or ():
                    self.assertTrue(
                        workflow_activates(contract, (path.replace("**", "probe"),)),
                        f"{workflow} {event} must activate for contracted path {path}",
                    )
                for path in idle_paths:
                    self.assertFalse(
                        workflow_activates(contract, (path,)),
                        f"{workflow} {event} must stay idle for ordinary path {path}",
                    )

    def test_isolation_triggers_are_bound(self) -> None:
        mutations = (
            # Only the second (push) occurrence changes, so PR and push paths diverge.
            (
                "      - 'docs/DEVELOPMENT.md'\n", "", "  push:\n",
                "push.paths changed", "docs/DEVELOPMENT.md",
            ),
            ("      - 'tests/README.md'\n", "", None, "pull_request.paths changed", "tests/README.md"),
            ("    - cron: '0 9 * * 1'\n", "    - cron: '0 9 * * *'\n", None, "weekly schedule changed"),
            ("  schedule:\n    - cron: '0 9 * * 1'\n", "", None, "weekly schedule changed"),
            ("  workflow_dispatch:\n", "", None, "manual workflow_dispatch trigger is required"),
        )
        for old, new, after, *diagnostics in mutations:
            with self.subTest(diagnostics=diagnostics):
                repository = Repository()
                try:
                    repository.replace(ISOLATION_WORKFLOW, old, new, after=after)
                    result = repository.check()
                    self.assertNotEqual(0, result.returncode, f"isolation trigger mutant passed: {diagnostics}")
                    for diagnostic in diagnostics:
                        self.assertIn(diagnostic, result.stderr, result.stderr)
                finally:
                    repository.close()

    def test_both_isolation_platforms_run_the_complete_fatal_sequence(self) -> None:
        harness = "    - name: Exercise the complete synthetic isolation harness\n"
        mutations = (
            ("    runs-on: macos-14\n", "    runs-on: ubuntu-24.04\n", MACOS_JOB,
             "owner job 'isolation-boundaries-macos' must run actively on macos-14"),
            ("    runs-on: macos-14\n", "    if: false\n    runs-on: macos-14\n", MACOS_JOB,
             "owner job 'isolation-boundaries-macos' must run actively"),
            ("    runs-on: macos-14\n", "    continue-on-error: true\n    runs-on: macos-14\n", MACOS_JOB,
             "owner job 'isolation-boundaries-macos' must gate failure"),
            (harness + "      timeout-minutes: 15\n      run: ./scripts/test_brain_isolation.sh\n", "",
             MACOS_JOB, "'isolation-boundaries-macos' (macos-14) fatal step "
             "'Exercise the complete synthetic isolation harness' must occur exactly once"),
            (harness, harness + "      if: false\n", MACOS_JOB, "must run unconditionally"),
            (harness, harness + "      continue-on-error: true\n", MACOS_JOB, "must gate failure"),
            ("tests/worker_node_isolation_contract.sh\n", "true\n", MACOS_JOB,
             "'Keep worker node identity inside disposable state' commands changed"),
            ("        ./scripts/test_brains.sh cargo test --lib brain::store -- --nocapture\n", "", None,
             "'isolation-boundaries' (ubuntu-24.04) fatal step", "commands changed"),
            ("  isolation-boundaries-macos:\n", "  isolation-boundaries-mac:\n", None,
             "isolation job inventory changed",
             "required owner job 'isolation-boundaries-macos' is missing"),
        )
        for old, new, after, *diagnostics in mutations:
            with self.subTest(diagnostics=diagnostics):
                repository = Repository()
                try:
                    repository.replace(ISOLATION_WORKFLOW, old, new, after=after)
                    result = repository.check()
                    self.assertNotEqual(0, result.returncode, f"isolation platform mutant passed: {diagnostics}")
                    for diagnostic in diagnostics:
                        self.assertIn(diagnostic, result.stderr, result.stderr)
                finally:
                    repository.close()

    def test_isolation_fatal_steps_cannot_be_rewritten_by_shell_directory_or_defaults(self) -> None:
        harness = "    - name: Exercise the complete synthetic isolation harness\n"
        mutations = (
            (harness, harness + "      shell: true {0}\n", MACOS_JOB,
             "'isolation-boundaries-macos' (macos-14) fatal step 'Exercise the complete synthetic isolation harness' must use the default shell"),
            (harness, harness + "      working-directory: /tmp\n", None,
             "'isolation-boundaries' (ubuntu-24.04) fatal step 'Exercise the complete synthetic isolation harness' must use the default working-directory"),
            ("    runs-on: macos-14\n", "    runs-on: macos-14\n    defaults:\n      run:\n        shell: true {0}\n", MACOS_JOB,
             "job 'isolation-boundaries-macos' (macos-14) defaults would rewrite its fatal steps"),
            ("    runs-on: ubuntu-24.04\n", "    runs-on: ubuntu-24.04\n    defaults:\n      run:\n        working-directory: /tmp\n", None,
             "job 'isolation-boundaries' (ubuntu-24.04) defaults would rewrite its fatal steps"),
            ("permissions:\n", "defaults:\n  run:\n    shell: true {0}\n\npermissions:\n", None,
             "workflow-level defaults would rewrite every fatal step"),
        )
        for old, new, after, diagnostic in mutations:
            with self.subTest(diagnostic=diagnostic):
                repository = Repository()
                try:
                    repository.replace(ISOLATION_WORKFLOW, old, new, after=after)
                    result = repository.check()
                    self.assertNotEqual(0, result.returncode, f"isolation rewrite mutant passed: {diagnostic}")
                    self.assertIn(diagnostic, result.stderr, result.stderr)
                finally:
                    repository.close()

    def test_isolation_fatal_steps_must_keep_their_order(self) -> None:
        path = self.repository.workflow(ISOLATION_WORKFLOW)
        source = path.read_text()
        macos = source.index(MACOS_JOB)
        start = source.index("    - name: Exercise the complete synthetic isolation harness\n", macos)
        harness = source[start:]
        source = source[:start]
        insertion = source.index("    - name: Check bins and tests\n", macos)
        path.write_text(source[:insertion] + harness + "\n" + source[insertion:])
        self.assert_fails("'isolation-boundaries-macos' (macos-14) fatal steps are out of order")

    def test_macos_isolation_cache_is_a_restore_only_consumer_of_the_macos_family(self) -> None:
        mutations = (
            ("        save-if: false\n", f"        save-if: {MAIN_SAVE_IF}\n",
             "'isolation-boundaries-macos' cache inputs changed"),
            ("apple-release-default-${{", "apple-release-isolation-${{",
             "'isolation-boundaries-macos' cache inputs changed",
             "Cargo cache identity set changed; expected 6 identities across 7 locations",
             "apple-release-isolation"),
            ('      CARGO_PROFILE_RELEASE_CODEGEN_UNITS: "16"\n', "",
             "'isolation-boundaries-macos' effective Cargo/Rust environment changed"),
            ("      CARGO_PROFILE_RELEASE_LTO: \"false\"\n",
             "      CARGO_PROFILE_RELEASE_LTO: \"false\"\n      CARGO_PROFILE_TEST_DEBUG: 0\n",
             "'isolation-boundaries-macos' effective Cargo/Rust environment changed"),
            ("      continue-on-error: true\n      with:\n",
             "      continue-on-error: true\n      env:\n        CARGO_INCREMENTAL: 0\n      with:\n",
             "'isolation-boundaries-macos' must not override action-hashed Rust/Cargo"),
        )
        for old, new, *diagnostics in mutations:
            with self.subTest(diagnostics=diagnostics):
                repository = Repository()
                try:
                    repository.replace(ISOLATION_WORKFLOW, old, new, after=MACOS_JOB)
                    result = repository.check()
                    self.assertNotEqual(0, result.returncode, f"macOS isolation cache mutant passed: {diagnostics}")
                    for diagnostic in diagnostics:
                        self.assertIn(diagnostic, result.stderr, result.stderr)
                finally:
                    repository.close()

    def test_ubuntu_isolation_environment_stays_bound_to_its_family(self) -> None:
        self.repository.replace(ISOLATION_WORKFLOW, "      CARGO_PROFILE_TEST_DEBUG: 0\n", "")
        self.assert_fails("'isolation-boundaries' effective Cargo/Rust environment changed")

    def test_supervised_commands_cannot_return_to_ordinary_ci(self) -> None:
        moved = (
            "    - name: Exercise the complete synthetic isolation harness on macOS\n"
            "      if: runner.os == 'macOS'\n"
            "      run: ./scripts/test_brain_isolation.sh\n\n"
        )
        self.repository.replace(
            "ci.yml", "    - name: Warm macOS isolation supervisor cache on trusted main\n",
            moved + "    - name: Warm macOS isolation supervisor cache on trusted main\n",
        )
        self.assert_fails(
            "runs supervised isolation work in ordinary CI",
            "Exercise the complete synthetic isolation harness on macOS",
        )

    def test_supervisor_cache_warm_stays_trusted_main_only(self) -> None:
        mutations = (
            ("      if: github.event_name == 'push' && runner.os == 'macOS'\n      run: |\n        cargo test --bin finch-test-supervisor",
             "      if: runner.os == 'macOS'\n      run: |\n        cargo test --bin finch-test-supervisor",
             "Warm macOS isolation supervisor cache on trusted main", "condition changed"),
            ("        cargo build --release --bin finch-test-supervisor\n",
             "        cargo build --release --bin finch-test-supervisor\n        ./scripts/test_brain_isolation.sh\n",
             "Warm macOS isolation supervisor cache on trusted main", "commands changed"),
        )
        for old, new, *diagnostics in mutations:
            with self.subTest(diagnostics=diagnostics):
                repository = Repository()
                try:
                    repository.replace("ci.yml", old, new)
                    result = repository.check()
                    self.assertNotEqual(0, result.returncode, f"supervisor warm mutant passed: {diagnostics}")
                    for diagnostic in diagnostics:
                        self.assertIn(diagnostic, result.stderr, result.stderr)
                finally:
                    repository.close()

    def test_escape_api_in_ordinary_source_fails_without_activating_isolation(self) -> None:
        ordinary = "src/cli/tui/mod.rs"
        contract = event_contract(
            load_yaml(ROOT / ".github/workflows" / ISOLATION_WORKFLOW), ISOLATION_WORKFLOW, "pull_request",
        )
        self.assertFalse(
            workflow_activates(contract, (ordinary,)),
            f"{ordinary} must not activate supervised isolation; the always-on scan must catch it",
        )
        self.repository.write_source(ordinary, "fn detach() {\n    unsafe { nix::libc::setsid() };\n}\n")
        self.assert_fails("escape-API allowlist changed", "src/cli/tui/mod.rs:unsafe { nix::libc::setsid() };")

    def test_escape_api_scan_covers_shell_job_control_and_every_root(self) -> None:
        cases = (
            ("scripts/new_launcher.sh", "set -m\n", "scripts/new_launcher.sh:set -m"),
            ("tests/new_test.rs", "cmd.process_group(0);\n", "tests/new_test.rs:cmd.process_group(0);"),
            ("src/node/spawn.rs", "libc::setpgid(0, 0);\n", "src/node/spawn.rs:libc::setpgid(0, 0);"),
        )
        for relative, text, diagnostic in cases:
            with self.subTest(relative=relative):
                repository = Repository()
                try:
                    repository.write_source(relative, text)
                    result = repository.check()
                    self.assertNotEqual(0, result.returncode, f"unauthorized escape API passed in {relative}")
                    self.assertIn(diagnostic, result.stderr, result.stderr)
                finally:
                    repository.close()

    def test_escape_api_allowlist_rejects_missing_or_changed_uses(self) -> None:
        self.repository.write_source("src/daemon/spawn.rs", "fn spawn() {}\n")
        self.assert_fails("missing_allowlisted=", "src/daemon/spawn.rs:if nix::libc::setsid() == -1 {")
        self.repository.write_source("src/daemon/spawn.rs", "    if nix::libc::setsid() != 0 {\n")
        self.assert_fails(
            "unauthorized=['src/daemon/spawn.rs:if nix::libc::setsid() != 0 {']",
            "src/daemon/spawn.rs:if nix::libc::setsid() == -1 {",
        )

    def test_escape_api_scan_keeps_harness_self_exclusion_and_ignores_other_files(self) -> None:
        self.repository.write_source("scripts/test_brain_isolation.sh", "rg 'setsid|setpgid'\nset -m\n")
        self.repository.write_source("src/notes.md", "setsid\n")
        self.repository.write_source("src/reset_id.rs", "let preset_idle = subset -mode;\n")
        self.assert_passes()

    def test_real_tree_escape_api_uses_match_the_allowlist(self) -> None:
        self.assertEqual([], escape_api_errors(ROOT), "current tree escape-API uses diverge from the allowlist")

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
            (
                "      continue-on-error: true\n      with:\n        cache-provider: github",
                "      continue-on-error: true\n      if: false\n      with:\n        cache-provider: github",
                "cache step must run actively",
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
        for action in ("actions/cache@v4", "mozilla-actions/sccache-action@v0.0.9"):
            with self.subTest(action=action):
                repository = Repository()
                try:
                    repository.replace(
                        "issue-201-chatgpt-auth.yml",
                        "      - name: Compile exact authentication sources on Windows\n",
                        f"      - uses: {action}\n"
                        "      - name: Compile exact authentication sources on Windows\n",
                    )
                    result = repository.check()
                    self.assertNotEqual(0, result.returncode, "alternate cache action passed")
                    self.assertIn("Cargo cache allocation changed", result.stderr, result.stderr)
                    self.assertIn("windows-verifier-compile", result.stderr, result.stderr)
                finally:
                    repository.close()

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

    def test_redundant_add_job_id_key_false_remains_optional(self) -> None:
        self.repository.replace(
            "ci.yml", "        cache-provider: github\n",
            "        cache-provider: github\n        add-job-id-key: false\n",
        )
        self.assert_passes()

    def test_cargo_audit_version_and_miss_install_are_bound(self) -> None:
        self.repository.replace(
            "ci.yml", "cargo install cargo-audit --version 0.22.2 --locked",
            "cargo install cargo-audit --locked",
        )
        self.assert_fails("Install cargo-audit 0.22.2 on cache miss", "commands changed")

    def test_cargo_audit_version_check_uses_direct_binary(self) -> None:
        self.repository.replace(
            "ci.yml",
            'test "$(cargo-audit --version)" = "cargo-audit 0.22.2"',
            'test "$(cargo audit --version)" = "cargo-audit 0.22.2"',
        )
        self.assert_fails("Verify cargo-audit 0.22.2", "commands changed")

    def test_malformed_yaml_fails_actionably(self) -> None:
        self.repository.workflow("docs.yml").write_text("jobs: [\n")
        self.assert_fails("docs.yml: invalid workflow YAML")


if __name__ == "__main__":
    unittest.main()
