#!/usr/bin/env python3
"""Production-boundary mutations for the exact workflow inventory contract."""

from __future__ import annotations

import json
import shutil
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

from check_ci_workflow_manifest import (
    ContractError,
    MAX_MANIFEST_BYTES,
    MAX_TOTAL_WORKFLOW_BYTES,
    MAX_WORKFLOW_BYTES,
    MAX_WORKFLOW_FILES,
    workflow_records,
)


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

    def manifest_path(self) -> Path:
        return self.root / "scripts/ci_workflow_manifest.json"

    def manifest(self) -> dict[str, object]:
        return json.loads(self.manifest_path().read_text(encoding="utf-8"))

    def write_manifest(self, manifest: dict[str, object]) -> None:
        self.manifest_path().write_text(
            json.dumps(manifest, indent=2, sort_keys=True) + "\n", encoding="utf-8"
        )

    def run(self) -> subprocess.CompletedProcess[str]:
        return subprocess.run(
            [sys.executable, str(CHECKER), "--root", str(self.root)],
            check=False,
            capture_output=True,
            text=True,
            timeout=10,
        )


class WorkflowManifestTests(unittest.TestCase):
    def assert_accepted(self, repository: WorkflowRepository) -> None:
        result = repository.run()
        self.assertEqual(
            result.returncode,
            0,
            "reviewed workflow inventory failed: "
            f"stdout={result.stdout!r} stderr={result.stderr!r}",
        )

    def assert_rejected(self, repository: WorkflowRepository, *diagnostics: str) -> None:
        result = repository.run()
        output = result.stdout + result.stderr
        self.assertEqual(
            result.returncode,
            1,
            f"mutated workflow inventory unexpectedly passed: output={output!r}",
        )
        for diagnostic in diagnostics:
            self.assertIn(
                diagnostic,
                output,
                f"rejection omitted diagnostic {diagnostic!r}: output={output!r}",
            )

    def test_current_reviewed_inventory_passes(self) -> None:
        repository = WorkflowRepository()
        try:
            self.assert_accepted(repository)
        finally:
            repository.close()

    def test_every_workflow_byte_change_requires_allocation_review(self) -> None:
        names = sorted(path.name for path in (ROOT / ".github/workflows").glob("*.y*ml"))
        self.assertGreater(names, [], "workflow inventory must contain reviewed files")
        for name in names:
            with self.subTest(workflow=name):
                repository = WorkflowRepository()
                try:
                    path = repository.workflow(name)
                    path.write_text(path.read_text(encoding="utf-8") + "\n# mutation\n")
                    self.assert_rejected(
                        repository,
                        name,
                        "reviewed sha256 changed",
                        "review actual GitHub allocation",
                    )
                finally:
                    repository.close()

    def test_new_yml_and_yaml_workflows_are_unreviewed(self) -> None:
        for name in ("surprise.yml", "surprise.yaml"):
            with self.subTest(name=name):
                repository = WorkflowRepository()
                try:
                    repository.workflow(name).write_text("name: surprise\n", encoding="utf-8")
                    self.assert_rejected(repository, name, "unreviewed workflow file was added")
                finally:
                    repository.close()

    def test_missing_reviewed_workflow_is_actionable(self) -> None:
        repository = WorkflowRepository()
        try:
            repository.workflow("issue-186-ssh-removal.yml").unlink()
            self.assert_rejected(repository, "issue-186-ssh-removal.yml", "is missing")
        finally:
            repository.close()

    def test_symlinked_workflow_is_rejected_before_hashing_target(self) -> None:
        repository = WorkflowRepository()
        try:
            workflow = repository.workflow("ci.yml")
            target = repository.root / "scripts/ci.yml"
            workflow.rename(target)
            workflow.symlink_to("../../scripts/ci.yml")
            self.assert_rejected(repository, ".github/workflows/ci.yml", "not a symbolic link")
        finally:
            repository.close()

    def test_symlinked_workflow_directory_is_rejected(self) -> None:
        repository = WorkflowRepository()
        try:
            directory = repository.root / ".github/workflows"
            target = repository.root / "workflow-target"
            directory.rename(target)
            directory.symlink_to("../workflow-target")
            self.assert_rejected(repository, ".github/workflows", "real directory", "not a link")
        finally:
            repository.close()

    def test_nonregular_workflow_is_rejected(self) -> None:
        repository = WorkflowRepository()
        try:
            repository.workflow("surprise.yml").mkdir()
            self.assert_rejected(repository, "surprise.yml", "must be a regular file")
        finally:
            repository.close()

    def test_oversized_workflow_is_rejected_before_hashing(self) -> None:
        repository = WorkflowRepository()
        try:
            path = repository.workflow("surprise.yml")
            path.write_bytes(b"x" * (MAX_WORKFLOW_BYTES + 1))
            self.assert_rejected(
                repository, "surprise.yml", str(MAX_WORKFLOW_BYTES + 1), "exceeds the reviewed"
            )
        finally:
            repository.close()

    def test_aggregate_workflow_bytes_are_bounded_before_inventory_comparison(self) -> None:
        repository = WorkflowRepository()
        try:
            for index in range(8):
                repository.workflow(f"padding-{index}.yml").write_bytes(
                    b"x" * MAX_WORKFLOW_BYTES
                )
            self.assert_rejected(
                repository,
                ".github/workflows",
                "aggregate bytes exceeds",
                str(MAX_TOTAL_WORKFLOW_BYTES),
            )
        finally:
            repository.close()

    def test_workflow_count_is_bounded_before_inventory_comparison(self) -> None:
        repository = WorkflowRepository()
        try:
            current_count = len(list((repository.root / ".github/workflows").glob("*.y*ml")))
            for index in range(MAX_WORKFLOW_FILES - current_count + 1):
                repository.workflow(f"empty-{index}.yml").touch()
            self.assert_rejected(
                repository,
                "workflow count exceeds",
                str(MAX_WORKFLOW_FILES),
            )
        finally:
            repository.close()

    def test_entry_added_after_enumeration_is_rejected(self) -> None:
        repository = WorkflowRepository()
        try:
            def add_workflow() -> None:
                repository.workflow("surprise.yml").write_text(
                    "name: surprise\n", encoding="utf-8"
                )

            with self.assertRaisesRegex(
                ContractError, "directory identity changed|entry set changed"
            ):
                workflow_records(repository.root, phase_hook=add_workflow)
        finally:
            repository.close()

    def test_leaf_replaced_after_enumeration_is_rejected(self) -> None:
        repository = WorkflowRepository()
        try:
            workflow = repository.workflow("ci.yml")
            reviewed_bytes = workflow.read_bytes()

            def replace_leaf() -> None:
                workflow.unlink()
                workflow.write_bytes(reviewed_bytes)

            with self.assertRaisesRegex(ContractError, "identity changed after enumeration"):
                workflow_records(repository.root, phase_hook=replace_leaf)
        finally:
            repository.close()

    def test_leaf_replaced_after_hashing_is_rejected(self) -> None:
        repository = WorkflowRepository()
        try:
            workflow = repository.workflow("ci.yml")
            reviewed_bytes = workflow.read_bytes()

            def replace_leaf() -> None:
                workflow.write_bytes(b"#" + reviewed_bytes[1:])

            with self.assertRaisesRegex(ContractError, "identity changed after hashing"):
                workflow_records(repository.root, after_hash_hook=replace_leaf)
        finally:
            repository.close()

    def test_workflow_directory_replaced_after_enumeration_is_rejected(self) -> None:
        repository = WorkflowRepository()
        try:
            directory = repository.root / ".github/workflows"
            target = repository.root / "workflow-target"

            def replace_directory() -> None:
                directory.rename(target)
                directory.symlink_to("../workflow-target")

            with self.assertRaisesRegex(ContractError, "real directory"):
                workflow_records(repository.root, phase_hook=replace_directory)
        finally:
            repository.close()

    def test_authoritative_opened_sizes_enforce_aggregate_bound(self) -> None:
        repository = WorkflowRepository()
        try:
            paths = sorted((repository.root / ".github/workflows").glob("*.y*ml"))
            initial_size = 65_500
            self.assertEqual(
                len(paths),
                16,
                f"aggregate-race fixture assumes 16 reviewed workflows: paths={paths!r}",
            )
            for path in paths:
                path.write_bytes(b"x" * initial_size)

            def grow_workflows() -> None:
                paths[-1].write_bytes(b"x" * (initial_size + 1_000))

            with self.assertRaisesRegex(ContractError, "aggregate opened bytes exceeds"):
                workflow_records(repository.root, phase_hook=grow_workflows)
        finally:
            repository.close()

    def test_symlinked_manifest_is_rejected(self) -> None:
        repository = WorkflowRepository()
        try:
            manifest = repository.manifest_path()
            target = repository.root / "manifest.json"
            manifest.rename(target)
            manifest.symlink_to("../manifest.json")
            self.assert_rejected(repository, "ci_workflow_manifest.json", "not a symbolic link")
        finally:
            repository.close()

    def test_nonregular_manifest_is_rejected(self) -> None:
        repository = WorkflowRepository()
        try:
            repository.manifest_path().unlink()
            repository.manifest_path().mkdir()
            self.assert_rejected(repository, "ci_workflow_manifest.json", "must be a regular file")
        finally:
            repository.close()

    def test_oversized_manifest_is_rejected_before_json_loading(self) -> None:
        repository = WorkflowRepository()
        try:
            repository.manifest_path().write_bytes(b" " * (MAX_MANIFEST_BYTES + 1))
            self.assert_rejected(
                repository,
                "ci_workflow_manifest.json",
                str(MAX_MANIFEST_BYTES + 1),
                "exceeds the reviewed",
            )
        finally:
            repository.close()

    def test_duplicate_manifest_key_is_rejected(self) -> None:
        repository = WorkflowRepository()
        try:
            path = repository.manifest_path()
            contents = path.read_text(encoding="utf-8")
            path.write_text(
                contents.replace(
                    '{\n  "schema"', '{\n  "schema": "duplicate",\n  "schema"', 1
                )
            )
            self.assert_rejected(repository, "duplicate JSON key", "schema")
        finally:
            repository.close()

    def test_large_manifest_integer_has_actionable_diagnostic(self) -> None:
        repository = WorkflowRepository()
        try:
            path = repository.manifest_path()
            contents = path.read_text(encoding="utf-8")
            path.write_text(
                contents.replace(
                    '"max_workflow_bytes": 131072',
                    '"max_workflow_bytes": ' + "9" * 5_000,
                    1,
                ),
                encoding="utf-8",
            )
            self.assert_rejected(
                repository,
                "ci_workflow_manifest.json",
                "reviewed manifest could not be loaded",
            )
        finally:
            repository.close()

    def test_manifest_root_and_schema_are_exact(self) -> None:
        mutations = (
            ("schema", "finch-ci-workflow-manifest:v999", "schema must be"),
            ("unexpected", True, "root keys changed"),
        )
        for key, value, diagnostic in mutations:
            with self.subTest(key=key):
                repository = WorkflowRepository()
                try:
                    manifest = repository.manifest()
                    manifest[key] = value
                    repository.write_manifest(manifest)
                    self.assert_rejected(repository, diagnostic)
                finally:
                    repository.close()

    def test_reviewed_limits_cannot_drift(self) -> None:
        repository = WorkflowRepository()
        try:
            manifest = repository.manifest()
            manifest["limits"]["max_workflow_bytes"] += 1
            repository.write_manifest(manifest)
            self.assert_rejected(repository, "reviewed limits changed", "max_workflow_bytes")
        finally:
            repository.close()

    def test_workflow_record_fields_are_exact(self) -> None:
        for field, value in (("sha256", "0" * 64), ("bytes", 1), ("activated_fixtures", [])):
            with self.subTest(field=field):
                repository = WorkflowRepository()
                try:
                    manifest = repository.manifest()
                    manifest["workflows"]["ci.yml"][field] = value
                    repository.write_manifest(manifest)
                    self.assert_rejected(repository, "ci.yml", f"reviewed {field} changed")
                finally:
                    repository.close()

    def test_workflow_record_schema_is_exact(self) -> None:
        mutations = (
            ("extra", {"approved_override": True}),
            ("missing", {"sha256": None}),
            ("non-object", "invalid"),
        )
        for name, mutation in mutations:
            with self.subTest(mutation=name):
                repository = WorkflowRepository()
                try:
                    manifest = repository.manifest()
                    record = manifest["workflows"]["ci.yml"]
                    if name == "extra":
                        record.update(mutation)
                    elif name == "missing":
                        record.pop("sha256")
                    else:
                        manifest["workflows"]["ci.yml"] = mutation
                    repository.write_manifest(manifest)
                    self.assert_rejected(repository, "workflow record", "ci.yml")
                finally:
                    repository.close()

    def test_fixture_inventory_change_is_rejected(self) -> None:
        repository = WorkflowRepository()
        try:
            manifest = repository.manifest()
            manifest["fixtures"]["ordinary_source"]["expected_count"] = 34
            repository.write_manifest(manifest)
            self.assert_rejected(repository, "representative fixture inventories changed")
        finally:
            repository.close()

    def test_pr_active_inventory_change_is_rejected(self) -> None:
        repository = WorkflowRepository()
        try:
            manifest = repository.manifest()
            manifest["pr_active_workflows"].remove("ci.yml")
            repository.write_manifest(manifest)
            self.assert_rejected(repository, "PR-active workflow inventory changed")
        finally:
            repository.close()


if __name__ == "__main__":
    unittest.main(verbosity=2)
