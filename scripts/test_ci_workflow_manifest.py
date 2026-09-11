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
from check_ci_workflow_manifest import workflow_activates  # noqa: E402


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
            "issue-187-subagent-fanout.yml",
            '      - "src/tools/implementations/spawn.rs"\n',
            '      - "src/tools/implementations/spawn.rs"\n      - "src/new/**"\n',
        )
        self.assert_fails("issue-187-subagent-fanout.yml: pull_request.paths changed", "src/new/**")

    def test_negative_path_filters_are_applied_per_changed_path(self) -> None:
        self.assertTrue(
            workflow_activates(
                {"paths": ("docs/**", "!docs/generated/**")},
                ("docs/generated/index.md", "docs/guide.md"),
            ),
            "one excluded file must not hide a different included changed path",
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
            "issue-187-subagent-fanout.yml", "os: [ubuntu-24.04, macos-14]",
            "os: [ubuntu-24.04, macos-14, windows-2022]",
        )
        self.assert_fails(
            "issue-187-subagent-fanout.yml: expanded check allocation changed",
            "Focused spawn tests (windows-2022)",
        )

    def test_cargo_slot_runs_on_exact_supported_linux_and_macos_images(self) -> None:
        self.repository.replace(
            "issue-245-cargo-slot.yml", "os: [ubuntu-24.04, macos-14]",
            "os: [ubuntu-latest, macos-latest]",
        )
        self.assert_fails(
            "issue-245-cargo-slot.yml: expanded check allocation changed",
            "macos-14 repository-wide lock", "macos-latest repository-wide lock",
            "ubuntu-24.04 repository-wide lock", "ubuntu-latest repository-wide lock",
        )

    def test_duplicate_expanded_check_names_fail_actionably(self) -> None:
        self.repository.replace(
            "issue-46-atomic-conversation.yml", "os: [ubuntu-24.04, macos-14]",
            "os: [ubuntu-24.04, ubuntu-24.04]",
        )
        self.assert_fails("issue-46-atomic-conversation.yml: duplicate expanded check names", "atomic-rounds")

    def test_unsupported_allocation_syntax_fails_actionably(self) -> None:
        self.repository.replace(
            "issue-187-subagent-fanout.yml", "matrix:\n        os:",
            "matrix:\n        exclude: []\n        os:",
        )
        self.assert_fails("issue-187-subagent-fanout.yml", "unsupported matrix.exclude")

    def test_malformed_yaml_fails_actionably(self) -> None:
        self.repository.workflow("docs.yml").write_text("jobs: [\n")
        self.assert_fails("docs.yml: invalid workflow YAML")


if __name__ == "__main__":
    unittest.main()
