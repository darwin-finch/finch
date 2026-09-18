#!/usr/bin/env python3
"""Production-boundary regressions for the semantic workflow checker.

The checker itself validates the full reviewed inventory on every real PR;
these tests keep only the load-bearing proof that it fails closed on the
mutations that matter most — untrusted cache writes, pull_request_target,
controller permission drift, main-only gates, and runner labels. Historical
note: this suite carried ~54 mutation tests (60s local, ~2.5m CI) covering
every rule; the owner cut it to this subset in the #518 throughput work,
trading checker-self-proof for PR wall time. The checker's rules are
unchanged.
"""

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
    BREAKAGE_WORKFLOW,
    CANCELLATION_WORKFLOW,
    ESCAPE_API_ALLOWLIST,
    EXPECTED_PATHS,
    escape_api_errors,
    event_contract,
    load_yaml,
    workflow_activates,
)


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

    def test_workflow_inventory_addition_and_removal_fails(self) -> None:
        shutil.copy2(self.repository.workflow("docs.yml"), self.repository.workflow("surprise.yml"))
        self.assert_fails("workflow inventory changed", "surprise.yml")
        self.repository.workflow("surprise.yml").unlink()
        self.repository.workflow("release.yml").unlink()
        self.assert_fails("workflow inventory changed", "release.yml")

    def test_negative_path_filters_are_applied_per_changed_path(self) -> None:
        self.assertTrue(
            workflow_activates(
                {"paths": ("docs/**", "!docs/generated/**")},
                ("docs/generated/index.md", "docs/guide.md"),
            ),
            "one excluded file must not hide a different included changed path",
        )

    def test_real_tree_escape_api_uses_match_the_allowlist(self) -> None:
        self.assertEqual([], escape_api_errors(ROOT), "current tree escape-API uses diverge from the allowlist")

    def test_pull_request_target_fails_closed(self) -> None:
        self.repository.replace("release.yml", "  push:\n", "  pull_request_target:\n\n  push:\n")
        self.assert_fails("release.yml: on.pull_request_target is unsupported")

    def test_macos_test_job_is_not_a_pull_request_gate(self) -> None:
        self.repository.replace(
            "ci.yml",
            "    if: github.event_name == 'push' && github.ref == 'refs/heads/main'\n",
            "    if: true\n",
        )
        self.assert_fails(
            "ci.yml: expanded check allocation changed",
            "Test (macos-14, default)",
        )

    def test_release_build_job_is_not_a_pull_request_gate(self) -> None:
        self.repository.replace(
            "ci.yml",
            "    if: github.event_name == 'push' && github.ref == 'refs/heads/main'\n",
            "    if: true\n",
            after="    name: Build Release (x86_64-unknown-linux-gnu)\n",
        )
        self.assert_fails(
            "ci.yml: job 'build' must stay main-only",
            "release preflight compiles are not pull-request merge gates",
            "expanded check allocation changed",
        )

    def test_blacksmith_pilot_runner_label_is_pinned(self) -> None:
        self.repository.replace(
            "ci.yml",
            "    runs-on: blacksmith-8vcpu-ubuntu-2404\n",
            "    runs-on: blacksmith-8vcpu-ubuntu-2405\n",
        )
        self.assert_fails(
            "ci.yml: runner label inventory changed",
            "blacksmith-8vcpu-ubuntu-2404",
            "owner job 'test' must run actively on blacksmith-8vcpu-ubuntu-2404",
        )

    def test_migrated_canonical_boundaries_reject_inert_or_changed_steps(self) -> None:
        mutations = (
            (
                "cargo test --doc -- ValidatedProviderRequest",
                "cargo test --lib -- ValidatedProviderRequest",
                "Prove validated request tokens cannot be forged", "commands changed",
            ),
            (
                "cargo test --release --target x86_64-unknown-linux-gnu --lib cli::conversation::tests -- --nocapture",
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

    def test_cancellation_controller_is_not_pull_request_active(self) -> None:
        self.assertNotIn(
            CANCELLATION_WORKFLOW,
            EXPECTED_PATHS,
            "the trusted controller must keep the empty fixture-membership set",
        )
        document = load_yaml(ROOT / ".github/workflows" / CANCELLATION_WORKFLOW)
        self.assertIs(
            event_contract(document, CANCELLATION_WORKFLOW, "pull_request"),
            False,
            "the trusted controller must not activate on pull_request",
        )

    def test_cancellation_effective_actions_permission_narrowing_fails(self) -> None:
        self.repository.replace(CANCELLATION_WORKFLOW, "  actions: write\n", "  actions: read\n")
        self.assert_fails("effective permissions changed", "'actions': 'write'", "'actions': 'read'")

    def test_main_breakage_controller_is_not_pull_request_active(self) -> None:
        self.assertNotIn(
            BREAKAGE_WORKFLOW,
            EXPECTED_PATHS,
            "the trusted controller must keep the empty fixture-membership set",
        )
        document = load_yaml(ROOT / ".github/workflows" / BREAKAGE_WORKFLOW)
        self.assertIs(
            event_contract(document, BREAKAGE_WORKFLOW, "pull_request"),
            False,
            "the trusted controller must not activate on pull_request",
        )
        self.assertIs(
            event_contract(document, BREAKAGE_WORKFLOW, "push"),
            False,
            "the trusted controller must not activate directly on push",
        )

    def test_main_breakage_controller_envelope_is_bound(self) -> None:
        mutations = (
            ("  issues: write\n", "  issues: read\n", "effective permissions changed"),
            (
                "          TOKEN: ${{ github.token }}\n",
                "          TOKEN: ${{ secrets.BREAKAGE_BOT_TOKEN }}\n",
                "trusted step token binding changed",
            ),
            ("    types: [completed]\n", "", "workflow_run trigger changed"),
            (
                "def api(method, path, expected=(200, 201), payload=None):",
                "def api(method, path, expected=(200,), payload=None):",
                "API default accepted-status set changed",
            ),
            (
                '          ISSUE_TITLE = "CI failed on main"\n',
                '          ISSUE_TITLE = "Main CI is red"\n',
                "breakage issue title changed",
            ),
        )
        for old, new, *diagnostics in mutations:
            with self.subTest(diagnostics=diagnostics):
                repository = Repository()
                try:
                    repository.replace(BREAKAGE_WORKFLOW, old, new)
                    result = repository.check()
                    self.assertNotEqual(0, result.returncode, f"breakage-controller mutant passed: {diagnostics}")
                    for diagnostic in diagnostics:
                        self.assertIn(diagnostic, result.stderr, result.stderr)
                finally:
                    repository.close()

    def test_main_breakage_controller_rejects_checkout_and_extra_jobs(self) -> None:
        self.repository.replace(
            BREAKAGE_WORKFLOW,
            "      - name: Update the main breakage issue\n",
            "      - uses: actions/checkout@v4\n      - name: Update the main breakage issue\n",
        )
        self.assert_fails("trusted controller must not checkout or run an action")

        repository = Repository()
        try:
            repository.replace(
                BREAKAGE_WORKFLOW,
                "permissions:\n",
                "defaults:\n  run:\n    shell: bash\n\npermissions:\n",
            )
            result = repository.check()
            self.assertNotEqual(0, result.returncode, "workflow-level defaults passed")
            self.assertIn(
                "workflow-level defaults would rewrite the trusted step", result.stderr, result.stderr
            )
        finally:
            repository.close()


if __name__ == "__main__":
    unittest.main()
