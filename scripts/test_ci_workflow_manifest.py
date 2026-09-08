#!/usr/bin/env python3
"""Production-boundary mutations for the exact workflow inventory contract."""

from __future__ import annotations

import errno
import hashlib
import io
import json
import os
import shutil
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path
from unittest import mock

from check_ci_workflow_manifest import (
    ContractError,
    HASH_CHUNK_BYTES,
    MAX_MANIFEST_BYTES,
    MAX_TOTAL_WORKFLOW_BYTES,
    MAX_WORKFLOW_BYTES,
    MAX_WORKFLOW_DIRECTORY_ENTRIES,
    MAX_WORKFLOW_FILES,
    WORKFLOW_DIRECTORY_FD_SUPPORTED,
    bounded_workflow_names,
    capture_canonical_workflow_stream,
    compare_contract,
    hash_canonical_workflow_stream,
    read_exact_bytes,
    manifest_snapshot,
    workflow_snapshot,
    workflow_records,
)


ROOT = Path(__file__).resolve().parent.parent
CHECKER = ROOT / "scripts/check_ci_workflow_manifest.py"


class PullBoundIterator:
    def __init__(self, names: list[str]) -> None:
        self.names = names
        self.pulls = 0

    def __iter__(self):
        return self

    def __next__(self):
        if self.pulls == len(self.names):
            raise AssertionError(
                "bounded workflow enumeration pulled after its first excess element: "
                f"pulls={self.pulls} names={len(self.names)}"
            )
        name = self.names[self.pulls]
        self.pulls += 1
        return type("Entry", (), {"name": name})()


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

    def git(self, *arguments: str) -> subprocess.CompletedProcess[str]:
        return subprocess.run(
            ["git", *arguments],
            cwd=self.root,
            check=True,
            capture_output=True,
            text=True,
            timeout=10,
        )


class WorkflowManifestTests(unittest.TestCase):
    def write_snapshot_source(
        self,
        root: Path,
        workflows: dict[str, bytes] | None = None,
        manifest_bytes: bytes = b"{}\n",
    ) -> None:
        workflow_directory = root / ".github/workflows"
        workflow_directory.mkdir(parents=True)
        for name, contents in (workflows or {"synthetic.yml": b"name: fixture\n"}).items():
            (workflow_directory / name).write_bytes(contents)
        scripts = root / "scripts"
        scripts.mkdir()
        (scripts / "ci_workflow_manifest.json").write_bytes(manifest_bytes)

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

    def assert_lstat_open_symlink_swap_rejected(
        self,
        repository: WorkflowRepository,
        path: Path,
        target: Path,
        link_target: str,
        kind: str,
    ) -> None:
        swapped = False

        def swap_to_same_inode_symlink(opened_path: Path) -> None:
            nonlocal swapped
            if swapped or opened_path != path:
                return
            opened_path.rename(target)
            opened_path.symlink_to(link_target)
            swapped = True

        def restore_path(opened_path: Path) -> None:
            if opened_path != path or not opened_path.is_symlink():
                return
            opened_path.unlink()
            target.rename(opened_path)

        hooks = {
            f"{kind}_before_open_hook": swap_to_same_inode_symlink,
            f"{kind}_after_open_hook": restore_path,
        }
        errors = compare_contract(repository.root, **hooks)
        self.assertTrue(
            swapped,
            f"{kind} race hook must replace the lstat-validated path before open: "
            f"path={path} target={target}",
        )
        self.assertTrue(
            any("regular file could not be opened safely" in error for error in errors),
            "O_NOFOLLOW must reject an lstat-to-open symlink swap even when the link "
            "resolves to the same reviewed inode; removing O_NOFOLLOW would let the "
            f"restore hook conceal the race: kind={kind} path={path} errors={errors!r}",
        )

    def test_current_reviewed_inventory_passes(self) -> None:
        repository = WorkflowRepository()
        try:
            self.assert_accepted(repository)
        finally:
            repository.close()

    def test_per_resource_snapshots_enforce_all_five_bounds(self) -> None:
        cases = (
            (
                "directory entries",
                {"synthetic.yml": b"name: fixture\n"},
                MAX_WORKFLOW_DIRECTORY_ENTRIES,
                "directory entry count exceeds",
            ),
            (
                "workflow count",
                {
                    f"workflow-{index}.yml": b"name: fixture\n"
                    for index in range(MAX_WORKFLOW_FILES + 1)
                },
                0,
                "workflow count exceeds",
            ),
            (
                "per-workflow bytes",
                {"oversized.yml": b"x" * (MAX_WORKFLOW_BYTES + 1)},
                0,
                "exceeds the reviewed",
            ),
            (
                "aggregate workflow bytes",
                {
                    f"workflow-{index}.yml": b"x" * MAX_WORKFLOW_BYTES
                    for index in range(9)
                },
                0,
                "aggregate bytes exceeds",
            ),
        )
        for label, workflows, ignored_entries, diagnostic in cases:
            with self.subTest(bound=label), tempfile.TemporaryDirectory() as name:
                root = Path(name)
                self.write_snapshot_source(root, workflows)
                directory = root / ".github/workflows"
                for index in range(ignored_entries):
                    (directory / f"ignored-{index}.txt").touch()
                with self.assertRaisesRegex(
                    ContractError,
                    diagnostic,
                    msg=(
                        "workflow_snapshot must enforce each source bound independently: "
                        f"bound={label} root={root} workflows={len(workflows)} "
                        f"ignored_entries={ignored_entries}"
                    ),
                ):
                    workflow_snapshot(root)

        with tempfile.TemporaryDirectory() as name:
            root = Path(name)
            manifest_bytes = b'{"padding":"' + b"x" * MAX_MANIFEST_BYTES + b'"}\n'
            self.write_snapshot_source(root, manifest_bytes=manifest_bytes)
            with self.assertRaisesRegex(
                ContractError,
                "exceeds the reviewed",
                msg=(
                    "manifest_snapshot must enforce its byte bound before reading: "
                    f"root={root} size={len(manifest_bytes)} limit={MAX_MANIFEST_BYTES}"
                ),
            ):
                manifest_snapshot(root)

    def test_bounded_enumeration_stops_at_each_first_excess_element(self) -> None:
        for label, names, limit, diagnostic in (
            (
                "directory entries",
                [
                    f"ignored-{index}.txt"
                    for index in range(MAX_WORKFLOW_DIRECTORY_ENTRIES + 1)
                ],
                MAX_WORKFLOW_DIRECTORY_ENTRIES,
                "directory entry count exceeds",
            ),
            (
                "workflow count",
                [f"workflow-{index}.yml" for index in range(MAX_WORKFLOW_FILES + 1)],
                MAX_WORKFLOW_FILES,
                "workflow count exceeds",
            ),
        ):
            with self.subTest(bound=label):
                entries = PullBoundIterator(names)
                with self.assertRaisesRegex(
                    ContractError,
                    diagnostic,
                    msg=f"enumeration must reject the first excess {label}: limit={limit}",
                ):
                    bounded_workflow_names(entries)
                self.assertEqual(
                    entries.pulls,
                    limit + 1,
                    "bounded enumeration must not pull after its first rejected element: "
                    f"bound={label} pulls={entries.pulls} limit={limit}",
                )

    def test_per_resource_snapshots_preserve_exact_raw_and_canonical_bytes(self) -> None:
        with tempfile.TemporaryDirectory() as name:
            root = Path(name)
            workflow_raw = b"name: fixture\r\non:\r\n  push:\r\n"
            manifest_raw = b'{"schema":"fixture"}\n'
            self.write_snapshot_source(
                root, {"synthetic.yml": workflow_raw}, manifest_raw
            )
            workflow_bytes, records = workflow_snapshot(root)
            captured_manifest, manifest, _ = manifest_snapshot(root)
            canonical = workflow_raw.replace(b"\r\n", b"\n")
            self.assertEqual(
                workflow_bytes,
                {"synthetic.yml": workflow_raw},
                "workflow_snapshot must preserve exact physical source bytes: "
                f"root={root} captured={workflow_bytes!r}",
            )
            self.assertEqual(
                records["synthetic.yml"]["sha256"],
                hashlib.sha256(canonical).hexdigest(),
                "workflow record digest must use canonical LF bytes while retaining raw "
                f"capture: raw={workflow_raw!r} canonical={canonical!r} records={records!r}",
            )
            self.assertEqual(
                records["synthetic.yml"]["bytes"],
                len(canonical),
                "workflow record byte count must describe canonical LF bytes: "
                f"canonical={canonical!r} records={records!r}",
            )
            self.assertEqual(
                (captured_manifest, manifest),
                (manifest_raw, {"schema": "fixture"}),
                "manifest_snapshot must return exact raw bytes and their parsed object: "
                f"raw={captured_manifest!r} manifest={manifest!r}",
            )

    def test_manifest_snapshot_rejects_path_replacement_after_open(self) -> None:
        with tempfile.TemporaryDirectory() as name:
            root = Path(name)
            self.write_snapshot_source(root, manifest_bytes=b'{"source":"opened"}\n')
            manifest = root / "scripts/ci_workflow_manifest.json"
            replacement = root / "scripts/manifest-replacement.json"
            replacement.write_bytes(b'{"source":"replacement"}\n')
            opened_inode = manifest.stat().st_ino
            replacement_inode = replacement.stat().st_ino

            def replace_manifest(opened_path: Path) -> None:
                self.assertEqual(
                    opened_path,
                    manifest,
                    "manifest replacement hook must target the opened manifest path: "
                    f"opened={opened_path} expected={manifest}",
                )
                os.replace(replacement, manifest)

            with self.assertRaisesRegex(
                ContractError,
                "file identity changed while capturing manifest",
                msg=(
                    "manifest_snapshot must reject a distinct live pathname replacement "
                    "after reading the originally opened descriptor: "
                    f"manifest={manifest} opened_inode={opened_inode} "
                    f"replacement_inode={replacement_inode}"
                ),
            ):
                manifest_snapshot(root, after_open_hook=replace_manifest)

    def test_workflow_snapshot_never_opens_ignored_nonworkflow_link(self) -> None:
        with tempfile.TemporaryDirectory() as name:
            root = Path(name)
            self.write_snapshot_source(root)
            target = root / "tiny-ignored-target.txt"
            target.write_bytes(b"forbidden target content\n")
            link = root / ".github/workflows/ignored.txt"
            link.symlink_to(target)
            real_open = os.open
            opened: list[str] = []

            def record_open(path, flags, *args, **kwargs):
                opened.append(os.fspath(path))
                return real_open(path, flags, *args, **kwargs)

            with mock.patch(
                "check_ci_workflow_manifest.os.open", side_effect=record_open
            ):
                workflow_bytes, _ = workflow_snapshot(root)
            forbidden = {str(link), link.name, str(target), target.name}
            forbidden_opens = [path for path in opened if path in forbidden]
            self.assertEqual(
                workflow_bytes,
                {"synthetic.yml": b"name: fixture\n"},
                "ignored non-workflow links must not enter the captured workflow set: "
                f"link={link} captured={workflow_bytes!r}",
            )
            self.assertEqual(
                forbidden_opens,
                [],
                "workflow_snapshot must filter an ignored non-workflow link before opening "
                "either its lexical path or target content: "
                f"link={link} target={target} opens={opened!r}",
            )

    @unittest.skipUnless(
        WORKFLOW_DIRECTORY_FD_SUPPORTED,
        "directory-relative workflow pinning is unavailable on this platform",
    )
    def test_workflow_snapshot_parent_replacement_never_captures_target_bytes(self) -> None:
        with tempfile.TemporaryDirectory() as name:
            root = Path(name)
            reviewed = b"name: reviewed source\n"
            forbidden = b"name: replacement source\n"
            self.write_snapshot_source(root, {"synthetic.yml": reviewed})
            directory = root / ".github/workflows"
            replacement = root / ".github/workflows-replacement"
            replacement.mkdir()
            (replacement / "synthetic.yml").write_bytes(forbidden)
            captured: list[bytes] = []

            def replace_parent() -> None:
                original = root / ".github/workflows-original"
                directory.rename(original)
                os.replace(replacement, directory)

            def record_capture(stream, expected_size: int, display: str):
                contents, digest, canonical_size = capture_canonical_workflow_stream(
                    stream, expected_size, display
                )
                captured.append(contents)
                return contents, digest, canonical_size

            with mock.patch(
                "check_ci_workflow_manifest.capture_canonical_workflow_stream",
                side_effect=record_capture,
            ):
                with self.assertRaisesRegex(
                    ContractError,
                    "directory identity changed|file identity changed",
                    msg=(
                        "replacing the workflow parent after pinned enumeration must reject "
                        "without capturing replacement bytes: "
                        f"directory={directory} replacement={replacement}"
                    ),
                ):
                    workflow_snapshot(root, phase_hook=replace_parent)
            self.assertIn(
                reviewed,
                captured,
                "the pinned directory descriptor must capture the reviewed directory inode: "
                f"reviewed={reviewed!r} captured={captured!r}",
            )
            self.assertNotIn(
                forbidden,
                captured,
                "a replaced parent pathname must never redirect workflow capture: "
                f"forbidden={forbidden!r} captured={captured!r}",
            )

    @unittest.skipUnless(
        WORKFLOW_DIRECTORY_FD_SUPPORTED and hasattr(os, "link"),
        "directory-relative workflow pinning and hard links are required",
    )
    def test_final_workflow_validation_rejects_same_leaf_new_directory(self) -> None:
        with tempfile.TemporaryDirectory() as name:
            root = Path(name)
            self.write_snapshot_source(root)
            directory = root / ".github/workflows"
            original_leaf = directory / "synthetic.yml"
            replacement = root / ".github/workflows-replacement"
            replacement.mkdir()
            os.link(original_leaf, replacement / original_leaf.name)
            original_directory_inode = directory.stat().st_ino
            replacement_directory_inode = replacement.stat().st_ino

            def replace_after_final_scan() -> None:
                original = root / ".github/workflows-original"
                directory.rename(original)
                os.replace(replacement, directory)

            with self.assertRaisesRegex(
                ContractError,
                "directory identity changed while checking",
                msg=(
                    "final validation must scan and stat through the retained directory fd, "
                    "then reject a new live directory pathname even when every leaf is the "
                    "same hard-linked inode: "
                    f"original_inode={original_directory_inode} "
                    f"replacement_inode={replacement_directory_inode}"
                ),
            ):
                workflow_snapshot(root, final_scan_hook=replace_after_final_scan)

    def test_aggregate_exact_first_excess_rejects_before_leaf_capture(self) -> None:
        with tempfile.TemporaryDirectory() as name:
            root = Path(name)
            workflows = {
                f"base-{index}.yml": b"x" * MAX_WORKFLOW_BYTES for index in range(8)
            }
            workflows["zz-crossing.yml"] = b""
            self.write_snapshot_source(root, workflows)
            crossing = root / ".github/workflows/zz-crossing.yml"
            captured: list[str] = []

            def grow_to_first_excess() -> None:
                crossing.write_bytes(b"x")

            def record_capture(stream, expected_size: int, display: str):
                captured.append(display)
                return capture_canonical_workflow_stream(stream, expected_size, display)

            with mock.patch(
                "check_ci_workflow_manifest.capture_canonical_workflow_stream",
                side_effect=record_capture,
            ):
                with self.assertRaisesRegex(
                    ContractError,
                    "1048577 aggregate opened bytes exceeds.*before capturing.*zz-crossing",
                    msg=(
                        "workflow_snapshot must reject exactly the first aggregate excess "
                        "byte before capturing its leaf: "
                        f"limit={MAX_TOTAL_WORKFLOW_BYTES} crossing={crossing} "
                        f"captured={captured!r}"
                    ),
                ):
                    workflow_snapshot(root, phase_hook=grow_to_first_excess)
            self.assertNotIn(
                ".github/workflows/zz-crossing.yml",
                captured,
                "the leaf crossing the aggregate bound by one byte must not be captured: "
                f"crossing={crossing} captured={captured!r}",
            )

    def test_workflow_snapshot_fails_closed_without_directory_fd_support(self) -> None:
        with tempfile.TemporaryDirectory() as name:
            root = Path(name)
            self.write_snapshot_source(root)
            with mock.patch(
                "check_ci_workflow_manifest.WORKFLOW_DIRECTORY_FD_SUPPORTED", False
            ), mock.patch(
                "check_ci_workflow_manifest.os.scandir",
                side_effect=AssertionError("unsupported platform enumerated source children"),
            ) as scandir:
                with self.assertRaisesRegex(
                    ContractError,
                    "cannot identity-pin the workflow directory.*refusing an unsafe",
                    msg=(
                        "workflow_snapshot must fail closed before enumeration when "
                        f"directory-relative descriptor pinning is unavailable: root={root}"
                    ),
                ):
                    workflow_snapshot(root)
            scandir.assert_not_called()

    @unittest.skipUnless(
        WORKFLOW_DIRECTORY_FD_SUPPORTED,
        "directory-relative workflow pinning is unavailable on this platform",
    )
    def test_workflow_snapshot_closes_its_owned_directory_fd(self) -> None:
        with tempfile.TemporaryDirectory() as name:
            root = Path(name)
            self.write_snapshot_source(root)
            directory = root / ".github/workflows"
            real_open = os.open
            directory_fds: list[int] = []

            def record_open(path, flags, *args, **kwargs):
                descriptor = real_open(path, flags, *args, **kwargs)
                if os.fspath(path) == os.fspath(directory):
                    directory_fds.append(descriptor)
                return descriptor

            with mock.patch(
                "check_ci_workflow_manifest.os.open", side_effect=record_open
            ):
                workflow_snapshot(root)
            self.assertGreaterEqual(
                len(directory_fds),
                1,
                "workflow_snapshot must acquire an owned directory descriptor: "
                f"directory={directory} descriptors={directory_fds!r}",
            )
            with self.assertRaises(OSError) as raised:
                os.fstat(directory_fds[0])
            self.assertEqual(
                raised.exception.errno,
                errno.EBADF,
                "workflow_snapshot must close its owned directory descriptor on success: "
                f"directory={directory} descriptor={directory_fds[0]}",
            )

    @unittest.skipUnless(
        WORKFLOW_DIRECTORY_FD_SUPPORTED,
        "directory-relative workflow pinning is unavailable on this platform",
    )
    def test_workflow_snapshot_closes_owned_directory_fd_after_rejection(self) -> None:
        with tempfile.TemporaryDirectory() as name:
            root = Path(name)
            self.write_snapshot_source(
                root, {"oversized.yml": b"x" * (MAX_WORKFLOW_BYTES + 1)}
            )
            directory = root / ".github/workflows"
            real_open = os.open
            directory_fds: list[int] = []

            def record_open(path, flags, *args, **kwargs):
                descriptor = real_open(path, flags, *args, **kwargs)
                if os.fspath(path) == os.fspath(directory):
                    directory_fds.append(descriptor)
                return descriptor

            with mock.patch(
                "check_ci_workflow_manifest.os.open", side_effect=record_open
            ):
                with self.assertRaisesRegex(
                    ContractError,
                    "exceeds the reviewed",
                    msg=(
                        "failure-path fd cleanup fixture must reject after acquiring the "
                        f"workflow directory descriptor: directory={directory}"
                    ),
                ):
                    workflow_snapshot(root)
            self.assertGreaterEqual(
                len(directory_fds),
                1,
                "rejection must occur after workflow_snapshot acquires its owned directory "
                f"descriptor: directory={directory} descriptors={directory_fds!r}",
            )
            with self.assertRaises(OSError) as raised:
                os.fstat(directory_fds[0])
            self.assertEqual(
                raised.exception.errno,
                errno.EBADF,
                "workflow_snapshot must close its owned directory descriptor after a "
                f"ContractError: directory={directory} descriptor={directory_fds[0]}",
            )

    def test_workflow_yaml_checkouts_are_normalized_to_lf(self) -> None:
        for name in ("ci.yml", "synthetic.yaml"):
            with self.subTest(workflow=name):
                result = subprocess.run(
                    [
                        "git",
                        "check-attr",
                        "text",
                        "eol",
                        "--",
                        f".github/workflows/{name}",
                    ],
                    cwd=ROOT,
                    check=False,
                    capture_output=True,
                    text=True,
                    timeout=10,
                )
                self.assertEqual(
                    result.returncode,
                    0,
                    "workflow EOL attribute lookup failed: "
                    f"workflow={name} stdout={result.stdout!r} stderr={result.stderr!r}",
                )
                self.assertIn(
                    f".github/workflows/{name}: text: set",
                    result.stdout,
                    "workflow YAML must be declared text for Git checkout normalization: "
                    f"workflow={name} attributes={result.stdout!r}",
                )
                self.assertIn(
                    f".github/workflows/{name}: eol: lf",
                    result.stdout,
                    "workflow YAML exact-byte hashes require LF in every Git checkout: "
                    f"workflow={name} attributes={result.stdout!r}",
                )

    def test_git_clean_autocrlf_checkout_from_pre_attributes_base_is_accepted(self) -> None:
        repository = WorkflowRepository()
        try:
            attributes = repository.root / ".gitattributes"
            repository.git("init", "--quiet")
            for key, value in (
                ("user.name", "Workflow Manifest Test"),
                ("user.email", "workflow-manifest@example.invalid"),
                ("core.autocrlf", "true"),
            ):
                repository.git("config", key, value)
            repository.git(
                "add", ".github/workflows", "scripts/ci_workflow_manifest.json"
            )
            repository.git(
                "commit", "--quiet", "-m", "base before workflow attributes"
            )
            shutil.rmtree(repository.root / ".github/workflows")
            repository.git("checkout", "--", ".github/workflows")
            attributes.write_text(
                ".github/workflows/*.yml text eol=lf\n"
                ".github/workflows/*.yaml text eol=lf\n",
                encoding="utf-8",
            )
            repository.git("add", ".gitattributes")
            repository.git("commit", "--quiet", "-m", "declare workflow LF checkout")

            status = repository.git("status", "--porcelain")
            workflow = repository.workflow("ci.yml")
            checkout_bytes = workflow.read_bytes()
            self.assertEqual(
                status.stdout,
                "",
                "the upgraded core.autocrlf=true checkout must be Git-clean before checking: "
                f"root={repository.root} status={status.stdout!r}",
            )
            self.assertIn(
                b"\r\n",
                checkout_bytes,
                "the reused-checkout regression must retain pre-attribute CRLF workflow bytes: "
                f"workflow={workflow} sample={checkout_bytes[:80]!r}",
            )
            self.assert_accepted(repository)

            workflow.write_bytes(checkout_bytes + b"# substantive mutation\r\n")
            self.assert_rejected(
                repository,
                "ci.yml",
                "reviewed sha256 changed",
                "review actual GitHub allocation",
            )
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

    @unittest.skipUnless(hasattr(os, "O_NOFOLLOW"), "O_NOFOLLOW is POSIX-only")
    def test_workflow_symlink_swap_between_lstat_and_open_is_rejected(self) -> None:
        repository = WorkflowRepository()
        try:
            self.assert_lstat_open_symlink_swap_rejected(
                repository,
                repository.workflow("ci.yml"),
                repository.root / "scripts/ci-race-target.yml",
                "../../scripts/ci-race-target.yml",
                "workflow",
            )
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

    def test_total_directory_entries_are_bounded_during_scandir(self) -> None:
        repository = WorkflowRepository()
        try:
            directory = repository.root / ".github/workflows"
            workflow_count = len(list(directory.glob("*.y*ml")))
            nonworkflow_count = MAX_WORKFLOW_DIRECTORY_ENTRIES - workflow_count + 1
            for index in range(nonworkflow_count):
                (directory / f"ignored-{index}.txt").touch()
            self.assert_rejected(
                repository,
                ".github/workflows",
                "directory entry count exceeds",
                str(MAX_WORKFLOW_DIRECTORY_ENTRIES),
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
                ContractError,
                "directory identity changed|entry set changed",
                msg=(
                    "a workflow inserted after initial enumeration must invalidate the "
                    f"snapshot: root={repository.root} added=surprise.yml"
                ),
            ):
                workflow_records(repository.root, phase_hook=add_workflow)
        finally:
            repository.close()

    def test_entry_added_after_final_scan_is_rejected(self) -> None:
        repository = WorkflowRepository()
        try:
            def add_workflow_during_final_scan() -> None:
                repository.workflow("late-final-scan.yml").write_text(
                    "name: late final scan\n", encoding="utf-8"
                )

            with self.assertRaisesRegex(
                ContractError,
                "directory identity changed during enumeration|"
                "directory identity changed while checking",
                msg=(
                    "an entry inserted after the final scandir exhausted must invalidate "
                    f"that enumeration: root={repository.root}"
                ),
            ):
                workflow_records(
                    repository.root, final_scan_hook=add_workflow_during_final_scan
                )
        finally:
            repository.close()

    def test_canonical_workflow_hash_never_requests_unbounded_reads(self) -> None:
        class BoundedReadStream(io.BytesIO):
            def __init__(self, contents: bytes) -> None:
                super().__init__(contents)
                self.requests: list[int] = []

            def read(self, size: int = -1) -> bytes:
                self.requests.append(size)
                if size < 0 or size > HASH_CHUNK_BYTES:
                    raise AssertionError(
                        "workflow hashing requested an unbounded or oversized read: "
                        f"size={size} requests={self.requests!r}"
                    )
                return super().read(size)

        contents = b"x" * (HASH_CHUNK_BYTES * 2 + 17)
        stream = BoundedReadStream(contents)
        digest, canonical_size = hash_canonical_workflow_stream(
            stream, len(contents), "bounded-read.yml"
        )
        self.assertEqual(
            len(digest),
            64,
            f"workflow hash must remain a SHA-256 digest: digest={digest!r}",
        )
        self.assertEqual(
            canonical_size,
            len(contents),
            "workflow canonical byte count must preserve bytes without CRLF pairs: "
            f"physical_size={len(contents)} canonical_size={canonical_size}",
        )
        self.assertGreater(
            len(stream.requests),
            3,
            "workflow hashing must use bounded chunks plus a one-byte growth probe: "
            f"requests={stream.requests!r}",
        )
        self.assertEqual(
            stream.requests[-1],
            1,
            "workflow hashing must probe for same-inode growth with one bounded byte: "
            f"requests={stream.requests!r}",
        )

    def test_canonical_workflow_hash_handles_crlf_split_across_chunks(self) -> None:
        lf_contents = b"x" * (HASH_CHUNK_BYTES - 1) + b"\nnext\n"
        crlf_contents = lf_contents.replace(b"\n", b"\r\n")
        digest, canonical_size = hash_canonical_workflow_stream(
            io.BytesIO(crlf_contents), len(crlf_contents), "split-crlf.yml"
        )
        self.assertEqual(
            digest,
            hashlib.sha256(lf_contents).hexdigest(),
            "CRLF split at the bounded-read edge must hash as the exact reviewed LF bytes: "
            f"physical_size={len(crlf_contents)} canonical_size={canonical_size}",
        )
        self.assertEqual(
            canonical_size,
            len(lf_contents),
            "canonical workflow byte count must remove exactly one byte per CRLF pair: "
            f"physical_size={len(crlf_contents)} canonical_size={canonical_size}",
        )

    def test_exact_read_rejects_early_eof(self) -> None:
        expected_size = 10
        contents = b"abc"
        with self.assertRaisesRegex(
            ContractError,
            "file size changed while reading; expected=10 actual=3",
            msg=(
                "an early EOF must not be accepted as the reviewed manifest bytes: "
                f"expected_size={expected_size} actual_size={len(contents)}"
            ),
        ):
            read_exact_bytes(io.BytesIO(contents), expected_size, "manifest.json")

    def test_manifest_truncated_after_open_is_rejected_at_checker_boundary(self) -> None:
        repository = WorkflowRepository()
        try:
            manifest = repository.manifest_path()
            initial_size = manifest.stat().st_size

            def truncate_open_manifest(opened_path: Path) -> None:
                self.assertEqual(
                    opened_path,
                    manifest,
                    "manifest mutation hook must target the safely opened manifest: "
                    f"opened={opened_path} expected={manifest}",
                )
                opened_path.write_bytes(b"{}\n")

            errors = compare_contract(
                repository.root, manifest_after_open_hook=truncate_open_manifest
            )
            self.assertTrue(
                any("file size changed while reading" in error for error in errors),
                "the checker boundary must reject an early EOF after manifest open: "
                f"manifest={manifest} initial_size={initial_size} errors={errors!r}",
            )
        finally:
            repository.close()

    def test_manifest_identity_overlaps_the_workflow_snapshot(self) -> None:
        repository = WorkflowRepository()
        try:
            workflow = repository.workflow("ci.yml")
            manifest_path = repository.manifest_path()
            initial_manifest_inode = manifest_path.stat().st_ino

            def replace_workflow_and_manifest_after_scan() -> None:
                changed_bytes = workflow.read_bytes() + b"\n# coherent late snapshot\n"
                workflow.write_bytes(changed_bytes)
                manifest = repository.manifest()
                manifest["workflows"]["ci.yml"]["bytes"] = len(changed_bytes)
                manifest["workflows"]["ci.yml"]["sha256"] = hashlib.sha256(
                    changed_bytes
                ).hexdigest()
                replacement = manifest_path.with_name("manifest-replacement.json")
                replacement.write_text(
                    json.dumps(manifest, indent=2, sort_keys=True) + "\n",
                    encoding="utf-8",
                )
                self.assertNotEqual(
                    replacement.stat().st_ino,
                    initial_manifest_inode,
                    "snapshot race fixture must allocate a distinct replacement while "
                    "the reviewed manifest inode still exists: "
                    f"path={manifest_path} initial_inode={initial_manifest_inode} "
                    f"replacement={replacement} "
                    f"replacement_inode={replacement.stat().st_ino}",
                )
                os.replace(replacement, manifest_path)

            errors = compare_contract(
                repository.root,
                after_workflow_hook=replace_workflow_and_manifest_after_scan,
            )
            self.assertTrue(
                any("manifest.json: file identity changed" in error for error in errors),
                "manifest identity must remain stable across the complete workflow scan: "
                f"manifest={manifest_path} workflow={workflow} errors={errors!r}",
            )
        finally:
            repository.close()

    def test_same_inode_growth_after_open_is_rejected(self) -> None:
        repository = WorkflowRepository()
        try:
            workflow = repository.workflow("ci.yml")
            initial_inode = workflow.stat().st_ino
            grew = False

            def grow_open_workflow(opened_path: Path) -> None:
                nonlocal grew
                if grew or opened_path != workflow:
                    return
                with opened_path.open("ab") as stream:
                    stream.write(b"# growth after open\n")
                grew = True
                self.assertEqual(
                    opened_path.stat().st_ino,
                    initial_inode,
                    "growth regression must mutate the already-open inode: "
                    f"path={opened_path} initial_inode={initial_inode} "
                    f"actual_inode={opened_path.stat().st_ino}",
                )

            with self.assertRaisesRegex(
                ContractError,
                "file grew while its reviewed bytes were hashed",
                msg=(
                    "same-inode growth after open must be rejected by the bounded hash "
                    f"stream: workflow={workflow} inode={initial_inode}"
                ),
            ):
                workflow_records(repository.root, after_open_hook=grow_open_workflow)
            self.assertTrue(
                grew,
                "same-inode growth hook must run after the workflow descriptor opens",
            )
        finally:
            repository.close()

    def test_leaf_replaced_after_enumeration_is_rejected(self) -> None:
        repository = WorkflowRepository()
        try:
            workflow = repository.workflow("ci.yml")
            reviewed_bytes = workflow.read_bytes()

            def replace_leaf() -> None:
                replacement = workflow.with_name("ci-enumeration-replacement.yml")
                replacement.write_bytes(reviewed_bytes)
                self.assertNotEqual(
                    replacement.stat().st_ino,
                    initial_inode,
                    "post-enumeration replacement regression must allocate a distinct leaf "
                    "while the enumerated inode still exists: "
                    f"workflow={workflow} initial_inode={initial_inode} "
                    f"replacement={replacement} "
                    f"replacement_inode={replacement.stat().st_ino}",
                )
                os.replace(replacement, workflow)

            initial_inode = workflow.stat().st_ino
            with self.assertRaisesRegex(
                ContractError,
                "identity changed after enumeration",
                msg=(
                    "atomic pathname replacement before workflow hashing must invalidate the "
                    f"enumerated leaf identity: workflow={workflow} "
                    f"initial_inode={initial_inode}"
                ),
            ):
                workflow_records(repository.root, phase_hook=replace_leaf)
        finally:
            repository.close()

    def test_leaf_replaced_after_hashing_is_rejected(self) -> None:
        repository = WorkflowRepository()
        try:
            workflow = repository.workflow("ci.yml")
            reviewed_bytes = workflow.read_bytes()
            initial_inode = workflow.stat().st_ino

            def replace_leaf() -> None:
                replacement = workflow.with_name("ci-replacement.yml")
                replacement.write_bytes(reviewed_bytes)
                self.assertNotEqual(
                    replacement.stat().st_ino,
                    initial_inode,
                    "post-hash replacement regression must allocate a distinct leaf while "
                    "the hashed inode still exists: "
                    f"workflow={workflow} initial_inode={initial_inode} "
                    f"replacement={replacement} "
                    f"replacement_inode={replacement.stat().st_ino}",
                )
                os.replace(replacement, workflow)

            with self.assertRaisesRegex(
                ContractError,
                "identity changed after hashing",
                msg=(
                    "atomic pathname replacement after hashing must invalidate the hashed "
                    "leaf identity: "
                    f"workflow={workflow} initial_inode={initial_inode}"
                ),
            ):
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

            with self.assertRaisesRegex(
                ContractError,
                "real directory",
                msg=(
                    "replacing the enumerated workflow directory with a symlink must fail: "
                    f"directory={directory} symlink_target={target}"
                ),
            ):
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

            with self.assertRaisesRegex(
                ContractError,
                "aggregate opened bytes exceeds",
                msg=(
                    "authoritative opened sizes must enforce the aggregate byte bound: "
                    f"paths={paths!r} initial_size={initial_size} "
                    f"limit={MAX_TOTAL_WORKFLOW_BYTES}"
                ),
            ):
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

    @unittest.skipUnless(hasattr(os, "O_NOFOLLOW"), "O_NOFOLLOW is POSIX-only")
    def test_manifest_symlink_swap_between_lstat_and_open_is_rejected(self) -> None:
        repository = WorkflowRepository()
        try:
            self.assert_lstat_open_symlink_swap_rejected(
                repository,
                repository.manifest_path(),
                repository.root / "manifest-race-target.json",
                "../manifest-race-target.json",
                "manifest",
            )
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
            ("workflow_canonical_eol", "crlf", "workflow canonical EOL must be"),
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
