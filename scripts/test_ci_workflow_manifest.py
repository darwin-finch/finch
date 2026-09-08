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
    repository_snapshot,
    workflow_snapshot,
    workflow_records,
)


ROOT = Path(__file__).resolve().parent.parent
CHECKER = ROOT / "scripts/check_ci_workflow_manifest.py"


class WorkflowRepository:
    def __init__(self, source_root: Path = ROOT) -> None:
        snapshot = repository_snapshot(source_root)
        self.temporary = tempfile.TemporaryDirectory()
        self.root = Path(self.temporary.name)
        try:
            workflows = self.root / ".github/workflows"
            workflows.mkdir(parents=True)
            for name, contents in snapshot.workflow_bytes.items():
                (workflows / name).write_bytes(contents)
            (self.root / "scripts").mkdir()
            self.manifest_path().write_bytes(snapshot.manifest_bytes)
        except BaseException:
            self.close()
            raise

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


class OpenAuditRecorder:
    """Record lexical open paths only while one source snapshot is active."""

    def __init__(self) -> None:
        self.active = False
        self.paths: list[str] = []
        probe_event = f"finch.workflow_manifest.audit_probe.{id(self)}"
        installed = False

        def record(event: str, arguments: tuple[object, ...]) -> None:
            nonlocal installed
            if event == probe_event:
                installed = True
                return
            if not self.active or event != "open" or not arguments:
                return
            try:
                path = os.fspath(arguments[0])
            except TypeError:
                return
            if isinstance(path, bytes):
                path = os.fsdecode(path)
            self.paths.append(path)

        sys.addaudithook(record)
        sys.audit(probe_event)
        if not installed:
            raise AssertionError(
                "workflow source access audit hook was not installed; an existing "
                "CPython audit hook may have vetoed sys.addaudithook"
            )

    def start(self) -> None:
        self.paths.clear()
        self.active = True

    def stop(self) -> None:
        self.active = False


class PullBoundIterator:
    """Fail if a bounded enumeration pulls beyond its first rejected element."""

    def __init__(self, names: list[str]) -> None:
        self.names = names
        self.pulls = 0

    def __iter__(self) -> PullBoundIterator:
        return self

    def __next__(self) -> object:
        if self.pulls == len(self.names):
            raise AssertionError(
                "bounded workflow enumeration pulled after its first excess element: "
                f"pulls={self.pulls} names={len(self.names)}"
            )
        name = self.names[self.pulls]
        self.pulls += 1
        return type("Entry", (), {"name": name})()


class MetadataRecordingEntry:
    """Proxy one real directory entry and record target-following metadata calls."""

    def __init__(self, entry, forbidden_name: str, attempts: list[str]) -> None:
        self.entry = entry
        self.forbidden_name = forbidden_name
        self.attempts = attempts

    @property
    def name(self) -> str:
        return self.entry.name

    def stat(self, *args, **kwargs):
        if self.name == self.forbidden_name and kwargs.get("follow_symlinks", True):
            self.attempts.append(f"DirEntry.stat({self.name!r})")
        return self.entry.stat(*args, **kwargs)

    def is_file(self, *args, **kwargs):
        if self.name == self.forbidden_name and kwargs.get("follow_symlinks", True):
            self.attempts.append(f"DirEntry.is_file({self.name!r})")
        return self.entry.is_file(*args, **kwargs)

    def __getattr__(self, name: str):
        return getattr(self.entry, name)


class MetadataRecordingScandir:
    """Preserve a real scandir context while proxying its yielded entries."""

    def __init__(self, entries, forbidden_name: str, attempts: list[str]) -> None:
        self.entries = entries
        self.forbidden_name = forbidden_name
        self.attempts = attempts

    def __enter__(self):
        self.entries.__enter__()
        return self

    def __exit__(self, *args):
        return self.entries.__exit__(*args)

    def __iter__(self):
        return (
            MetadataRecordingEntry(entry, self.forbidden_name, self.attempts)
            for entry in self.entries
        )


class WorkflowManifestTests(unittest.TestCase):
    def path_stat_follow_patch(
        self, forbidden_paths: tuple[Path, ...], attempts: list[str]
    ):
        original_stat = Path.stat
        forbidden = set(forbidden_paths)

        def record_stat(path: Path, *args, **kwargs):
            if path in forbidden and kwargs.get("follow_symlinks", True):
                attempts.append(f"Path.stat({path!s})")
            return original_stat(path, *args, **kwargs)

        return mock.patch.object(Path, "stat", autospec=True, side_effect=record_stat)

    def scandir_metadata_follow_patch(
        self, forbidden_name: str, attempts: list[str]
    ):
        original_scandir = os.scandir

        def record_scandir(path):
            return MetadataRecordingScandir(
                original_scandir(path), forbidden_name, attempts
            )

        return mock.patch(
            "check_ci_workflow_manifest.os.scandir", side_effect=record_scandir
        )

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

    def assert_snapshot_source_rejected(
        self,
        diagnostic: str,
        invariant: str,
        workflows: dict[str, bytes] | None = None,
        manifest_bytes: bytes = b"{}\n",
        ignored_entries: int = 0,
        unopened_names: tuple[str, ...] = (),
    ) -> None:
        with tempfile.TemporaryDirectory() as name:
            root = Path(name)
            self.write_snapshot_source(root, workflows, manifest_bytes)
            directory = root / ".github/workflows"
            for index in range(ignored_entries):
                (directory / f"ignored-{index}.txt").touch()
            recorder = OpenAuditRecorder() if unopened_names else None
            if recorder is not None:
                recorder.start()
            try:
                with self.assertRaisesRegex(
                    ContractError,
                    diagnostic,
                    msg=f"{invariant}: root={root}",
                ):
                    repository_snapshot(root)
            finally:
                if recorder is not None:
                    recorder.stop()
            if recorder is not None:
                self.assert_open_names_absent(recorder, root, unopened_names, invariant)

    def assert_open_names_absent(
        self,
        recorder: OpenAuditRecorder,
        root: Path,
        forbidden_names: tuple[str, ...],
        invariant: str,
    ) -> None:
        absolute = {str(root / name) for name in forbidden_names}
        basenames = {Path(name).name for name in forbidden_names}
        forbidden_opens = [
            opened
            for opened in recorder.paths
            if opened in absolute or Path(opened).name in basenames
        ]
        self.assertEqual(
            forbidden_opens,
            [],
            f"{invariant}; forbidden paths were opened by absolute or directory-relative "
            f"spelling: root={root} names={forbidden_names!r} "
            f"forbidden_opens={forbidden_opens!r} all_opens={recorder.paths!r}",
        )

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

    def test_source_snapshot_bounds_total_directory_entries(self) -> None:
        self.assert_snapshot_source_rejected(
            "directory entry count exceeds",
            "the source snapshot must reject the first entry beyond its reviewed "
            f"directory bound; entries={MAX_WORKFLOW_DIRECTORY_ENTRIES + 1}",
            ignored_entries=MAX_WORKFLOW_DIRECTORY_ENTRIES,
        )

    def test_source_snapshot_bounds_workflow_count_independently(self) -> None:
        workflows = {
            f"workflow-{index}.yml": b"name: fixture\n"
            for index in range(MAX_WORKFLOW_FILES + 1)
        }
        self.assert_snapshot_source_rejected(
            "workflow count exceeds",
            "the source snapshot must reject the first workflow beyond its reviewed "
            f"count independently of entry and byte bounds; workflows={len(workflows)}",
            workflows,
        )

    def test_source_snapshot_bounds_each_workflow_before_reading(self) -> None:
        size = MAX_WORKFLOW_BYTES + 1
        self.assert_snapshot_source_rejected(
            rf"oversized.yml: {size} bytes exceeds the reviewed {MAX_WORKFLOW_BYTES}-byte bound",
            "the source snapshot must enforce the per-workflow bound before reading; "
            f"size={size} limit={MAX_WORKFLOW_BYTES}",
            {"oversized.yml": b"x" * size},
            unopened_names=(".github/workflows/oversized.yml",),
        )

    def test_source_snapshot_bounds_aggregate_workflow_bytes_independently(self) -> None:
        workflows = {
            f"workflow-{index}.yml": b"x" * MAX_WORKFLOW_BYTES for index in range(9)
        }
        total = sum(len(contents) for contents in workflows.values())
        self.assert_snapshot_source_rejected(
            rf"{total} aggregate bytes exceeds the reviewed {MAX_TOTAL_WORKFLOW_BYTES}-byte bound",
            "the source snapshot must enforce aggregate bytes while every leaf and count "
            f"remains in bounds; total={total} limit={MAX_TOTAL_WORKFLOW_BYTES}",
            workflows,
        )

    def test_source_snapshot_bounds_valid_manifest_before_reading(self) -> None:
        manifest_bytes = b'{"padding":"' + b"x" * MAX_MANIFEST_BYTES + b'"}\n'
        self.assert_snapshot_source_rejected(
            rf"{len(manifest_bytes)} bytes exceeds the reviewed {MAX_MANIFEST_BYTES}-byte bound",
            "the source snapshot must reject an otherwise valid oversized manifest before "
            f"reading or parsing it; size={len(manifest_bytes)} limit={MAX_MANIFEST_BYTES}",
            manifest_bytes=manifest_bytes,
            unopened_names=("scripts/ci_workflow_manifest.json",),
        )

    def test_source_snapshot_never_opens_ignored_nonworkflow_link_or_target(self) -> None:
        temporary = tempfile.TemporaryDirectory()
        recorder = OpenAuditRecorder()
        try:
            root = Path(temporary.name)
            self.write_snapshot_source(root)
            link = root / ".github/workflows/ignored.txt"
            link.symlink_to(link.name)
            metadata_attempts: list[str] = []

            recorder.start()
            try:
                with self.scandir_metadata_follow_patch(
                    link.name, metadata_attempts
                ):
                    snapshot = repository_snapshot(root)
            finally:
                recorder.stop()

            self.assertEqual(
                sorted(snapshot.workflow_bytes),
                ["synthetic.yml"],
                "the source snapshot must contain only workflow YAML after ignoring a link: "
                f"root={root} captured={sorted(snapshot.workflow_bytes)!r}",
            )
            self.assert_open_names_absent(
                recorder,
                root,
                (".github/workflows/ignored.txt",),
                "the source snapshot must filter an ignored non-workflow link lexically "
                "before content or target-metadata access; the self-referential target "
                f"makes any metadata dereference fail actionably: link={link}",
            )
            self.assertEqual(
                metadata_attempts,
                [],
                "ignored non-workflow entries must be filtered by name before any "
                "target-following DirEntry metadata query, even if its failure is swallowed: "
                f"link={link} attempts={metadata_attempts!r}",
            )
        finally:
            recorder.stop()
            temporary.cleanup()

    def test_source_snapshot_rejects_manifest_link_without_opening_target(self) -> None:
        temporary = tempfile.TemporaryDirectory()
        recorder = OpenAuditRecorder()
        try:
            root = Path(temporary.name)
            self.write_snapshot_source(root)
            manifest = root / "scripts/ci_workflow_manifest.json"
            manifest.unlink()
            manifest.symlink_to(manifest.name)
            metadata_attempts: list[str] = []

            recorder.start()
            try:
                with self.path_stat_follow_patch((manifest,), metadata_attempts):
                    with self.assertRaisesRegex(
                        ContractError,
                        "must be a regular file, not a symbolic link",
                        msg=(
                            "the source snapshot must reject a manifest link before opening "
                            "its target or following target metadata: "
                            f"manifest={manifest} attempts={metadata_attempts!r}"
                        ),
                    ):
                        repository_snapshot(root)
            finally:
                recorder.stop()

            self.assert_open_names_absent(
                recorder,
                root,
                ("scripts/ci_workflow_manifest.json",),
                "manifest lstat rejection must occur before any absolute or "
                f"directory-relative open or target metadata access; manifest={manifest}",
            )
            self.assertEqual(
                metadata_attempts,
                [],
                "manifest validation must not issue target-following Path.stat calls, even "
                "if their errors would be swallowed before lstat rejection: "
                f"manifest={manifest} attempts={metadata_attempts!r}",
            )
        finally:
            recorder.stop()
            temporary.cleanup()

    def test_source_snapshot_rejects_workflow_link_without_opening_target(self) -> None:
        with tempfile.TemporaryDirectory() as name:
            root = Path(name)
            self.write_snapshot_source(root)
            workflow = root / ".github/workflows/synthetic.yml"
            workflow.unlink()
            workflow.symlink_to(workflow.name)
            recorder = OpenAuditRecorder()
            metadata_attempts: list[str] = []
            recorder.start()
            try:
                with self.path_stat_follow_patch((workflow,), metadata_attempts):
                    with self.assertRaisesRegex(
                        ContractError,
                        "must be a regular file, not a symbolic link",
                        msg=(
                            "the source snapshot must reject a workflow link before opening "
                            "its target or following target metadata: "
                            f"workflow={workflow} attempts={metadata_attempts!r}"
                        ),
                    ):
                        repository_snapshot(root)
            finally:
                recorder.stop()
            self.assert_open_names_absent(
                recorder,
                root,
                (".github/workflows/synthetic.yml",),
                "workflow metadata rejection must precede any link open or target "
                f"metadata access: workflow={workflow}",
            )
            self.assertEqual(
                metadata_attempts,
                [],
                "workflow validation must not issue target-following Path.stat calls, even "
                "if their errors would be swallowed before lstat rejection: "
                f"workflow={workflow} attempts={metadata_attempts!r}",
            )

    def test_source_snapshot_rejects_workflow_directory_link_without_target_open(self) -> None:
        with tempfile.TemporaryDirectory() as name:
            root = Path(name)
            self.write_snapshot_source(root)
            directory = root / ".github/workflows"
            shutil.rmtree(directory)
            directory.symlink_to(directory.name)
            recorder = OpenAuditRecorder()
            metadata_attempts: list[str] = []
            recorder.start()
            try:
                with self.path_stat_follow_patch((directory,), metadata_attempts):
                    with self.assertRaisesRegex(
                        ContractError,
                        "must be a real directory, not a link",
                        msg=(
                            "the source snapshot must reject a workflow-directory link "
                            "before opening it or following target metadata: "
                            f"directory={directory} attempts={metadata_attempts!r}"
                        ),
                    ):
                        repository_snapshot(root)
            finally:
                recorder.stop()
            self.assert_open_names_absent(
                recorder,
                root,
                (".github/workflows",),
                "workflow-directory lstat rejection must precede any target open or "
                f"metadata access: directory={directory}",
            )
            self.assertEqual(
                metadata_attempts,
                [],
                "workflow-directory validation must not issue target-following Path.stat "
                "calls, even if their errors would be swallowed before lstat rejection: "
                f"directory={directory} attempts={metadata_attempts!r}",
            )

    @unittest.skipUnless(
        WORKFLOW_DIRECTORY_FD_SUPPORTED,
        "directory-relative workflow pinning is unavailable on this platform",
    )
    def test_parent_replacement_never_captures_target_workflow_bytes(self) -> None:
        with tempfile.TemporaryDirectory() as name:
            root = Path(name)
            reviewed = b"name: reviewed source\n"
            forbidden = b"name: forbidden replacement\n"
            self.write_snapshot_source(root, {"synthetic.yml": reviewed})
            directory = root / ".github/workflows"
            target = root / "workflow-directory-target"
            target.mkdir()
            (target / "synthetic.yml").write_bytes(forbidden)
            captured: list[bytes] = []
            recorder = OpenAuditRecorder()

            def replace_parent() -> None:
                original = root / ".github/workflows-original"
                directory.rename(original)
                directory.symlink_to(target)

            def record_capture(stream, expected_size: int, display: str):
                contents, digest, canonical_size = capture_canonical_workflow_stream(
                    stream, expected_size, display
                )
                captured.append(contents)
                return contents, digest, canonical_size

            recorder.start()
            try:
                with mock.patch(
                    "check_ci_workflow_manifest.capture_canonical_workflow_stream",
                    side_effect=record_capture,
                ):
                    with self.assertRaisesRegex(
                        ContractError,
                        "must be a real directory, not a link|directory identity changed|"
                        "file identity changed after enumeration",
                        msg=(
                            "replacing the workflow parent after enumeration must reject "
                            "without capturing target bytes: "
                            f"directory={directory} target={target}"
                        ),
                    ):
                        repository_snapshot(root, workflow_phase_hook=replace_parent)
            finally:
                recorder.stop()
            self.assertIn(
                reviewed,
                captured,
                "the pinned directory descriptor must still capture the reviewed inode: "
                f"captured={captured!r} reviewed={reviewed!r}",
            )
            self.assertNotIn(
                forbidden,
                captured,
                "post-enumeration parent replacement must never expose target bytes: "
                f"captured={captured!r} forbidden={forbidden!r}",
            )
            self.assertNotIn(
                str(directory / "synthetic.yml"),
                recorder.paths,
                "POSIX child opens must use the pinned directory descriptor rather than the "
                "replaced absolute parent path: "
                f"opens={recorder.paths!r} directory={directory}",
            )

    @unittest.skipUnless(
        WORKFLOW_DIRECTORY_FD_SUPPORTED,
        "directory-relative workflow pinning is unavailable on this platform",
    )
    def test_successful_workflow_snapshot_closes_owned_directory_fd(self) -> None:
        with tempfile.TemporaryDirectory() as name:
            root = Path(name)
            self.write_snapshot_source(root)
            directory = root / ".github/workflows"
            real_open = os.open
            directory_fds: list[int] = []

            def record_directory_open(path, flags, *args, **kwargs):
                descriptor = real_open(path, flags, *args, **kwargs)
                if os.fspath(path) == os.fspath(directory):
                    directory_fds.append(descriptor)
                return descriptor

            with mock.patch(
                "check_ci_workflow_manifest.os.open", side_effect=record_directory_open
            ):
                workflow_snapshot(root)

            self.assertGreaterEqual(
                len(directory_fds),
                1,
                "a successful workflow snapshot must acquire an owned directory descriptor: "
                f"directory={directory} descriptors={directory_fds!r}",
            )
            descriptor = directory_fds[0]
            with self.assertRaises(OSError) as raised:
                os.fstat(descriptor)
            self.assertEqual(
                raised.exception.errno,
                errno.EBADF,
                "workflow_snapshot must close its owned directory descriptor on success: "
                f"directory={directory} descriptor={descriptor} error={raised.exception!r}",
            )

    def test_snapshot_without_directory_fd_fails_before_source_enumeration(self) -> None:
        with tempfile.TemporaryDirectory() as name:
            root = Path(name)
            contents = b"name: fallback workflow\n"
            self.write_snapshot_source(root, {"synthetic.yml": contents})
            recorder = OpenAuditRecorder()
            recorder.start()
            try:
                with mock.patch(
                    "check_ci_workflow_manifest.WORKFLOW_DIRECTORY_FD_SUPPORTED", False
                ), mock.patch(
                    "check_ci_workflow_manifest.os.scandir",
                    side_effect=AssertionError(
                        "unsupported snapshot enumerated workflow source children"
                    ),
                ) as scandir:
                    with self.assertRaisesRegex(
                        ContractError,
                        "cannot identity-pin the workflow directory.*refusing an unsafe "
                        "pathname-based snapshot",
                        msg=(
                            "platforms without directory-relative descriptor pinning must "
                            "fail closed before enumerating or opening workflow children: "
                            f"root={root}"
                        ),
                    ):
                        repository_snapshot(root)
                scandir.assert_not_called()
            finally:
                recorder.stop()
            self.assert_open_names_absent(
                recorder,
                root,
                (".github/workflows/synthetic.yml",),
                "unsupported platforms must not open or read a source workflow leaf before "
                f"the fail-closed diagnostic: root={root}",
            )

    def test_opened_aggregate_bound_precedes_over_budget_leaf_capture(self) -> None:
        with tempfile.TemporaryDirectory() as name:
            root = Path(name)
            workflows = {
                f"base-{index}.yml": b"x" * MAX_WORKFLOW_BYTES for index in range(8)
            }
            workflows["zz-grown.yml"] = b""
            self.write_snapshot_source(root, workflows)
            grown = root / ".github/workflows/zz-grown.yml"
            captured: list[str] = []

            def grow_last_workflow() -> None:
                grown.write_bytes(b"x" * MAX_WORKFLOW_BYTES)

            def record_capture(stream, expected_size: int, display: str):
                captured.append(display)
                return capture_canonical_workflow_stream(stream, expected_size, display)

            with mock.patch(
                "check_ci_workflow_manifest.capture_canonical_workflow_stream",
                side_effect=record_capture,
            ):
                with self.assertRaisesRegex(
                    ContractError,
                    "aggregate opened bytes exceeds.*before capturing.*zz-grown.yml",
                    msg=(
                        "authoritative opened-fd aggregate size must reject before reading "
                        f"the over-budget leaf: leaf={grown} captured={captured!r}"
                    ),
                ):
                    repository_snapshot(root, workflow_phase_hook=grow_last_workflow)
            self.assertNotIn(
                ".github/workflows/zz-grown.yml",
                captured,
                "the leaf that crosses the authoritative aggregate bound must not be "
                f"captured: leaf={grown} captured={captured!r}",
            )

    def test_source_snapshot_rejects_directory_replacement_before_materializing(self) -> None:
        temporary = tempfile.TemporaryDirectory()
        try:
            root = Path(temporary.name)
            workflow_bytes = b"name: fixture\n"
            self.write_snapshot_source(root, {"synthetic.yml": workflow_bytes})
            directory = root / ".github/workflows"
            replacement = root / ".github/workflows-replacement"
            replacement.mkdir()
            (replacement / "synthetic.yml").write_bytes(workflow_bytes)
            original_inode = directory.stat().st_ino
            replacement_inode = replacement.stat().st_ino
            self.assertNotEqual(
                replacement_inode,
                original_inode,
                "directory replacement fixture must allocate both same-byte directories "
                "while the original remains live: "
                f"directory={directory} inode={original_inode} replacement={replacement} "
                f"replacement_inode={replacement_inode}",
            )

            def replace_directory() -> None:
                original = root / ".github/workflows-original"
                directory.rename(original)
                os.replace(replacement, directory)

            with self.assertRaisesRegex(
                ContractError,
                "directory identity changed during enumeration",
                msg=(
                    "the source snapshot must discard captured names when the workflow "
                    "directory pathname changes before leaf opens: "
                    f"directory={directory} original_inode={original_inode} "
                    f"replacement_inode={replacement_inode}"
                ),
            ):
                repository_snapshot(root, workflow_post_scan_hook=replace_directory)
        finally:
            temporary.cleanup()

    def test_rejected_source_snapshot_allocates_no_fixture_tree(self) -> None:
        temporary = tempfile.TemporaryDirectory()
        try:
            root = Path(temporary.name)
            self.write_snapshot_source(
                root, {"oversized.yml": b"x" * (MAX_WORKFLOW_BYTES + 1)}
            )
            with mock.patch.object(
                tempfile,
                "TemporaryDirectory",
                side_effect=AssertionError("fixture tree allocated before source validation"),
            ) as temporary_factory, mock.patch.object(
                tempfile,
                "mkdtemp",
                side_effect=AssertionError("raw fixture directory allocated before validation"),
            ) as mkdtemp_factory:
                with self.assertRaisesRegex(
                    ContractError,
                    "exceeds the reviewed",
                    msg=(
                        "WorkflowRepository must reject an invalid source snapshot before "
                        "TemporaryDirectory or mkdtemp can allocate a destination: "
                        f"source={root}"
                    ),
                ):
                    WorkflowRepository(root)
            temporary_factory.assert_not_called()
            mkdtemp_factory.assert_not_called()
        finally:
            temporary.cleanup()

    def test_directory_entry_bound_stops_at_first_excess_pull(self) -> None:
        entries = PullBoundIterator(
            [f"ignored-{index}.txt" for index in range(MAX_WORKFLOW_DIRECTORY_ENTRIES + 1)]
        )
        with self.assertRaisesRegex(
            ContractError,
            "directory entry count exceeds",
            msg=(
                "directory enumeration must stop on the first entry beyond its bound: "
                f"limit={MAX_WORKFLOW_DIRECTORY_ENTRIES}"
            ),
        ):
            bounded_workflow_names(entries)
        self.assertEqual(
            entries.pulls,
            MAX_WORKFLOW_DIRECTORY_ENTRIES + 1,
            "directory enumeration must not pull after its first rejected entry: "
            f"pulls={entries.pulls} limit={MAX_WORKFLOW_DIRECTORY_ENTRIES}",
        )

    def test_workflow_count_bound_stops_at_first_excess_pull(self) -> None:
        entries = PullBoundIterator(
            [f"workflow-{index}.yml" for index in range(MAX_WORKFLOW_FILES + 1)]
        )
        with self.assertRaisesRegex(
            ContractError,
            "workflow count exceeds",
            msg=(
                "workflow enumeration must stop on the first YAML entry beyond its bound: "
                f"limit={MAX_WORKFLOW_FILES}"
            ),
        ):
            bounded_workflow_names(entries)
        self.assertEqual(
            entries.pulls,
            MAX_WORKFLOW_FILES + 1,
            "workflow enumeration must not pull after its first rejected YAML entry: "
            f"pulls={entries.pulls} limit={MAX_WORKFLOW_FILES}",
        )

    def test_workflow_source_replacement_after_snapshot_uses_captured_bytes(self) -> None:
        with tempfile.TemporaryDirectory() as name:
            root = Path(name)
            captured = b"name: captured workflow\n"
            replacement = b"name: replacement workflow\n"
            self.write_snapshot_source(root, {"synthetic.yml": captured})
            original_snapshot = repository_snapshot

            def replace_after_snapshot(source_root: Path):
                snapshot = original_snapshot(source_root)
                (source_root / ".github/workflows/synthetic.yml").write_bytes(replacement)
                return snapshot

            with mock.patch.object(
                sys.modules[__name__], "repository_snapshot", replace_after_snapshot
            ):
                repository = WorkflowRepository(root)
            try:
                self.assertEqual(
                    repository.workflow("synthetic.yml").read_bytes(),
                    captured,
                    "fixture materialization must use captured workflow bytes after the "
                    f"source pathname changes: source={root} captured={captured!r} "
                    f"replacement={replacement!r}",
                )
            finally:
                repository.close()

    def test_manifest_source_replacement_after_snapshot_uses_captured_bytes(self) -> None:
        with tempfile.TemporaryDirectory() as name:
            root = Path(name)
            captured = b"{}\n"
            replacement = b'{"replacement":true}\n'
            self.write_snapshot_source(root, manifest_bytes=captured)
            original_snapshot = repository_snapshot

            def replace_after_snapshot(source_root: Path):
                snapshot = original_snapshot(source_root)
                (source_root / "scripts/ci_workflow_manifest.json").write_bytes(replacement)
                return snapshot

            with mock.patch.object(
                sys.modules[__name__], "repository_snapshot", replace_after_snapshot
            ):
                repository = WorkflowRepository(root)
            try:
                self.assertEqual(
                    repository.manifest_path().read_bytes(),
                    captured,
                    "fixture materialization must use captured manifest bytes after the "
                    f"source pathname changes: source={root} captured={captured!r} "
                    f"replacement={replacement!r}",
                )
            finally:
                repository.close()

    def test_materialization_failure_removes_allocated_tree_with_traceback_retained(self) -> None:
        with tempfile.TemporaryDirectory() as name:
            source_root = Path(name)
            self.write_snapshot_source(source_root)
            attempted_paths: list[Path] = []

            def fail_write(path: Path, contents: bytes) -> int:
                attempted_paths.append(path)
                raise OSError("injected fixture materialization write failure")

            retained_error: OSError | None = None
            with mock.patch.object(Path, "write_bytes", autospec=True, side_effect=fail_write):
                try:
                    WorkflowRepository(source_root)
                except OSError as error:
                    retained_error = error
            self.assertIsNotNone(
                retained_error,
                "materialization failure injection must retain the raised exception",
            )
            self.assertIsNotNone(
                retained_error.__traceback__ if retained_error is not None else None,
                "materialization failure must retain its traceback while cleanup is checked: "
                f"error={retained_error!r}",
            )
            self.assertEqual(
                len(attempted_paths),
                1,
                "failure injection must stop at the first fixture write: "
                f"attempted_paths={attempted_paths!r}",
            )
            allocated_root = attempted_paths[0].parents[2]
            self.assertFalse(
                allocated_root.exists(),
                "WorkflowRepository must remove its partially materialized tree before "
                "propagating the write failure, even while traceback retains constructor "
                f"locals: allocated_root={allocated_root} error={retained_error!r}",
            )

    def test_materialization_interrupt_removes_allocated_tree_with_traceback_retained(self) -> None:
        with tempfile.TemporaryDirectory() as name:
            source_root = Path(name)
            self.write_snapshot_source(source_root)
            attempted_paths: list[Path] = []

            def interrupt_write(path: Path, contents: bytes) -> int:
                attempted_paths.append(path)
                raise KeyboardInterrupt("injected fixture materialization interruption")

            retained_interrupt: KeyboardInterrupt | None = None
            with mock.patch.object(
                Path, "write_bytes", autospec=True, side_effect=interrupt_write
            ):
                try:
                    WorkflowRepository(source_root)
                except KeyboardInterrupt as error:
                    retained_interrupt = error
            self.assertIsNotNone(
                retained_interrupt,
                "materialization interruption injection must retain KeyboardInterrupt",
            )
            self.assertIsNotNone(
                retained_interrupt.__traceback__
                if retained_interrupt is not None
                else None,
                "materialization interruption must retain its traceback while cleanup is "
                f"checked: error={retained_interrupt!r}",
            )
            self.assertEqual(
                len(attempted_paths),
                1,
                "KeyboardInterrupt injection must stop at the first fixture write: "
                f"attempted_paths={attempted_paths!r}",
            )
            allocated_root = attempted_paths[0].parents[2]
            self.assertFalse(
                allocated_root.exists(),
                "WorkflowRepository must remove its partially materialized tree before "
                "re-raising KeyboardInterrupt, even while its traceback retains constructor "
                f"locals: allocated_root={allocated_root} error={retained_interrupt!r}",
            )

    def test_audit_recorder_fails_when_existing_hook_vetoes_installation(self) -> None:
        program = (
            "import sys\n"
            "from test_ci_workflow_manifest import OpenAuditRecorder\n"
            "def veto(event, arguments):\n"
            "    if event == 'sys.addaudithook':\n"
            "        raise RuntimeError('injected audit-hook veto')\n"
            "sys.addaudithook(veto)\n"
            "OpenAuditRecorder()\n"
        )
        result = subprocess.run(
            [sys.executable, "-c", program],
            cwd=ROOT / "scripts",
            check=False,
            capture_output=True,
            text=True,
            timeout=10,
        )
        self.assertNotEqual(
            result.returncode,
            0,
            "audit recorder construction must fail when CPython silently vetoes its hook: "
            f"stdout={result.stdout!r} stderr={result.stderr!r}",
        )
        self.assertIn(
            "workflow source access audit hook was not installed",
            result.stderr,
            "audit-hook veto failure must explain why negative-open assertions are unsafe: "
            f"stdout={result.stdout!r} stderr={result.stderr!r}",
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
                "directory identity changed during enumeration",
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
