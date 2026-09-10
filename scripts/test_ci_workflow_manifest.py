#!/usr/bin/env python3
"""Production-boundary mutations for the exact workflow inventory contract."""

from __future__ import annotations

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
    compare_contract,
    hash_canonical_workflow_stream,
    open_path_descriptor,
    read_exact_bytes,
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
    def test_path_descriptor_open_uses_exact_read_only_safety_mask(self) -> None:
        with tempfile.TemporaryDirectory() as name:
            path = Path(name) / "reviewed.bin"
            path.write_bytes(b"reviewed")
            observed: list[tuple[Path, int]] = []
            real_open = os.open

            def record_open(opened_path, flags, *args, **kwargs):
                observed.append((Path(opened_path), flags))
                return real_open(opened_path, flags, *args, **kwargs)

            with mock.patch(
                "check_ci_workflow_manifest.os.open", side_effect=record_open
            ):
                descriptor = open_path_descriptor(path, "reviewed.bin")
            os.close(descriptor)
            expected = (
                os.O_RDONLY
                | getattr(os, "O_CLOEXEC", 0)
                | os.O_NOFOLLOW
                | os.O_NONBLOCK
            )
            self.assertEqual(
                observed,
                [(path, expected)],
                "pathname acquisition must make one os.open call with the exact "
                "read-only, no-follow, nonblocking mask: "
                f"path={path} observed={observed!r} expected_flags={expected:#x}",
            )
            self.assertEqual(
                expected & os.O_ACCMODE,
                os.O_RDONLY,
                "reviewed pathname acquisition must not expand access to writing: "
                f"path={path} flags={expected:#x} access={expected & os.O_ACCMODE:#x}",
            )

    def test_path_descriptor_open_rejects_same_inode_symlink_with_os_detail(self) -> None:
        with tempfile.TemporaryDirectory() as name:
            root = Path(name)
            path = root / "reviewed.bin"
            path.write_bytes(b"reviewed")
            target = root / "reviewed-target.bin"
            path.rename(target)
            path.symlink_to(target.name)
            display = "manifest-contract"
            with self.assertRaises(
                ContractError,
                msg=(
                    "O_NOFOLLOW must reject a pathname changed to a same-inode symlink: "
                    f"display={display} path={path} target={target} "
                    f"target_inode={target.stat().st_ino}"
                ),
            ) as raised:
                open_path_descriptor(path, display)
            diagnostic = str(raised.exception)
            self.assertIn(
                display,
                diagnostic,
                "symlink rejection must identify the reviewed display name: "
                f"display={display} path={path} diagnostic={diagnostic!r}",
            )
            self.assertIn(
                str(path),
                diagnostic,
                "symlink rejection must include the exact source path: "
                f"display={display} path={path} diagnostic={diagnostic!r}",
            )
            for invariant in ("read-only", "no-follow", "nonblocking"):
                with self.subTest(invariant=invariant):
                    self.assertIn(
                        invariant,
                        diagnostic,
                        "symlink rejection must name every enforced open invariant: "
                        f"display={display} path={path} invariant={invariant!r} "
                        f"diagnostic={diagnostic!r}",
                    )
            self.assertIsInstance(
                raised.exception.__cause__,
                OSError,
                "symlink rejection must retain the underlying OSError as its cause: "
                f"display={display} path={path} cause={raised.exception.__cause__!r}",
            )
            self.assertIn(
                os.strerror(raised.exception.__cause__.errno),
                diagnostic,
                "symlink rejection must preserve the underlying OS error detail: "
                f"display={display} path={path} cause={raised.exception.__cause__!r} "
                f"diagnostic={diagnostic!r}",
            )

    @unittest.skipUnless(hasattr(os, "mkfifo"), "FIFO open requires os.mkfifo")
    def test_path_descriptor_open_fifo_returns_promptly_and_is_caller_owned(self) -> None:
        with tempfile.TemporaryDirectory() as name:
            path = Path(name) / "reviewed.fifo"
            os.mkfifo(path)
            program = (
                "import os, sys\n"
                "from pathlib import Path\n"
                "from check_ci_workflow_manifest import open_path_descriptor\n"
                "path = Path(sys.argv[1])\n"
                "fd = open_path_descriptor(path, 'reviewed.fifo')\n"
                "os.fstat(fd)\n"
                "os.close(fd)\n"
                "raise SystemExit(0)\n"
            )
            try:
                result = subprocess.run(
                    [sys.executable, "-c", program, str(path)],
                    cwd=ROOT / "scripts",
                    check=False,
                    capture_output=True,
                    text=True,
                    timeout=2,
                )
            except subprocess.TimeoutExpired as error:
                self.fail(
                    "O_NONBLOCK must make FIFO pathname acquisition return promptly: "
                    f"path={path} timeout=2 error={error!r}"
                )
            self.assertEqual(
                result.returncode,
                0,
                "FIFO descriptor must return promptly and remain caller-closeable: "
                f"path={path} stdout={result.stdout!r} stderr={result.stderr!r}",
            )

    def test_path_descriptor_open_fails_before_open_without_required_flags(self) -> None:
        path = Path("/bounded/fixture/reviewed.bin")
        for flag_name in ("O_NOFOLLOW", "O_NONBLOCK"):
            with self.subTest(flag=flag_name):
                original = getattr(os, flag_name)
                delattr(os, flag_name)
                try:
                    with mock.patch(
                        "check_ci_workflow_manifest.os.open",
                        side_effect=AssertionError(f"os.open called without {flag_name}"),
                    ) as opened:
                        with self.assertRaisesRegex(
                            ContractError,
                            rf"reviewed\.bin: {flag_name} is unavailable; refusing to open "
                            rf"path={path}",
                            msg=(
                                "missing safety capability must fail closed before os.open: "
                                f"display=reviewed.bin path={path} flag={flag_name}"
                            ),
                        ):
                            open_path_descriptor(path, "reviewed.bin")
                        opened.assert_not_called()
                finally:
                    setattr(os, flag_name, original)

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
            repository.workflow("ci.yml").unlink()
            self.assert_rejected(repository, "ci.yml", "is missing")
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
            self.assertEqual(
                len(paths),
                14,
                f"aggregate-race fixture assumes 14 reviewed workflows: paths={paths!r}",
            )
            initial_size = MAX_TOTAL_WORKFLOW_BYTES // len(paths) - 25
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
