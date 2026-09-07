#!/usr/bin/env python3
"""Mutation-sensitive tests for the bounded workflow fan-out contract."""

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
        (self.root / ".github" / "workflows" / name).write_text(contents, encoding="utf-8")

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

    def assert_rejected(self, *diagnostics: str) -> None:
        result = self.repo.run()
        output = result.stdout + result.stderr
        self.assertEqual(result.returncode, 1, f"mutated workflow contract unexpectedly passed:\n{output}")
        for diagnostic in diagnostics:
            self.assertIn(diagnostic, output, f"rejection did not explain {diagnostic!r}:\n{output}")

    def assert_accepted(self) -> None:
        result = self.repo.run()
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)

    def add_job(self, workflow: str, yaml: str) -> None:
        self.repo.replace(workflow, "jobs:\n", f"jobs:\n{yaml}")

    def test_rejects_retired_workflow_with_yml_extension(self) -> None:
        self.repo.write("issue-185-spreadsheet-advisories.yml", "on: pull_request\njobs:\n  audit:\n    runs-on: ubuntu-24.04\n    steps:\n    - run: cargo audit\n")
        self.assert_rejected("retired closed-issue workflow returned", "found 15")

    def test_rejects_retired_workflow_with_yaml_extension(self) -> None:
        self.repo.write("issue-186-ssh-removal.yaml", "on: pull_request\njobs:\n  probe:\n    runs-on: ubuntu-24.04\n    steps:\n    - run: cargo check\n")
        self.assert_rejected("issue-186-ssh-removal.yaml", "found 15")

    def test_rejects_literal_false_required_job(self) -> None:
        self.repo.replace("ci.yml", "  security:\n", "  security:\n    if: false\n")
        self.assert_rejected("expected 14 activated jobs, found 13", "critical job keys changed", "if")

    def test_counts_truthy_conditional_extra_job(self) -> None:
        self.add_job("repository-hygiene.yml", "  extra:\n    if: ${{ always() }}\n    runs-on: ubuntu-24.04\n    steps:\n    - run: cargo check\n")
        self.assert_rejected("expected 14 activated jobs, found 15", "repository-hygiene.yml::extra")

    def test_counts_unknown_conditional_extra_job(self) -> None:
        self.add_job("repository-hygiene.yml", "  extra:\n    if: github.actor != 'nobody'\n    runs-on: ubuntu-24.04\n    steps:\n    - run: cargo check\n")
        self.assert_rejected("expected 14 activated jobs, found 15", "if")

    def test_counts_constant_expression_instead_of_treating_it_as_false(self) -> None:
        self.add_job("repository-hygiene.yml", "  extra:\n    if: ${{ 1 == 0 }}\n    runs-on: ubuntu-24.04\n    steps:\n    - run: cargo check\n")
        self.assert_rejected("expected 14 activated jobs, found 15", "if=${{ 1 == 0 }}")

    def test_rejects_inverted_canonical_job_condition(self) -> None:
        self.repo.replace("ci.yml", "  security:\n", "  security:\n    if: github.event_name != 'pull_request'\n")
        self.assert_rejected("ci.yml::security", "unsupported=['if']")

    def test_rejects_push_only_canonical_job_condition(self) -> None:
        self.repo.replace("ci.yml", "  security:\n", "  security:\n    if: github.event_name == 'push'\n")
        self.assert_rejected("ci.yml::security", "unsupported=['if']")

    def test_rejects_canonical_trigger_that_omits_synchronize(self) -> None:
        self.repo.replace("ci.yml", "  pull_request:\n    branches: [ main ]\n", "  pull_request:\n    branches: [ main ]\n    types: [closed]\n")
        self.assert_rejected("ci.yml: trigger must exactly model normal pull_request synchronization", "expected 14 activated jobs, found 4")

    def test_accepts_unrelated_closed_only_workflow(self) -> None:
        self.repo.write("closed-maintenance.yaml", "on:\n  pull_request:\n    types: [closed]\njobs:\n  note:\n    runs-on: ubuntu-24.04\n    steps:\n    - run: echo closed\n")
        self.assert_accepted()

    def test_rejects_security_step_early_exit(self) -> None:
        self.repo.replace("ci.yml", "      run: |\n        printf '%s\\n' \\\n", "      run: |\n        exit 0\n        printf '%s\\n' \\\n")
        self.assert_rejected("Prove removed SSH and RSA packages", "key 'run' changed", "exit 0")

    def test_rejects_noop_package_guard(self) -> None:
        self.repo.replace("ci.yml", "        if grep -Eq '^name = \"(rsa|russh|russh-cryptovec|russh-keys)\"$' Cargo.lock; then\n", "        if false && grep -Eq '^name = \"(rsa|russh|russh-cryptovec|russh-keys)\"$' Cargo.lock; then\n")
        self.assert_rejected("Prove removed SSH and RSA packages", "if false")

    def test_rejects_overwritten_quick_xml_floor(self) -> None:
        self.repo.replace("ci.yml", "        minimum=0.41.0\n", "        minimum=0.41.0\n        minimum=0.0.0\n")
        self.assert_rejected("Prove the quick-xml floor", "minimum=0.0.0")

    def test_rejects_hidden_matrix_axis(self) -> None:
        self.repo.replace("ci.yml", "            cargo_args: \"\"\n", "            cargo_args: \"\"\n            hidden: surprise\n")
        self.assert_rejected("hidden=surprise", "missing=")

    def test_rejects_duplicate_matrix_row_without_collapsing_it(self) -> None:
        row = "          - os: macos-14\n            feature_name: no-default-features\n            cargo_args: --no-default-features\n"
        self.repo.replace("ci.yml", row, row + row)
        self.assert_rejected("expected 14 activated jobs, found 15", "row=4")

    def test_rejects_duplicate_axis_value_without_collapsing_it(self) -> None:
        self.repo.replace("repository-hygiene.yml", "        os: [ubuntu-24.04, macos-14]\n", "        os: [ubuntu-24.04, macos-14, macos-14]\n")
        self.assert_rejected("expected 14 activated jobs, found 15", "row=2")

    def test_rejects_duplicate_matrix_axis_mapping_key(self) -> None:
        self.repo.replace("repository-hygiene.yml", "        os: [ubuntu-24.04, macos-14]\n", "        os: [ubuntu-24.04, macos-14]\n        os: [ubuntu-24.04]\n")
        self.assert_rejected("duplicate mapping key 'os'")

    def test_rejects_dynamic_matrix(self) -> None:
        self.repo.replace("repository-hygiene.yml", "      matrix:\n        os: [ubuntu-24.04, macos-14]\n", "      matrix: ${{ fromJSON(needs.plan.outputs.matrix) }}\n")
        self.assert_rejected("dynamic or non-mapping matrix is unsupported")

    def test_rejects_wrapped_additional_audit(self) -> None:
        self.add_job("repository-hygiene.yml", "  wrapped-audit:\n    runs-on: ubuntu-24.04\n    steps:\n    - run: bash -c 'cargo audit'\n")
        self.assert_rejected("exactly one full cargo audit", "found 2 occurrences", "wrapped-audit")

    def test_rejects_cargo_audit_binary_invocation(self) -> None:
        self.add_job("repository-hygiene.yml", "  binary-audit:\n    runs-on: ubuntu-24.04\n    steps:\n    - run: cargo-audit --json\n")
        self.assert_rejected("exactly one full cargo audit", "found 2 occurrences", "binary-audit")

    def test_rejects_action_based_additional_audit(self) -> None:
        self.add_job("repository-hygiene.yml", "  action-audit:\n    runs-on: ubuntu-24.04\n    steps:\n    - uses: rustsec/audit-check@v2\n")
        self.assert_rejected("exactly one full cargo audit", "rustsec/audit-check@v2")

    def test_rejects_second_cargo_audit_install(self) -> None:
        self.add_job("repository-hygiene.yml", "  install-audit:\n    runs-on: ubuntu-24.04\n    steps:\n    - run: cargo install cargo-audit --locked\n")
        self.assert_rejected("exactly the canonical cargo-audit install", "found 2 occurrences")

    def test_rejects_critical_job_continue_on_error(self) -> None:
        self.repo.replace("ci.yml", "  security:\n", "  security:\n    continue-on-error: true\n")
        self.assert_rejected("ci.yml::security", "continue-on-error")

    def test_rejects_critical_job_defaults(self) -> None:
        self.repo.replace("ci.yml", "  security:\n", "  security:\n    defaults:\n      run:\n        working-directory: /tmp\n")
        self.assert_rejected("ci.yml::security", "defaults")

    def test_rejects_critical_job_environment(self) -> None:
        self.repo.replace("ci.yml", "  security:\n", "  security:\n    env:\n      GITHUB_OUTPUT: /dev/null\n")
        self.assert_rejected("ci.yml::security", "env")

    def test_rejects_critical_step_working_directory(self) -> None:
        self.repo.replace("ci.yml", "    - name: Prove removed SSH and RSA packages stay out of the resolved graph\n      shell: bash\n", "    - name: Prove removed SSH and RSA packages stay out of the resolved graph\n      shell: bash\n      working-directory: /tmp\n")
        self.assert_rejected("working-directory", "unsupported")

    def test_rejects_critical_step_environment(self) -> None:
        self.repo.replace("ci.yml", "    - name: Detect manifest and public-API changes\n      id: security-paths\n", "    - name: Detect manifest and public-API changes\n      id: security-paths\n      env:\n        GITHUB_OUTPUT: /dev/null\n")
        self.assert_rejected("env", "GITHUB_OUTPUT")

    def test_rejects_critical_background_step(self) -> None:
        self.repo.replace("ci.yml", "    - name: Detect manifest and public-API changes\n      id: security-paths\n", "    - name: Detect manifest and public-API changes\n      id: security-paths\n      background: true\n")
        self.assert_rejected("background", "unsupported")

    def test_rejects_critical_step_continue_on_error(self) -> None:
        self.repo.replace("repository-hygiene.yml", "      - name: Reject restoration of the removed SSH surface\n", "      - name: Reject restoration of the removed SSH surface\n        continue-on-error: true\n")
        self.assert_rejected("continue-on-error", "Reject restoration")

    def test_rejects_producer_consumer_reordering(self) -> None:
        producer = "    - name: Detect manifest and public-API changes\n      id: security-paths\n      shell: bash\n      run: |\n"
        start = (self.repo.root / ".github/workflows/ci.yml").read_text(encoding="utf-8")
        producer_index = start.index(producer)
        consumer = "    - name: Install Cap'n Proto for the downstream removed-API probe\n"
        consumer_index = start.index(consumer)
        producer_block = start[producer_index:consumer_index]
        after_consumer = start.index("    - name: Compile downstream positive controls", consumer_index)
        mutated = start[:producer_index] + start[consumer_index:after_consumer] + producer_block + start[after_consumer:]
        (self.repo.root / ".github/workflows/ci.yml").write_text(mutated, encoding="utf-8")
        self.assert_rejected("critical step order changed", "security-paths")

    def test_rejects_split_hardcoded_all_target_jobs_on_unmodeled_path(self) -> None:
        jobs = "".join(
            f"  duplicate-{index}:\n    runs-on: {os_name}\n    steps:\n    - run: cargo test --all-targets {args}\n"
            for index, (os_name, args) in enumerate((
                ("ubuntu-24.04", ""),
                ("ubuntu-24.04", "--no-default-features"),
                ("macos-14", ""),
                ("macos-14", "--no-default-features"),
            ))
        )
        self.repo.write("unmodeled.yaml", "on:\n  pull_request:\n    paths: [never/**]\njobs:\n" + jobs)
        self.assert_rejected("unmodeled.yaml::duplicate-0", "forbidden duplicate cargo test --all-targets")

    def test_rejects_release_target_build_outside_canonical_ci(self) -> None:
        self.repo.write("duplicate-release.yml", "on: pull_request\njobs:\n  release:\n    runs-on: ubuntu-24.04\n    steps:\n    - run: cargo build --release --target x86_64-unknown-linux-gnu\n")
        self.assert_rejected("forbidden duplicate cargo build --release --target")

    def test_rejects_release_target_build_with_reversed_argument_order(self) -> None:
        self.repo.write("duplicate-release.yml", "on: pull_request\njobs:\n  release:\n    runs-on: ubuntu-24.04\n    steps:\n    - run: cargo build --target x86_64-unknown-linux-gnu --release\n")
        self.assert_rejected("forbidden duplicate cargo build --release --target")

    def test_preserves_issue_56_supervisor_release_build(self) -> None:
        self.assert_accepted()

    def test_rejects_missing_hygiene_self_trigger(self) -> None:
        self.repo.replace("repository-hygiene.yml", "  pull_request:\n", "  pull_request:\n    paths: [README.md]\n")
        self.assert_rejected("trigger must exactly model normal pull_request synchronization", "hygiene workflow definition")

    def test_rejects_changed_downstream_condition(self) -> None:
        self.repo.replace("ci.yml", "      if: steps.security-paths.outputs.removed_ssh_api == 'true'\n", "      if: steps.security-paths.outputs.removed_ssh_api != 'true'\n")
        self.assert_rejected("key 'if' changed", "removed_ssh_api != 'true'")

    def test_rejects_changed_detector_paths(self) -> None:
        self.repo.replace("ci.yml", "          Cargo.toml Cargo.lock src/lib.rs scripts/check_removed_ssh_api.py \\\n", "          Cargo.lock src/lib.rs scripts/check_removed_ssh_api.py \\\n")
        self.assert_rejected("Detect manifest and public-API changes", "key 'run' changed")


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
