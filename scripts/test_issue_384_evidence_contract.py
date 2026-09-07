#!/usr/bin/env python3
"""Mutation tests for the temporary, privileged issue 384 evidence lane."""

from __future__ import annotations

import os
import shutil
import subprocess
import tempfile
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parent.parent
WORKFLOW = ROOT / ".github/workflows/temporary-issue-384-cache-evidence.yml"
CHECKER = ROOT / "scripts/check_issue_384_evidence_contract.rb"
HELPER = ROOT / "scripts/configure_temporary_issue_384_sccache.sh"


class EvidenceContractTests(unittest.TestCase):
    def setUp(self) -> None:
        self.temporary = tempfile.TemporaryDirectory()
        self.workflow = Path(self.temporary.name) / WORKFLOW.name
        self.helper = Path(self.temporary.name) / HELPER.name
        shutil.copy2(WORKFLOW, self.workflow)
        shutil.copy2(HELPER, self.helper)

    def tearDown(self) -> None:
        self.temporary.cleanup()

    def replace(self, old: str, new: str) -> None:
        source = self.workflow.read_text(encoding="utf-8")
        self.assertIn(old, source, f"mutation source is absent: {old!r}")
        self.workflow.write_text(source.replace(old, new, 1), encoding="utf-8")

    def replace_helper(self, old: str, new: str) -> None:
        source = self.helper.read_text(encoding="utf-8")
        self.assertIn(old, source, f"helper mutation source is absent: {old!r}")
        self.helper.write_text(source.replace(old, new, 1), encoding="utf-8")

    def run_checker(self) -> subprocess.CompletedProcess[str]:
        return subprocess.run(
            ["ruby", str(CHECKER), str(self.workflow), str(self.helper)],
            check=False,
            capture_output=True,
            text=True,
        )

    def reject(self, expected: str) -> None:
        result = self.run_checker()
        output = result.stdout + result.stderr
        self.assertEqual(result.returncode, 1, output)
        self.assertIn(expected, output, output)

    def test_current_evidence_contract_passes(self) -> None:
        result = self.run_checker()
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn("disjoint claim/run caches", result.stdout)

    def test_actor_is_exact(self) -> None:
        self.replace("github.actor == 'schancel'", "github.actor == github.repository_owner")
        self.reject("authority predicate changed")

    def test_triggering_actor_is_exact(self) -> None:
        self.replace("github.triggering_actor == 'schancel'", "github.triggering_actor != ''")
        self.reject("authority predicate changed")

    def test_head_repository_must_be_same_repository(self) -> None:
        self.replace("github.event.pull_request.head.repo.full_name == github.repository", "true")
        self.reject("authority predicate changed")

    def test_head_branch_is_exact(self) -> None:
        self.replace("codex/issue-384-ci-cache-v3", "codex/**")
        self.reject("authority predicate changed")

    def test_push_trigger_is_rejected(self) -> None:
        self.replace("  pull_request:\n", "  push:\n")
        self.reject("trigger must be only pull_request")

    def test_production_cache_namespace_is_rejected(self) -> None:
        self.replace("finch-evidence-only-issue384-claim-", "sccache-local-v3-")
        self.reject("production cache namespace")

    def test_run_specific_namespace_is_required(self) -> None:
        self.replace("-run-${{ github.run_id }}-cargo-", "-cargo-")
        self.reject("escaped claim/run namespace")

    def test_fallback_restore_is_rejected(self) -> None:
        self.replace("          key: finch-evidence-only-", "          restore-keys: unsafe-\n          key: finch-evidence-only-")
        self.reject("exact-only")

    def test_mutable_action_is_rejected(self) -> None:
        self.replace("actions/cache/restore@0057852bfaa89a56745cba8c7296529d2fc39830", "actions/cache/restore@v4")
        self.reject("not an approved immutable pin")

    def test_checkout_credentials_cannot_persist(self) -> None:
        self.replace("          persist-credentials: false", "          persist-credentials: true")
        self.reject("without persisted credentials")

    def test_cargo_save_requires_miss(self) -> None:
        self.replace("        if: steps.evidence-cargo.outputs.cache-hit != 'true'\n        uses: actions/cache/save@", "        uses: actions/cache/save@")
        self.reject("Cargo evidence save must run only on exact miss")

    def test_compiler_save_requires_cap_measurement(self) -> None:
        self.replace(" && steps.sccache-stop.outputs.save-ready == 'true'", "")
        self.reject("measured cap")

    def test_compile_targets_must_be_distinct(self) -> None:
        self.replace("finch-evidence-target-second", "finch-evidence-target-first")
        self.reject("distinct run-attempt-scoped target directories")

    def test_timestamps_are_required(self) -> None:
        self.replace("first-compile-start", "first-compile")
        self.reject("timestamp first-compile-start")

    def test_helper_revalidates_triggering_actor(self) -> None:
        self.replace_helper(
            '"${GITHUB_TRIGGERING_ACTOR:-}" != "schancel"',
            '"${GITHUB_TRIGGERING_ACTOR:-}" == ""',
        )
        self.reject("GITHUB_TRIGGERING_ACTOR")


class EvidenceHelperBoundaryTests(unittest.TestCase):
    def test_helper_grants_only_exact_event_and_actor_then_scopes_override_to_child(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            workspace = Path(temporary)
            scripts = workspace / "scripts"
            scripts.mkdir()
            record = workspace / "record"
            production_helper = scripts / "configure_ci_sccache.sh"
            production_helper.write_text(
                "#!/bin/sh\nprintf '%s:%s\\n' \"$GITHUB_EVENT_NAME\" \"$GITHUB_REF\" >\"$EVIDENCE_RECORD\"\n",
                encoding="utf-8",
            )
            production_helper.chmod(0o755)
            env = os.environ | {
                "FINCH_ISSUE_384_EVIDENCE": "5f155aeb-2a15-4837-8590-833b4a4daa06",
                "GITHUB_REPOSITORY": "darwin-finch/finch",
                "GITHUB_EVENT_NAME": "pull_request",
                "GITHUB_BASE_REF": "main",
                "GITHUB_HEAD_REF": "codex/issue-384-ci-cache-v3",
                "GITHUB_ACTOR": "schancel",
                "GITHUB_TRIGGERING_ACTOR": "schancel",
                "GITHUB_WORKSPACE": str(workspace),
                "EVIDENCE_RECORD": str(record),
            }
            result = subprocess.run(
                [str(HELPER)], env=env, check=False, capture_output=True, text=True
            )
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
            self.assertEqual(record.read_text(encoding="utf-8"), "push:refs/heads/main\n")

            record.unlink()
            denied = subprocess.run(
                [str(HELPER)],
                env=env | {"GITHUB_TRIGGERING_ACTOR": "intruder"},
                check=False,
                capture_output=True,
                text=True,
            )
            self.assertEqual(denied.returncode, 1, denied.stdout + denied.stderr)
            self.assertIn("authority check failed", denied.stderr)
            self.assertFalse(record.exists(), "denied evidence helper invoked the production helper")


if __name__ == "__main__":
    unittest.main()
