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
        self.assert_rejected("Cargo audit occurrence is outside", "wrapped-audit")

    def test_rejects_cargo_audit_binary_invocation(self) -> None:
        self.add_job("repository-hygiene.yml", "  binary-audit:\n    runs-on: ubuntu-24.04\n    steps:\n    - run: cargo-audit --json\n")
        self.assert_rejected("Cargo audit occurrence is outside", "binary-audit")

    def test_rejects_action_based_additional_audit(self) -> None:
        self.add_job("repository-hygiene.yml", "  action-audit:\n    runs-on: ubuntu-24.04\n    steps:\n    - uses: rustsec/audit-check@v2\n")
        self.assert_rejected("Cargo audit occurrence is outside", "rustsec/audit-check@v2")

    def test_rejects_second_cargo_audit_install(self) -> None:
        self.add_job("repository-hygiene.yml", "  install-audit:\n    runs-on: ubuntu-24.04\n    steps:\n    - run: cargo install cargo-audit --locked\n")
        self.assert_rejected("Cargo audit occurrence is outside", "install-audit")

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
        self.repo.replace("ci.yml", "    - name: Detect removed-SSH API-relevant changes\n      id: removed-ssh-api-paths\n", "    - name: Detect removed-SSH API-relevant changes\n      id: removed-ssh-api-paths\n      env:\n        GITHUB_OUTPUT: /dev/null\n")
        self.assert_rejected("env", "GITHUB_OUTPUT")

    def test_rejects_critical_background_step(self) -> None:
        self.repo.replace("ci.yml", "    - name: Detect removed-SSH API-relevant changes\n      id: removed-ssh-api-paths\n", "    - name: Detect removed-SSH API-relevant changes\n      id: removed-ssh-api-paths\n      background: true\n")
        self.assert_rejected("background", "unsupported")

    def test_rejects_critical_step_continue_on_error(self) -> None:
        self.repo.replace("repository-hygiene.yml", "      - name: Reject restoration of the removed SSH surface\n", "      - name: Reject restoration of the removed SSH surface\n        continue-on-error: true\n")
        self.assert_rejected("continue-on-error", "Reject restoration")

    def test_rejects_producer_consumer_reordering(self) -> None:
        producer = "    - name: Detect removed-SSH API-relevant changes\n      id: removed-ssh-api-paths\n      shell: bash\n      run: |\n"
        start = (self.repo.root / ".github/workflows/ci.yml").read_text(encoding="utf-8")
        producer_index = start.index(producer)
        consumer = "    - name: Compile downstream positive controls and require removed-API diagnostics\n"
        consumer_index = start.index(consumer)
        producer_block = start[producer_index:consumer_index]
        after_consumer = start.index("    - name: Run clippy", consumer_index)
        mutated = start[:producer_index] + start[consumer_index:after_consumer] + producer_block + start[after_consumer:]
        (self.repo.root / ".github/workflows/ci.yml").write_text(mutated, encoding="utf-8")
        self.assert_rejected("critical step order changed", "removed-ssh-api-paths")

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
        self.repo.replace("ci.yml", "      if: steps.removed-ssh-api-paths.outputs.run_probe == 'true'\n", "      if: steps.removed-ssh-api-paths.outputs.run_probe != 'true'\n")
        self.assert_rejected("key 'if' changed", "run_probe != 'true'")

    def test_rejects_changed_detector_paths(self) -> None:
        self.repo.replace("ci.yml", "          Cargo.toml Cargo.lock build.rs src/lib.rs scripts/check_removed_ssh_api.py \\\n", "          Cargo.lock build.rs src/lib.rs scripts/check_removed_ssh_api.py \\\n")
        self.assert_rejected("Detect removed-SSH API-relevant changes", "key 'run' changed")

    def test_rejects_workflow_root_defaults(self) -> None:
        self.repo.replace("ci.yml", "name: CI\n", "name: CI\ndefaults:\n  run:\n    working-directory: /tmp\n")
        self.assert_rejected("workflow-root defaults are forbidden", "working-directory")

    def test_ci_cross_hosts_the_fanout_checker(self) -> None:
        self.repo.replace("ci.yml", "    - name: Check pull-request workflow fan-out\n      run: python3 scripts/check_workflow_fanout.py\n\n", "")
        self.assert_rejected("ci.yml::security", "critical step order changed")

    def test_hygiene_cross_hosts_the_fanout_checker(self) -> None:
        self.repo.replace("repository-hygiene.yml", "      - name: Check pull-request workflow fan-out\n        run: python3 scripts/check_workflow_fanout.py\n", "")
        self.assert_rejected("repository-hygiene.yml::tracked-tree", "critical step order changed")

    def test_ci_cross_hosts_the_fanout_mutation_suite(self) -> None:
        self.repo.replace("ci.yml", "    - name: Run workflow fan-out mutation regressions\n      run: python3 scripts/test_workflow_fanout.py\n\n", "")
        self.assert_rejected("ci.yml::security", "critical step order changed")

    def test_hygiene_cross_hosts_the_fanout_mutation_suite(self) -> None:
        self.repo.replace("repository-hygiene.yml", "      - name: Run workflow fan-out mutation regressions\n        run: python3 scripts/test_workflow_fanout.py\n", "")
        self.assert_rejected("repository-hygiene.yml::tracked-tree", "critical step order changed")

    def test_rejects_reusable_workflow_job_on_synchronize(self) -> None:
        self.repo.write("reusable.yml", "on:\n  pull_request:\n    paths: [never/**]\njobs:\n  delegated:\n    uses: owner/repo/.github/workflows/test.yml@main\n")
        self.assert_rejected("reusable-workflow job-level uses is unsupported", "owner/repo")

    def test_rejects_synchronize_capable_pull_request_target(self) -> None:
        self.repo.write("target.yml", "on:\n  pull_request_target:\njobs:\n  note:\n    runs-on: ubuntu-24.04\n    steps:\n    - run: echo unsafe\n")
        self.assert_rejected("synchronize-capable pull_request_target is unsupported", "target.yml")

    def test_accepts_close_only_pull_request_target(self) -> None:
        self.repo.write("target.yml", "on:\n  pull_request_target:\n    types: [closed]\njobs:\n  note:\n    runs-on: ubuntu-24.04\n    steps:\n    - run: echo closed\n")
        self.assert_accepted()

    def test_rejects_path_specific_audit_not_in_ordinary_fixture(self) -> None:
        self.repo.write("manifest-audit.yml", "on:\n  pull_request:\n    paths: [Cargo.toml]\njobs:\n  audit:\n    runs-on: ubuntu-24.04\n    steps:\n    - run: cargo audit\n")
        self.assert_rejected("Cargo audit occurrence is outside", "manifest-audit.yml")

    def test_rejects_close_only_audit(self) -> None:
        self.repo.write("closed-audit.yml", "on:\n  pull_request:\n    types: [closed]\njobs:\n  audit:\n    runs-on: ubuntu-24.04\n    steps:\n    - run: cargo-audit\n")
        self.assert_rejected("Cargo audit occurrence is outside", "closed-audit.yml")

    def test_rejects_toolchain_selected_audit(self) -> None:
        self.repo.write("toolchain-audit.yml", "on:\n  pull_request:\n    paths: [Cargo.toml]\njobs:\n  audit:\n    runs-on: ubuntu-24.04\n    steps:\n    - run: sudo cargo +nightly audit --json\n")
        self.assert_rejected("Cargo audit occurrence is outside", "cargo +nightly audit")

    def test_rejects_job_policy_on_path_outside_fixtures(self) -> None:
        self.repo.write("non-gating.yml", "on:\n  pull_request:\n    paths: [unmodeled/**]\njobs:\n  check:\n    continue-on-error: true\n    runs-on: ubuntu-24.04\n    steps:\n    - run: cargo check\n")
        self.assert_rejected("job-level continue-on-error is forbidden", "non-gating.yml::check")

    def test_rejects_job_environment_on_path_outside_fixtures(self) -> None:
        self.repo.write("environment.yml", "on:\n  pull_request:\n    paths: [unmodeled/**]\njobs:\n  check:\n    env:\n      GITHUB_OUTPUT: /dev/null\n    runs-on: ubuntu-24.04\n    steps:\n    - run: cargo check\n")
        self.assert_rejected("job-level env is not an exact allowed existing contract", "GITHUB_OUTPUT")

    def test_rejects_job_defaults_on_path_outside_fixtures(self) -> None:
        self.repo.write("defaults.yml", "on:\n  pull_request:\n    paths: [unmodeled/**]\njobs:\n  check:\n    defaults:\n      run:\n        shell: bash\n    runs-on: ubuntu-24.04\n    steps:\n    - run: cargo check\n")
        self.assert_rejected("job-level defaults is forbidden", "shell")

    def test_rejects_deleted_canonical_test_step(self) -> None:
        self.repo.replace("ci.yml", "    - name: Test all targets\n      run: cargo test --all-targets ${{ matrix.cargo_args }} -- --nocapture\n\n", "")
        self.assert_rejected("ci.yml::test", "critical step order changed", "Test all targets")

    def test_rejects_inert_canonical_test_step(self) -> None:
        self.repo.replace("ci.yml", "    - name: Test all targets\n", "    - name: Test all targets\n      if: false\n")
        self.assert_rejected("ci.yml::test", "unsupported=['if']", "actual_step")

    def test_rejects_hardcoded_test_runner(self) -> None:
        self.repo.replace("ci.yml", "    runs-on: ${{ matrix.os }}\n", "    runs-on: ubuntu-24.04\n")
        self.assert_rejected("ci.yml::test", "'runs-on' changed", "ubuntu-24.04")

    def test_rejects_removed_feature_argument_plumbing(self) -> None:
        self.repo.replace("ci.yml", "cargo test --all-targets ${{ matrix.cargo_args }} -- --nocapture", "cargo test --all-targets -- --nocapture")
        self.assert_rejected("ci.yml::test", "Test all targets", "key 'run' changed")

    def test_rejects_hardcoded_release_target(self) -> None:
        self.repo.replace("ci.yml", "cargo build --release --target ${{ matrix.target }}", "cargo build --release --target x86_64-unknown-linux-gnu")
        self.assert_rejected("ci.yml::build", "Build release binary", "key 'run' changed")

    def test_rejects_disabled_brain_isolation_step(self) -> None:
        self.repo.replace("issue-56-brain-isolation.yml", "    - name: Exercise the complete synthetic isolation harness\n", "    - name: Exercise the complete synthetic isolation harness\n      if: false\n")
        self.assert_rejected("issue-56-brain-isolation.yml::isolation-boundaries", "unsupported=['if']")

    def test_rejects_non_gating_brain_isolation_step(self) -> None:
        self.repo.replace("issue-56-brain-isolation.yml", "    - name: Exercise the complete synthetic isolation harness\n", "    - name: Exercise the complete synthetic isolation harness\n      continue-on-error: true\n")
        self.assert_rejected("issue-56-brain-isolation.yml::isolation-boundaries", "continue-on-error")

    def test_rejects_hardcoded_brain_isolation_runner(self) -> None:
        self.repo.replace("issue-56-brain-isolation.yml", "    runs-on: ${{ matrix.os }}\n", "    runs-on: ubuntu-24.04\n")
        self.assert_rejected("issue-56-brain-isolation.yml::isolation-boundaries", "'runs-on' changed")

    def test_rejects_deleted_effect_audit_command(self) -> None:
        self.repo.replace("issue-163-effect-audit.yml", "      - name: Test durable reducer and log recovery\n        run: cargo test --lib runtime::effect_log::tests -- --nocapture\n", "")
        self.assert_rejected("issue-163-effect-audit.yml::effect-audit", "critical step order changed")

    def test_rejects_detector_missing_build_script(self) -> None:
        self.repo.replace("ci.yml", "          Cargo.toml Cargo.lock build.rs src/lib.rs scripts/check_removed_ssh_api.py \\\n", "          Cargo.toml Cargo.lock src/lib.rs scripts/check_removed_ssh_api.py \\\n")
        self.assert_rejected("Detect removed-SSH API-relevant changes", "build.rs")

    def test_rejects_missing_base_that_skips_instead_of_runs_probe(self) -> None:
        self.repo.replace("ci.yml", "          echo 'run_probe=true' >> \"$GITHUB_OUTPUT\"\n          exit 0\n", "          echo 'run_probe=false' >> \"$GITHUB_OUTPUT\"\n          exit 0\n")
        self.assert_rejected("Detect removed-SSH API-relevant changes", "run_probe=false")

    def test_rejects_fetch_failure_that_skips_instead_of_runs_probe(self) -> None:
        anchor = "        if ! git fetch --no-tags --depth=1 origin \"$base\"; then\n          echo 'cannot fetch the comparison base; running the downstream probe' >&2\n          echo 'run_probe=true' >> \"$GITHUB_OUTPUT\"\n"
        replacement = anchor.replace("run_probe=true", "run_probe=false")
        self.repo.replace("ci.yml", anchor, replacement)
        self.assert_rejected("Detect removed-SSH API-relevant changes", "run_probe=false")

    def test_rejects_diff_failure_that_exits_without_running_probe(self) -> None:
        self.repo.replace("ci.yml", "            echo 'run_probe=true' >> \"$GITHUB_OUTPUT\"\n            ;;\n", "            exit \"$diff_status\"\n            ;;\n")
        self.assert_rejected("Detect removed-SSH API-relevant changes", "exit")

    def test_rejects_probe_without_feature_argument(self) -> None:
        self.repo.replace("ci.yml", "python3 scripts/check_removed_ssh_api.py ${{ matrix.cargo_args }}", "python3 scripts/check_removed_ssh_api.py")
        self.assert_rejected("Compile downstream positive controls", "matrix.cargo_args")

    def test_rejects_removed_macos_no_default_probe_row(self) -> None:
        row = "          - os: macos-14\n            feature_name: no-default-features\n            cargo_args: --no-default-features\n"
        self.repo.replace("ci.yml", row, "")
        self.assert_rejected("ci.yml::test", "macos-14", "no-default-features")

    def test_double_star_slash_matches_zero_directories(self) -> None:
        self.repo.write("root-glob.yml", "on:\n  pull_request:\n    paths: ['**/README.md']\njobs:\n  root-file:\n    runs-on: ubuntu-24.04\n    steps:\n    - run: echo root\n")
        self.assert_rejected("fixture README", "root-glob.yml::root-file")

    def test_rejects_unsupported_github_glob_construct(self) -> None:
        self.repo.write("unsupported-glob.yml", "on:\n  pull_request:\n    paths: ['[R]EADME.md']\njobs:\n  maybe:\n    runs-on: ubuntu-24.04\n    steps:\n    - run: echo maybe\n")
        self.assert_rejected("unsupported GitHub path-pattern construct", "[R]EADME.md")

    def test_rejects_toolchain_selected_duplicate_all_targets(self) -> None:
        self.repo.write("toolchain-test.yml", "on:\n  pull_request:\n    paths: [never/**]\njobs:\n  test:\n    runs-on: ubuntu-24.04\n    steps:\n    - run: cargo +nightly test --all-targets\n")
        self.assert_rejected("forbidden duplicate cargo test --all-targets", "cargo +nightly test")

    def test_rejects_toolchain_selected_duplicate_release_build(self) -> None:
        self.repo.write("toolchain-build.yml", "on:\n  pull_request:\n    paths: [never/**]\njobs:\n  build:\n    runs-on: ubuntu-24.04\n    steps:\n    - run: cargo +nightly build --target x86_64-unknown-linux-gnu --release\n")
        self.assert_rejected("forbidden duplicate cargo build --release --target", "cargo +nightly build")

    def test_accepts_close_only_duplicate_command_as_non_synchronize(self) -> None:
        self.repo.write("closed-test.yml", "on:\n  pull_request:\n    types: [closed]\njobs:\n  report:\n    runs-on: ubuntu-24.04\n    steps:\n    - run: cargo test --all-targets\n")
        self.assert_accepted()

    def test_accepts_close_only_reusable_workflow_as_non_synchronize(self) -> None:
        self.repo.write("closed-reusable.yml", "on:\n  pull_request:\n    types: [closed]\njobs:\n  report:\n    uses: owner/repo/.github/workflows/report.yml@main\n")
        self.assert_accepted()


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
