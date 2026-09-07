#!/usr/bin/env python3
"""Mutation regressions for the complete-document CI workflow manifest."""

from __future__ import annotations

import json
import shutil
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parent.parent
CHECKER = ROOT / "scripts/check_ci_workflow_manifest.py"


class WorkflowRepository:
    def __init__(self) -> None:
        self.temporary = tempfile.TemporaryDirectory()
        self.root = Path(self.temporary.name)
        shutil.copytree(ROOT / ".github/workflows", self.root / ".github/workflows")
        (self.root / "scripts").mkdir()
        shutil.copy2(ROOT / "scripts/ci_workflow_manifest.json", self.root / "scripts")

    def close(self) -> None:
        self.temporary.cleanup()

    def workflow(self, name: str) -> Path:
        return self.root / ".github/workflows" / name

    def replace(self, name: str, old: str, new: str) -> None:
        path = self.workflow(name)
        contents = path.read_text(encoding="utf-8")
        if old not in contents:
            raise AssertionError(f"mutation anchor missing from {name}: {old!r}")
        path.write_text(contents.replace(old, new, 1), encoding="utf-8")

    def manifest(self) -> dict[str, object]:
        return json.loads(
            (self.root / "scripts/ci_workflow_manifest.json").read_text(encoding="utf-8")
        )

    def write_manifest(self, manifest: dict[str, object]) -> None:
        (self.root / "scripts/ci_workflow_manifest.json").write_text(
            json.dumps(manifest, indent=2, sort_keys=True) + "\n", encoding="utf-8"
        )

    def run(self) -> subprocess.CompletedProcess[str]:
        return subprocess.run(
            [sys.executable, str(CHECKER), "--root", str(self.root)],
            check=False,
            capture_output=True,
            text=True,
        )


class WorkflowManifestMutationTests(unittest.TestCase):
    def assert_accepted(self, repository: WorkflowRepository) -> None:
        result = repository.run()
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)

    def assert_rejected(
        self, repository: WorkflowRepository, *diagnostics: str
    ) -> None:
        result = repository.run()
        output = result.stdout + result.stderr
        self.assertEqual(
            result.returncode,
            1,
            f"mutated workflow contract unexpectedly passed:\n{output}",
        )
        for diagnostic in diagnostics:
            self.assertIn(
                diagnostic,
                output,
                f"rejection did not explain {diagnostic!r}:\n{output}",
            )

    def test_current_reviewed_contract_passes(self) -> None:
        repository = WorkflowRepository()
        try:
            self.assert_accepted(repository)
        finally:
            repository.close()

    def test_every_workflow_document_is_digest_guarded(self) -> None:
        names = sorted(
            path.name
            for pattern in ("*.yml", "*.yaml")
            for path in (ROOT / ".github/workflows").glob(pattern)
        )
        self.assertGreater(names, [], "the workflow mutation inventory must not be empty")
        for name in names:
            with self.subTest(workflow=name):
                repository = WorkflowRepository()
                try:
                    path = repository.workflow(name)
                    path.write_text(
                        path.read_text(encoding="utf-8")
                        + f"\nx-review-mutation: {name}\n",
                        encoding="utf-8",
                    )
                    self.assert_rejected(
                        repository,
                        name,
                        "expected=",
                        "actual=",
                        "review actual GitHub allocation",
                    )
                finally:
                    repository.close()

    def test_semantically_inert_comments_do_not_change_digest(self) -> None:
        repository = WorkflowRepository()
        try:
            path = repository.workflow("ci.yml")
            path.write_text(
                "# comment intentionally absent from semantic digest\n"
                + path.read_text(encoding="utf-8"),
                encoding="utf-8",
            )
            self.assert_accepted(repository)
        finally:
            repository.close()

    def test_base_loader_preserves_unquoted_on_as_a_string_key(self) -> None:
        repository = WorkflowRepository()
        try:
            repository.replace("ci.yml", "on:\n", '"on":\n')
            self.assert_accepted(repository)
        finally:
            repository.close()

    def test_new_yaml_workflow_is_unreviewed(self) -> None:
        repository = WorkflowRepository()
        try:
            repository.workflow("surprise.yaml").write_text(
                "name: Surprise\non: pull_request\njobs: {}\n", encoding="utf-8"
            )
            self.assert_rejected(
                repository, "surprise.yaml", "unreviewed workflow document was added"
            )
        finally:
            repository.close()

    def test_duplicate_yaml_keys_are_rejected_with_location(self) -> None:
        repository = WorkflowRepository()
        try:
            path = repository.workflow("ci.yml")
            path.write_text(path.read_text(encoding="utf-8") + "\nname: duplicate\n", encoding="utf-8")
            self.assert_rejected(repository, "ci.yml", "duplicate mapping key 'name'", "line")
        finally:
            repository.close()

    def test_duplicate_job_mapping_key_is_rejected_with_location(self) -> None:
        repository = WorkflowRepository()
        try:
            repository.replace(
                "ci.yml",
                "    runs-on: ubuntu-24.04\n",
                "    runs-on: ubuntu-24.04\n    runs-on: macos-14\n",
            )
            self.assert_rejected(repository, "ci.yml", "duplicate mapping key 'runs-on'", "line")
        finally:
            repository.close()

    def test_duplicate_matrix_mapping_key_is_rejected_with_location(self) -> None:
        repository = WorkflowRepository()
        try:
            repository.replace(
                "repository-hygiene.yml",
                "        os: [ubuntu-24.04, macos-14]\n",
                "        os: [ubuntu-24.04, macos-14]\n        os: [ubuntu-24.04]\n",
            )
            self.assert_rejected(
                repository, "repository-hygiene.yml", "duplicate mapping key 'os'", "line"
            )
        finally:
            repository.close()

    def test_duplicate_step_mapping_key_is_rejected_with_location(self) -> None:
        repository = WorkflowRepository()
        try:
            repository.replace(
                "repository-hygiene.yml",
                "      - uses: actions/checkout@v4\n",
                "      - uses: actions/checkout@v4\n        uses: actions/checkout@v3\n",
            )
            self.assert_rejected(
                repository, "repository-hygiene.yml", "duplicate mapping key 'uses'", "line"
            )
        finally:
            repository.close()

    def test_trigger_change_is_digest_rejected(self) -> None:
        repository = WorkflowRepository()
        try:
            repository.replace("ci.yml", "  pull_request:\n", "  pull_request_target:\n")
            self.assert_rejected(repository, "ci.yml", "semantic digest changed")
        finally:
            repository.close()

    def test_existing_root_environment_change_is_digest_rejected(self) -> None:
        repository = WorkflowRepository()
        try:
            repository.replace("ci.yml", "  CARGO_BUILD_JOBS: 1\n", "  CARGO_BUILD_JOBS: 2\n")
            self.assert_rejected(repository, "ci.yml", "semantic digest changed")
        finally:
            repository.close()

    def test_existing_concurrency_change_is_digest_rejected(self) -> None:
        repository = WorkflowRepository()
        try:
            repository.replace(
                "issue-56-brain-isolation.yml",
                "  cancel-in-progress: true\n",
                "  cancel-in-progress: false\n",
            )
            self.assert_rejected(
                repository, "issue-56-brain-isolation.yml", "semantic digest changed"
            )
        finally:
            repository.close()

    def test_existing_timeout_change_is_digest_rejected(self) -> None:
        repository = WorkflowRepository()
        try:
            repository.replace(
                "repository-hygiene.yml", "    timeout-minutes: 5\n", "    timeout-minutes: 6\n"
            )
            self.assert_rejected(repository, "repository-hygiene.yml", "semantic digest changed")
        finally:
            repository.close()

    def test_existing_runner_change_is_digest_rejected(self) -> None:
        repository = WorkflowRepository()
        try:
            repository.replace("ci.yml", "    runs-on: ubuntu-24.04\n", "    runs-on: ubuntu-latest\n")
            self.assert_rejected(repository, "ci.yml", "semantic digest changed")
        finally:
            repository.close()

    def test_existing_action_version_change_is_digest_rejected(self) -> None:
        repository = WorkflowRepository()
        try:
            repository.replace(
                "ci.yml", "      uses: actions/checkout@v4\n", "      uses: actions/checkout@v3\n"
            )
            self.assert_rejected(repository, "ci.yml", "semantic digest changed")
        finally:
            repository.close()

    def test_condition_change_is_digest_rejected(self) -> None:
        repository = WorkflowRepository()
        try:
            repository.replace("ci.yml", "  security:\n", "  security:\n    if: false\n")
            self.assert_rejected(repository, "ci.yml", "semantic digest changed")
        finally:
            repository.close()

    def test_matrix_change_is_digest_rejected(self) -> None:
        repository = WorkflowRepository()
        try:
            repository.replace(
                "repository-hygiene.yml",
                "        os: [ubuntu-24.04, macos-14]\n",
                "        os: [ubuntu-24.04]\n",
            )
            self.assert_rejected(repository, "repository-hygiene.yml", "semantic digest changed")
        finally:
            repository.close()

    def test_step_change_is_digest_rejected(self) -> None:
        repository = WorkflowRepository()
        try:
            repository.replace(
                "repository-hygiene.yml",
                "        run: python3 scripts/check_no_ssh_surface.py\n",
                "        run: 'true'\n",
            )
            self.assert_rejected(repository, "repository-hygiene.yml", "semantic digest changed")
        finally:
            repository.close()

    def test_permissions_change_is_digest_rejected(self) -> None:
        repository = WorkflowRepository()
        try:
            repository.replace("repository-hygiene.yml", "  contents: read\n", "  contents: write\n")
            self.assert_rejected(repository, "repository-hygiene.yml", "semantic digest changed")
        finally:
            repository.close()

    def test_defaults_change_is_digest_rejected(self) -> None:
        repository = WorkflowRepository()
        try:
            repository.replace(
                "ci.yml",
                "jobs:\n",
                "defaults:\n  run:\n    working-directory: /tmp\n\njobs:\n",
            )
            self.assert_rejected(repository, "ci.yml", "semantic digest changed")
        finally:
            repository.close()

    def test_reusable_call_change_is_digest_rejected(self) -> None:
        repository = WorkflowRepository()
        try:
            path = repository.workflow("ci.yml")
            path.write_text(
                path.read_text(encoding="utf-8")
                + "\n  delegated:\n    uses: owner/repo/.github/workflows/ci.yml@main\n    secrets: inherit\n",
                encoding="utf-8",
            )
            self.assert_rejected(repository, "ci.yml", "semantic digest changed")
        finally:
            repository.close()

    def test_deleting_retained_ssh_guard_is_digest_rejected(self) -> None:
        repository = WorkflowRepository()
        try:
            repository.replace(
                "repository-hygiene.yml",
                "      - name: Reject restoration of the removed SSH surface\n"
                "        run: python3 scripts/check_no_ssh_surface.py\n",
                "",
            )
            self.assert_rejected(repository, "repository-hygiene.yml", "semantic digest changed")
        finally:
            repository.close()

    def test_missing_reviewed_workflow_is_actionable(self) -> None:
        repository = WorkflowRepository()
        try:
            repository.workflow("issue-56-brain-isolation.yml").unlink()
            self.assert_rejected(
                repository, "issue-56-brain-isolation.yml", "reviewed workflow document is missing"
            )
        finally:
            repository.close()

    def test_fixture_check_name_change_is_rejected(self) -> None:
        repository = WorkflowRepository()
        try:
            manifest = repository.manifest()
            manifest["fixtures"]["ordinary_source"]["expected_checks"][0] = "renamed"
            repository.write_manifest(manifest)
            self.assert_rejected(repository, "ordinary_source", "expected_checks", "renamed")
        finally:
            repository.close()

    def test_fixture_budget_change_is_rejected(self) -> None:
        repository = WorkflowRepository()
        try:
            manifest = repository.manifest()
            manifest["fixtures"]["brain_effect"]["expected_count"] = 16
            repository.write_manifest(manifest)
            self.assert_rejected(repository, "brain_effect", "expected_count", "16")
        finally:
            repository.close()

    def test_pr_active_inventory_change_is_rejected(self) -> None:
        repository = WorkflowRepository()
        try:
            manifest = repository.manifest()
            manifest["pr_active_workflows"].remove("repository-hygiene.yml")
            repository.write_manifest(manifest)
            self.assert_rejected(repository, "PR-active workflow inventory changed")
        finally:
            repository.close()

    def test_workflow_fixture_membership_change_is_rejected(self) -> None:
        repository = WorkflowRepository()
        try:
            manifest = repository.manifest()
            manifest["workflows"]["ci.yml"]["activated_fixtures"] = []
            repository.write_manifest(manifest)
            self.assert_rejected(repository, "ci.yml fixture inventory changed")
        finally:
            repository.close()

    def test_unknown_manifest_field_is_rejected(self) -> None:
        repository = WorkflowRepository()
        try:
            manifest = repository.manifest()
            manifest["unreviewed_override"] = True
            repository.write_manifest(manifest)
            self.assert_rejected(repository, "root keys changed", "unreviewed_override")
        finally:
            repository.close()

    def test_missing_schema_version_is_rejected(self) -> None:
        repository = WorkflowRepository()
        try:
            manifest = repository.manifest()
            del manifest["schema"]
            repository.write_manifest(manifest)
            self.assert_rejected(repository, "root keys changed", "schema")
        finally:
            repository.close()

    def test_wrong_schema_version_is_rejected(self) -> None:
        repository = WorkflowRepository()
        try:
            manifest = repository.manifest()
            manifest["schema"] = "finch-ci-workflow-manifest:v2"
            repository.write_manifest(manifest)
            self.assert_rejected(repository, "schema must be", "v2")
        finally:
            repository.close()

    def test_non_string_schema_version_is_rejected(self) -> None:
        repository = WorkflowRepository()
        try:
            manifest = repository.manifest()
            manifest["schema"] = 1
            repository.write_manifest(manifest)
            self.assert_rejected(repository, "schema must be", "1")
        finally:
            repository.close()

    def test_non_object_workflow_record_is_rejected(self) -> None:
        repository = WorkflowRepository()
        try:
            manifest = repository.manifest()
            manifest["workflows"]["ci.yml"] = "not-an-object"
            repository.write_manifest(manifest)
            self.assert_rejected(repository, "ci.yml", "must be an object")
        finally:
            repository.close()

    def test_non_string_workflow_digest_is_rejected(self) -> None:
        repository = WorkflowRepository()
        try:
            manifest = repository.manifest()
            manifest["workflows"]["ci.yml"]["digest"] = 42
            repository.write_manifest(manifest)
            self.assert_rejected(repository, "ci.yml", "invalid SHA-256 digest", "42")
        finally:
            repository.close()


if __name__ == "__main__":
    unittest.main(verbosity=2)
