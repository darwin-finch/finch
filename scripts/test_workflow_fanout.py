#!/usr/bin/env python3
"""Mutation-sensitive regressions for the workflow fan-out contract."""

from __future__ import annotations

import shutil
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parent.parent
CHECKER = ROOT / "scripts/check_workflow_fanout.py"


class WorkflowRepository:
    def __init__(self) -> None:
        self.temporary = tempfile.TemporaryDirectory()
        self.root = Path(self.temporary.name)
        shutil.copytree(ROOT / ".github" / "workflows", self.root / ".github" / "workflows")

    def close(self) -> None:
        self.temporary.cleanup()

    def replace(self, name: str, old: str, new: str) -> None:
        path = self.root / ".github" / "workflows" / name
        contents = path.read_text(encoding="utf-8")
        if old not in contents:
            raise AssertionError(f"mutation anchor missing from {name}: {old!r}")
        path.write_text(contents.replace(old, new, 1), encoding="utf-8")

    def write(self, name: str, contents: str) -> None:
        path = self.root / ".github" / "workflows" / name
        path.write_text(contents, encoding="utf-8")

    def run(self) -> subprocess.CompletedProcess[str]:
        return subprocess.run(
            [sys.executable, str(CHECKER), "--root", str(self.root)],
            check=False,
            capture_output=True,
            text=True,
        )


class WorkflowFanoutMutationTests(unittest.TestCase):
    def setUp(self) -> None:
        self.repo = WorkflowRepository()

    def tearDown(self) -> None:
        self.repo.close()

    def assert_rejected(self, expected: str) -> None:
        result = self.repo.run()
        self.assertEqual(result.returncode, 1, result.stdout + result.stderr)
        self.assertIn(expected, result.stderr, result.stdout + result.stderr)

    def test_rejects_unfiltered_closed_issue_pull_request(self) -> None:
        self.repo.write(
            "issue-185-spreadsheet-advisories.yml",
            "name: retired\non:\n  pull_request:\njobs:\n  audit:\n"
            "    runs-on: ubuntu-24.04\n    steps:\n      - run: cargo audit\n",
        )
        self.assert_rejected("retired closed-issue workflow returned")

    def test_rejects_restored_closed_issue_yaml_extension(self) -> None:
        self.repo.write(
            "issue-186-ssh-removal.yaml",
            "name: retired\non:\n  pull_request:\njobs:\n  probe:\n"
            "    runs-on: ubuntu-24.04\n    steps:\n      - run: cargo check\n",
        )
        self.assert_rejected("issue-186-ssh-removal.yaml: retired closed-issue workflow")

    def test_rejects_duplicate_canonical_all_target_matrix(self) -> None:
        self.repo.replace(
            "repository-hygiene.yml",
            "  tracked-tree:\n",
            "  duplicate-all-targets:\n    strategy:\n      matrix:\n        os: [ubuntu-24.04, macos-14]\n        feature_name: [default, no-default-features]\n    runs-on: ${{ matrix.os }}\n    steps:\n      - run: cargo test --all-targets\n\n  tracked-tree:\n",
        )
        self.assert_rejected("duplicates the canonical four-job all-target")

    def test_rejects_duplicate_canonical_release_matrix(self) -> None:
        self.repo.replace(
            "repository-hygiene.yml",
            "  tracked-tree:\n",
            "  duplicate-release:\n    strategy:\n      matrix:\n        include:\n          - os: ubuntu-24.04\n            target: x86_64-unknown-linux-gnu\n          - os: macos-14\n            target: aarch64-apple-darwin\n    runs-on: ${{ matrix.os }}\n    steps:\n      - run: cargo build --release --target ${{ matrix.target }}\n\n  tracked-tree:\n",
        )
        self.assert_rejected("duplicates the canonical two-job release")

    def test_rejects_duplicate_matrix_in_yaml_extension(self) -> None:
        self.repo.write(
            "duplicate.yaml",
            "name: duplicate\non:\n  pull_request:\njobs:\n  all-targets:\n"
            "    strategy:\n      matrix:\n"
            "        os: [ubuntu-24.04, macos-14]\n"
            "        feature_name: [default, no-default-features]\n"
            "    runs-on: ${{ matrix.os }}\n    steps:\n"
            "      - run: cargo test --all-targets\n",
        )
        self.assert_rejected("duplicate.yaml::all-targets: duplicates the canonical")

    def test_rejects_second_ordinary_full_cargo_audit(self) -> None:
        self.repo.replace(
            "repository-hygiene.yml",
            "      - name: Check the current tracked tree\n",
            "      - name: Duplicate full audit\n        run: cargo audit\n      - name: Check the current tracked tree\n",
        )
        self.assert_rejected("exactly one full cargo audit; found 3")

    def test_rejects_removed_or_commented_ssh_source_guard(self) -> None:
        self.repo.replace(
            "repository-hygiene.yml",
            "        run: python3 scripts/check_no_ssh_surface.py\n",
            "        # run: python3 scripts/check_no_ssh_surface.py\n",
        )
        self.assert_rejected("active steps must run scripts/check_no_ssh_surface.py")

    def test_rejects_if_false_ssh_source_guard(self) -> None:
        self.repo.replace(
            "repository-hygiene.yml",
            "      - name: Reject restoration of the removed SSH surface\n",
            "      - name: Reject restoration of the removed SSH surface\n        if: false\n",
        )
        self.assert_rejected("active steps must run scripts/check_no_ssh_surface.py")

    def test_rejects_manifest_not_triggering_downstream_api_gate(self) -> None:
        self.repo.replace(
            "ci.yml",
            "          Cargo.toml Cargo.lock src/lib.rs scripts/check_removed_ssh_api.py \\\n",
            "          Cargo.lock src/lib.rs scripts/check_removed_ssh_api.py \\\n",
        )
        self.assert_rejected("required path 'Cargo.toml'")

    def test_rejects_lockfile_not_triggering_downstream_api_gate(self) -> None:
        self.repo.replace(
            "ci.yml",
            "          Cargo.toml Cargo.lock src/lib.rs scripts/check_removed_ssh_api.py \\\n",
            "          Cargo.toml src/lib.rs scripts/check_removed_ssh_api.py \\\n",
        )
        self.assert_rejected("required path 'Cargo.lock'")

    def test_rejects_public_api_not_triggering_ssh_api_gate(self) -> None:
        self.repo.replace(
            "ci.yml",
            "          Cargo.toml Cargo.lock src/lib.rs scripts/check_removed_ssh_api.py \\\n",
            "          Cargo.toml Cargo.lock scripts/check_removed_ssh_api.py \\\n",
        )
        self.assert_rejected("required path 'src/lib.rs'")

    def test_rejects_disabled_required_matrix(self) -> None:
        self.repo.replace(
            "ci.yml",
            "  test:\n",
            "  test:\n    if: false\n",
        )
        self.assert_rejected("fixture ordinary source")

    def test_rejects_if_false_canonical_audit_step(self) -> None:
        self.repo.replace(
            "ci.yml",
            "    - name: Run the one full Cargo audit and retain named advisory diagnostics\n",
            "    - name: Run the one full Cargo audit and retain named advisory diagnostics\n"
            "      if: false\n",
        )
        self.assert_rejected("exactly one full cargo audit; found 0")

    def test_rejects_if_false_removed_package_step(self) -> None:
        self.repo.replace(
            "ci.yml",
            "    - name: Prove removed SSH and RSA packages stay out of the resolved graph\n",
            "    - name: Prove removed SSH and RSA packages stay out of the resolved graph\n"
            "      if: false\n",
        )
        self.assert_rejected("grep -Eq")

    def test_rejects_if_false_downstream_api_step(self) -> None:
        self.repo.replace(
            "ci.yml",
            "      if: steps.security-paths.outputs.removed_ssh_api == 'true'\n"
            "      run: |\n"
            "        python3 scripts/check_removed_ssh_api.py\n",
            "      if: false\n"
            "      run: |\n"
            "        python3 scripts/check_removed_ssh_api.py\n",
        )
        self.assert_rejected("active downstream removed-API step")

    def test_rejects_inverted_downstream_api_condition(self) -> None:
        self.repo.replace(
            "ci.yml",
            "      if: steps.security-paths.outputs.removed_ssh_api == 'true'\n"
            "      run: |\n"
            "        python3 scripts/check_removed_ssh_api.py\n",
            "      if: steps.security-paths.outputs.removed_ssh_api != 'true'\n"
            "      run: |\n"
            "        python3 scripts/check_removed_ssh_api.py\n",
        )
        self.assert_rejected("condition changed")

    def test_rejects_push_only_canonical_job_condition(self) -> None:
        self.repo.replace(
            "ci.yml",
            "  security:\n",
            "  security:\n    if: github.event_name == 'push'\n",
        )
        self.assert_rejected("canonical job-level if is unsupported")

    def test_rejects_pull_request_types_that_omit_normal_updates(self) -> None:
        self.repo.replace(
            "ci.yml",
            "  pull_request:\n    branches: [ main ]\n",
            "  pull_request:\n    branches: [ main ]\n    types: [closed]\n",
        )
        self.assert_rejected("omits required PR activation types")

    def test_rejects_empty_pull_request_types(self) -> None:
        self.repo.replace(
            "issue-163-effect-audit.yml",
            "  pull_request:\n    paths:\n",
            "  pull_request:\n    types: []\n    paths:\n",
        )
        self.assert_rejected("omits required PR activation types")

    def test_rejects_required_guard_continue_on_error(self) -> None:
        self.repo.replace(
            "repository-hygiene.yml",
            "      - name: Reject restoration of the removed SSH surface\n",
            "      - name: Reject restoration of the removed SSH surface\n"
            "        continue-on-error: true\n",
        )
        self.assert_rejected("continue-on-error is unsupported")

    def test_rejects_detector_early_exit(self) -> None:
        self.repo.replace(
            "ci.yml",
            "      run: |\n        base='${{ github.event.pull_request.base.sha || github.event.before }}'\n",
            "      run: |\n        exit 0\n"
            "        base='${{ github.event.pull_request.base.sha || github.event.before }}'\n",
        )
        self.assert_rejected("Detect manifest and public-API changes: normalized run block changed")

    def test_rejects_package_guard_early_exit(self) -> None:
        self.repo.replace(
            "ci.yml",
            "      run: |\n        printf '%s\\n' \\\n"
            "          'RUSTSEC-2026-0154 remains unreachable because russh is absent' \\\n",
            "      run: |\n        exit 0\n        printf '%s\\n' \\\n"
            "          'RUSTSEC-2026-0154 remains unreachable because russh is absent' \\\n",
        )
        self.assert_rejected("Prove removed SSH and RSA packages")

    def test_rejects_package_guard_hidden_behind_false(self) -> None:
        self.repo.replace(
            "ci.yml",
            "        if grep -Eq '^name = \"(rsa|russh|russh-cryptovec|russh-keys)\"$' Cargo.lock; then\n",
            "        if false && grep -Eq '^name = \"(rsa|russh|russh-cryptovec|russh-keys)\"$' Cargo.lock; then\n",
        )
        self.assert_rejected("Prove removed SSH and RSA packages")

    def test_rejects_quick_xml_guard_early_exit(self) -> None:
        self.repo.replace(
            "ci.yml",
            "        cargo tree --invert quick-xml",
            "        exit 0\n        cargo tree --invert quick-xml",
        )
        self.assert_rejected("Prove the quick-xml floor")

    def test_rejects_overwritten_quick_xml_minimum(self) -> None:
        self.repo.replace("ci.yml", "        minimum=0.41.0\n", "        minimum=0.40.0\n")
        self.assert_rejected("Prove the quick-xml floor")

    def test_rejects_duplicate_matrix_row_without_deduplication(self) -> None:
        self.repo.replace(
            "ci.yml",
            "          - os: macos-14\n            feature_name: no-default-features\n            cargo_args: --no-default-features\n",
            "          - os: macos-14\n            feature_name: no-default-features\n            cargo_args: --no-default-features\n"
            "          - os: macos-14\n            feature_name: no-default-features\n            cargo_args: --no-default-features\n",
        )
        self.assert_rejected("expected 14 activated jobs, found 15")

    def test_rejects_matrix_argument_change_even_when_display_axes_match(self) -> None:
        self.repo.replace(
            "ci.yml",
            "            cargo_args: --no-default-features\n",
            "            cargo_args: --all-features\n",
        )
        self.assert_rejected("cargo_args=--all-features")

    def test_rejects_wrapped_additional_full_audit(self) -> None:
        self.repo.replace(
            "repository-hygiene.yml",
            "      - name: Check the current tracked tree\n",
            "      - name: Wrapped audit\n        run: bash -c 'cargo audit'\n"
            "      - name: Check the current tracked tree\n",
        )
        self.assert_rejected("cargo-audit invocation is outside")

    def test_rejects_advisory_marker_only_in_comment(self) -> None:
        self.repo.replace(
            "ci.yml",
            "          'RUSTSEC-2026-0194 is excluded by quick-xml >= 0.41.0' \\\n",
            "          # RUSTSEC-2026-0194 is excluded by quick-xml >= 0.41.0 \\\n",
        )
        self.repo.replace(
            "ci.yml",
            '              .advisory.id != "RUSTSEC-2026-0194"\n',
            '              .advisory.id != "RUSTSEC-2099-9999"\n',
        )
        self.assert_rejected("security marker 'RUSTSEC-2026-0194'")


class CurrentWorkflowFanoutTests(unittest.TestCase):
    def test_current_workflows_match_pinned_fanout(self) -> None:
        result = subprocess.run(
            [sys.executable, str(CHECKER), "--root", str(ROOT)],
            check=False,
            capture_output=True,
            text=True,
        )
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)


if __name__ == "__main__":
    unittest.main()
