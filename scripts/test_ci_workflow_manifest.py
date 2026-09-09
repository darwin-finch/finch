#!/usr/bin/env python3
"""Production-boundary mutations for the exact workflow inventory contract."""

from __future__ import annotations

import errno
import hashlib
import io
import json
import os
import shutil
import stat
import subprocess
import sys
import tempfile
import tracemalloc
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
    file_identity,
    hash_canonical_workflow_stream,
    load_manifest,
    manifest_bytes_snapshot,
    manifest_file_metadata,
    open_path_descriptor,
    read_bounded_descriptor,
    read_exact_bytes,
    regular_file_metadata,
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
    def write_manifest_source(self, root: Path, contents: bytes) -> Path:
        manifest = root / "scripts/ci_workflow_manifest.json"
        manifest.parent.mkdir(parents=True, exist_ok=True)
        manifest.write_bytes(contents)
        return manifest

    def assert_fd_closed(self, descriptor: int, invariant: str) -> None:
        with self.assertRaises(
            OSError,
            msg=f"{invariant}: expected fd={descriptor} to be closed, but fstat succeeded",
        ) as raised:
            os.fstat(descriptor)
        self.assertEqual(
            raised.exception.errno,
            errno.EBADF,
            f"{invariant}: closed fd={descriptor} must report EBADF; "
            f"error={raised.exception!r}",
        )

    def test_manifest_snapshot_exact_limit_raw_fidelity_flags_and_close(self) -> None:
        contents = b"\x00\xff" + b"x" * (MAX_MANIFEST_BYTES - 2)
        with tempfile.TemporaryDirectory() as name:
            root = Path(name)
            manifest = self.write_manifest_source(root, contents)
            real_open = os.open
            opens: list[tuple[Path, int, int]] = []

            def record_open(path, flags, *args, **kwargs):
                descriptor = real_open(path, flags, *args, **kwargs)
                opens.append((Path(path), flags, descriptor))
                return descriptor

            with mock.patch(
                "check_ci_workflow_manifest.os.open", side_effect=record_open
            ):
                captured, identity = manifest_bytes_snapshot(root)
            expected_flags = (
                os.O_RDONLY
                | getattr(os, "O_CLOEXEC", 0)
                | os.O_NOFOLLOW
                | os.O_NONBLOCK
            )
            expected_parent_flags = expected_flags | os.O_DIRECTORY
            self.assertEqual(
                captured,
                contents,
                "manifest capture must preserve arbitrary bytes at the exact limit: "
                f"path={manifest} expected_size={len(contents)} "
                f"actual_size={len(captured)} identity={identity!r}",
            )
            self.assertEqual(
                opens,
                [
                    (manifest.parent, expected_parent_flags, opens[0][2]),
                    (Path(manifest.name), expected_flags, opens[1][2]),
                ],
                "manifest capture must pin its parent then open the leaf with exact safe "
                f"masks: path={manifest} opens={opens!r} "
                f"expected_parent_flags={expected_parent_flags:#x} "
                f"expected_leaf_flags={expected_flags:#x}",
            )
            for opened_path, _, descriptor in opens:
                self.assert_fd_closed(
                    descriptor,
                    f"successful manifest capture must close fd path={manifest} "
                    f"opened_path={opened_path}",
                )

    def test_manifest_snapshot_static_first_excess_rejects_before_open(self) -> None:
        contents = b"x" * (MAX_MANIFEST_BYTES + 1)
        with tempfile.TemporaryDirectory() as name:
            root = Path(name)
            manifest = self.write_manifest_source(root, contents)
            with mock.patch(
                "check_ci_workflow_manifest.open_path_descriptor",
                side_effect=AssertionError("oversized manifest reached descriptor open"),
            ) as opened:
                with self.assertRaisesRegex(
                    ContractError,
                    rf"{MAX_MANIFEST_BYTES + 1} bytes exceeds the reviewed "
                    rf"{MAX_MANIFEST_BYTES}-byte bound",
                    msg=(
                        "a statically oversized manifest must fail its reviewed bound "
                        f"before descriptor acquisition: path={manifest} size={len(contents)}"
                    ),
                ):
                    manifest_bytes_snapshot(root)
            opened.assert_not_called()

    def test_manifest_snapshot_composes_the_safe_open_primitive_once(self) -> None:
        with tempfile.TemporaryDirectory() as name:
            root = Path(name)
            manifest = self.write_manifest_source(root, b"raw")
            with mock.patch(
                "check_ci_workflow_manifest.open_path_descriptor",
                wraps=open_path_descriptor,
            ) as safe_open:
                manifest_bytes_snapshot(root)
            safe_open.assert_called_once_with(
                manifest.name,
                "scripts/ci_workflow_manifest.json",
                dir_fd=mock.ANY,
                diagnostic_path=manifest,
            )

    def test_manifest_snapshot_first_excess_is_only_excess_byte_pulled(self) -> None:
        contents = b"x" * MAX_MANIFEST_BYTES
        with tempfile.TemporaryDirectory() as name:
            root = Path(name)
            manifest = self.write_manifest_source(root, contents)
            real_open = os.open
            duplicate_fds: list[int] = []

            def retain_offset(path, flags, *args, **kwargs):
                descriptor = real_open(path, flags, *args, **kwargs)
                if Path(path).name == manifest.name:
                    duplicate_fds.append(os.dup(descriptor))
                return descriptor

            def grow_after_open(path: Path) -> None:
                with path.open("ab") as stream:
                    stream.write(b"y" * 8192)

            try:
                with mock.patch(
                    "check_ci_workflow_manifest.os.open", side_effect=retain_offset
                ):
                    with self.assertRaisesRegex(
                        ContractError,
                        rf"captured {MAX_MANIFEST_BYTES + 1} bytes exceeds the reviewed "
                        rf"{MAX_MANIFEST_BYTES}-byte bound",
                        msg=(
                            "manifest capture must reject exactly +1 after physically "
                            f"pulling only the first excess byte: path={manifest}"
                        ),
                    ):
                        manifest_bytes_snapshot(root, after_open_hook=grow_after_open)
                offset = os.lseek(duplicate_fds[0], 0, os.SEEK_CUR)
                self.assertEqual(
                    offset,
                    MAX_MANIFEST_BYTES + 1,
                    "bounded raw capture must stop after the first excess byte: "
                    f"path={manifest} offset={offset} limit={MAX_MANIFEST_BYTES + 1}",
                )
            finally:
                for descriptor in duplicate_fds:
                    os.close(descriptor)

    def test_manifest_snapshot_accumulates_legal_short_reads(self) -> None:
        contents = b"short reads must still preserve every raw byte"
        with tempfile.TemporaryDirectory() as name:
            root = Path(name)
            manifest = self.write_manifest_source(root, contents)
            real_read = os.read
            requests: list[int] = []

            def short_read(descriptor: int, requested: int) -> bytes:
                requests.append(requested)
                return real_read(descriptor, min(requested, 3))

            with mock.patch(
                "check_ci_workflow_manifest.os.read", side_effect=short_read
            ):
                captured, _ = manifest_bytes_snapshot(root)
            self.assertEqual(
                captured,
                contents,
                "legal short os.read results must be accumulated through EOF: "
                f"path={manifest} expected={contents!r} actual={captured!r} "
                f"requests={requests!r}",
            )
            self.assertGreater(
                len(requests),
                2,
                "short-read regression must exercise multiple bounded pulls: "
                f"path={manifest} requests={requests!r}",
            )
            self.assertEqual(
                requests,
                list(range(len(contents) + 1, 0, -3)) + [1],
                "short-read capture must reduce each request by bytes actually consumed and "
                "finish with one bounded EOF probe: "
                f"path={manifest} requests={requests!r} expected_size={len(contents)}",
            )

    def test_manifest_short_reads_retain_bounded_accumulator_overhead(self) -> None:
        remaining = MAX_MANIFEST_BYTES
        reads = 0

        def one_fresh_byte(_descriptor: int, _requested: int) -> bytes:
            nonlocal remaining, reads
            reads += 1
            if remaining == 0:
                return b""
            remaining -= 1
            return bytes(bytearray((remaining & 0xFF,)))

        tracemalloc.start()
        try:
            with mock.patch(
                "check_ci_workflow_manifest.os.read", new=one_fresh_byte
            ):
                captured = read_bounded_descriptor(
                    -1,
                    MAX_MANIFEST_BYTES,
                    MAX_MANIFEST_BYTES,
                    "scripts/ci_workflow_manifest.json",
                )
            _, peak = tracemalloc.get_traced_memory()
        finally:
            tracemalloc.stop()
        self.assertEqual(
            len(captured),
            MAX_MANIFEST_BYTES,
            "one-byte legal reads must preserve the exact reviewed payload: "
            f"captured={len(captured)} expected={MAX_MANIFEST_BYTES} reads={reads} "
            f"peak={peak}",
        )
        self.assertEqual(
            reads,
            MAX_MANIFEST_BYTES + 1,
            "exact-limit one-byte reads must finish with one EOF probe: "
            f"reads={reads} expected={MAX_MANIFEST_BYTES + 1} peak={peak}",
        )
        self.assertLess(
            peak,
            MAX_MANIFEST_BYTES * 8,
            "legal short reads must not retain one allocation per byte: "
            f"payload={MAX_MANIFEST_BYTES} reads={reads} traced_peak={peak} "
            f"bound={MAX_MANIFEST_BYTES * 8}",
        )

    def test_file_identity_pins_all_five_fields_and_rejects_ctime_only_change(self) -> None:
        with tempfile.TemporaryDirectory() as name:
            root = Path(name)
            manifest = self.write_manifest_source(root, b"raw")
            initial = manifest.stat()
            expected = (
                initial.st_dev,
                initial.st_ino,
                initial.st_size,
                initial.st_mtime_ns,
                initial.st_ctime_ns,
            )
            self.assertEqual(
                file_identity(initial),
                expected,
                "file identity must independently pin dev, inode, size, mtime_ns, and "
                f"ctime_ns in that order: path={manifest} expected={expected!r} "
                f"actual={file_identity(initial)!r}",
            )

            changed: os.stat_result | None = None

            def change_ctime_only(path: Path) -> None:
                nonlocal changed
                os.chmod(path, initial.st_mode ^ stat.S_IXUSR)
                changed = path.stat()
                stable_four = (
                    changed.st_dev,
                    changed.st_ino,
                    changed.st_size,
                    changed.st_mtime_ns,
                )
                initial_four = expected[:4]
                self.assertEqual(
                    stable_four,
                    initial_four,
                    "ctime-only fixture must preserve the other identity fields: "
                    f"path={path} initial={expected!r} changed="
                    f"{(stable_four + (changed.st_ctime_ns,))!r}",
                )
                self.assertNotEqual(
                    changed.st_ctime_ns,
                    initial.st_ctime_ns,
                    "ctime-only fixture must actually advance ctime_ns: "
                    f"path={path} initial_ctime={initial.st_ctime_ns} "
                    f"changed_ctime={changed.st_ctime_ns}",
                )

            with self.assertRaises(
                ContractError,
                msg=(
                    "manifest capture must reject a ctime-only pre-open change: "
                    f"path={manifest} initial_identity={expected!r}"
                ),
            ) as raised:
                manifest_bytes_snapshot(root, before_open_hook=change_ctime_only)
            self.assertIsNotNone(
                changed,
                f"ctime-only mutation hook must run before manifest open: path={manifest}",
            )
            self.assertIn(
                f"initial_identity={expected!r}",
                str(raised.exception),
                "ctime-only rejection must report the independently pinned initial "
                f"identity: path={manifest} changed={changed!r} "
                f"diagnostic={str(raised.exception)!r}",
            )

    def test_manifest_snapshot_rejects_preopen_identity_mutations_and_closes(self) -> None:
        for mutation in ("distinct", "same-inode-size", "same-inode-metadata"):
            with self.subTest(mutation=mutation), tempfile.TemporaryDirectory() as name:
                root = Path(name)
                manifest = self.write_manifest_source(root, b"original")
                initial = manifest.stat()
                initial_identity = file_identity(initial)
                replacement = root / "replacement.json"
                replacement.write_bytes(b"replacement")
                real_open = os.open
                descriptors: list[int] = []

                def record_open(path, flags, *args, **kwargs):
                    descriptor = real_open(path, flags, *args, **kwargs)
                    descriptors.append(descriptor)
                    return descriptor

                def mutate(_path: Path) -> None:
                    if mutation == "distinct":
                        os.replace(replacement, manifest)
                    elif mutation == "same-inode-size":
                        with manifest.open("ab") as stream:
                            stream.write(b"!")
                    else:
                        os.utime(
                            manifest,
                            ns=(initial.st_atime_ns, initial.st_mtime_ns + 1_000_000_000),
                        )

                with mock.patch(
                    "check_ci_workflow_manifest.os.open", side_effect=record_open
                ):
                    with self.assertRaises(
                        ContractError,
                        msg=(
                            "pre-open mutation must reject with full identities: "
                            f"path={manifest} mutation={mutation} "
                            f"initial_identity={initial_identity!r}"
                        ),
                    ) as raised:
                        manifest_bytes_snapshot(root, before_open_hook=mutate)
                opened_identity = file_identity(os.stat(manifest))
                diagnostic = str(raised.exception)
                for detail in (
                    str(manifest),
                    f"initial_identity={initial_identity!r}",
                    f"opened_identity={opened_identity!r}",
                ):
                    self.assertIn(
                        detail,
                        diagnostic,
                        "pre-open mutation diagnostic must name path and both identities: "
                        f"path={manifest} mutation={mutation} detail={detail!r} "
                        f"diagnostic={diagnostic!r}",
                    )
                self.assertEqual(
                    len(descriptors),
                    2,
                    "pre-open mutation must use one pinned parent and one manifest "
                    "descriptor: "
                    f"path={manifest} mutation={mutation} descriptors={descriptors!r}",
                )
                for descriptor in descriptors:
                    self.assert_fd_closed(
                        descriptor,
                        f"pre-open {mutation} rejection must close every fd "
                        f"path={manifest}",
                    )

    def test_manifest_snapshot_rejects_growth_and_truncation_and_closes(self) -> None:
        for mutation in ("growth", "truncation"):
            with self.subTest(mutation=mutation), tempfile.TemporaryDirectory() as name:
                root = Path(name)
                manifest = self.write_manifest_source(root, b"original")
                real_open = os.open
                descriptors: list[int] = []

                def record_open(path, flags, *args, **kwargs):
                    descriptor = real_open(path, flags, *args, **kwargs)
                    descriptors.append(descriptor)
                    return descriptor

                def mutate(_path: Path) -> None:
                    if mutation == "growth":
                        with manifest.open("ab") as stream:
                            stream.write(b"!")
                    else:
                        manifest.write_bytes(b"x")

                with mock.patch(
                    "check_ci_workflow_manifest.os.open", side_effect=record_open
                ):
                    with self.assertRaisesRegex(
                        ContractError,
                        r"file size changed while reading; expected=8 actual=(9|1)",
                        msg=(
                            "same-inode growth/truncation must reject at bounded read: "
                            f"path={manifest} mutation={mutation}"
                        ),
                    ):
                        manifest_bytes_snapshot(root, after_open_hook=mutate)
                self.assert_fd_closed(
                    descriptors[0],
                    f"{mutation} rejection must close fd path={manifest}",
                )

    def test_manifest_snapshot_rejects_nonregular_directory_and_closes(self) -> None:
        with tempfile.TemporaryDirectory() as name:
            root = Path(name)
            manifest = self.write_manifest_source(root, b"original")
            replacement = root / "replacement-directory"
            replacement.mkdir()
            real_open = os.open
            descriptors: list[int] = []

            def record_open(path, flags, *args, **kwargs):
                descriptor = real_open(path, flags, *args, **kwargs)
                descriptors.append(descriptor)
                return descriptor

            def replace_with_directory(_path: Path) -> None:
                manifest.unlink()
                replacement.rename(manifest)

            with mock.patch(
                "check_ci_workflow_manifest.os.open", side_effect=record_open
            ):
                with self.assertRaisesRegex(
                    ContractError,
                    rf"path={manifest} opened descriptor is not a regular file; "
                    r"opened_mode=.*opened_identity=",
                    msg=(
                        "manifest capture must reject a raced directory with mode/identity: "
                        f"path={manifest} replacement={replacement}"
                    ),
                ):
                    manifest_bytes_snapshot(root, before_open_hook=replace_with_directory)
            self.assertEqual(
                len(descriptors),
                2,
                "directory replacement must reach one parent and one bounded manifest open: "
                f"path={manifest} descriptors={descriptors!r}",
            )
            for descriptor in descriptors:
                self.assert_fd_closed(
                    descriptor,
                    f"nonregular directory rejection must close every fd path={manifest}",
                )

    @unittest.skipUnless(hasattr(os, "mkfifo"), "FIFO replacement requires os.mkfifo")
    def test_manifest_snapshot_rejects_fifo_promptly_and_closes(self) -> None:
        with tempfile.TemporaryDirectory() as name:
            root = Path(name)
            manifest = self.write_manifest_source(root, b"original")
            program = (
                "import os, sys\n"
                "from pathlib import Path\n"
                "import check_ci_workflow_manifest as checker\n"
                "root = Path(sys.argv[1]); manifest = root / 'scripts/ci_workflow_manifest.json'\n"
                "real_open = os.open; descriptors = []\n"
                "def record_open(path, flags, *args, **kwargs):\n"
                "    fd = real_open(path, flags, *args, **kwargs); descriptors.append(fd); return fd\n"
                "def install_fifo(_path): manifest.unlink(); os.mkfifo(manifest)\n"
                "checker.os.open = record_open\n"
                "try:\n"
                "    checker.manifest_bytes_snapshot(root, before_open_hook=install_fifo)\n"
                "except checker.ContractError as error:\n"
                "    try: os.fstat(descriptors[0])\n"
                "    except OSError: closed = True\n"
                "    else: closed = False\n"
                "    detail = str(error)\n"
                "    ok = str(manifest) in detail and 'not a regular file' in detail and 'opened_mode=' in detail and 'opened_identity=' in detail and closed\n"
                "    print(f'error={detail!r} closed={closed} descriptors={descriptors!r}')\n"
                "    raise SystemExit(0 if ok else 2)\n"
                "raise SystemExit(3)\n"
            )
            try:
                result = subprocess.run(
                    [sys.executable, "-c", program, str(root)],
                    cwd=ROOT / "scripts",
                    check=False,
                    capture_output=True,
                    text=True,
                    timeout=2,
                )
            except subprocess.TimeoutExpired as error:
                self.fail(
                    "O_NONBLOCK must make raced FIFO manifest validation terminate: "
                    f"path={manifest} timeout=2 error={error!r}"
                )
            self.assertEqual(
                result.returncode,
                0,
                "FIFO manifest replacement must reject with path/mode/identity and close: "
                f"path={manifest} stdout={result.stdout!r} stderr={result.stderr!r}",
            )

    def test_manifest_snapshot_final_descriptor_and_path_identity_are_revalidated(self) -> None:
        for mutation in (
            "descriptor-size",
            "descriptor-metadata",
            "pathname-replacement",
            "pathname-removal",
            "pathname-symlink",
            "live-identity",
        ):
            with self.subTest(mutation=mutation), tempfile.TemporaryDirectory() as name:
                root = Path(name)
                manifest = self.write_manifest_source(root, b"original")
                initial_metadata = manifest.stat()
                expected_opened_identity = (
                    initial_metadata.st_dev,
                    initial_metadata.st_ino,
                    initial_metadata.st_size,
                    initial_metadata.st_mtime_ns,
                    initial_metadata.st_ctime_ns,
                )
                replacement = root / "replacement.json"
                replacement.write_bytes(b"replacement")
                symlink_target = root / "symlink-target.json"
                symlink_target.write_bytes(b"original")
                real_open = os.open
                real_manifest_metadata = manifest_file_metadata
                descriptors: list[int] = []
                manifest_metadata_calls = 0

                def record_open(path, flags, *args, **kwargs):
                    descriptor = real_open(path, flags, *args, **kwargs)
                    descriptors.append(descriptor)
                    return descriptor

                def mutate(_path: Path) -> None:
                    if mutation == "descriptor-size":
                        with manifest.open("ab") as stream:
                            stream.write(b"!")
                    elif mutation == "descriptor-metadata":
                        metadata = manifest.stat()
                        os.utime(
                            manifest,
                            ns=(
                                metadata.st_atime_ns,
                                metadata.st_mtime_ns + 1_000_000_000,
                            ),
                        )
                    elif mutation == "pathname-replacement":
                        os.replace(replacement, manifest)
                    elif mutation == "pathname-removal":
                        manifest.unlink()
                    elif mutation == "pathname-symlink":
                        manifest.unlink()
                        manifest.symlink_to(symlink_target.name)

                def staged_manifest_metadata(
                    parent_descriptor: int, path: Path, display: str, maximum: int
                ):
                    nonlocal manifest_metadata_calls
                    manifest_metadata_calls += 1
                    if mutation == "live-identity" and manifest_metadata_calls == 2:
                        return replacement.stat()
                    return real_manifest_metadata(
                        parent_descriptor, path, display, maximum
                    )

                with mock.patch(
                    "check_ci_workflow_manifest.os.open", side_effect=record_open
                ), mock.patch(
                    "check_ci_workflow_manifest.manifest_file_metadata",
                    side_effect=staged_manifest_metadata,
                ):
                    with self.assertRaises(
                        ContractError,
                        msg=(
                            "post-read descriptor/path mutation must fail final validation: "
                            f"path={manifest} mutation={mutation} descriptors={descriptors!r}"
                        ),
                    ) as raised:
                        manifest_bytes_snapshot(root, after_read_hook=mutate)
                diagnostic = str(raised.exception)
                self.assertIn(
                    str(manifest),
                    diagnostic,
                    "final manifest validation diagnostic must name the absolute path: "
                    f"path={manifest} mutation={mutation} diagnostic={diagnostic!r}",
                )
                if mutation == "descriptor-metadata":
                    final_metadata = manifest.stat()
                    expected_final_identity = (
                        final_metadata.st_dev,
                        final_metadata.st_ino,
                        final_metadata.st_size,
                        final_metadata.st_mtime_ns,
                        final_metadata.st_ctime_ns,
                    )
                    for detail in (
                        f"opened_identity={expected_opened_identity!r}",
                        f"final_opened_identity={expected_final_identity!r}",
                    ):
                        self.assertIn(
                            detail,
                            diagnostic,
                            "final descriptor mismatch must report both actionable "
                            f"identities: path={manifest} mutation={mutation} "
                            f"detail={detail!r} diagnostic={diagnostic!r}",
                        )
                elif mutation == "live-identity":
                    live_metadata = replacement.stat()
                    expected_live_identity = (
                        live_metadata.st_dev,
                        live_metadata.st_ino,
                        live_metadata.st_size,
                        live_metadata.st_mtime_ns,
                        live_metadata.st_ctime_ns,
                    )
                    for detail in (
                        f"opened_identity={expected_opened_identity!r}",
                        f"live_identity={expected_live_identity!r}",
                    ):
                        self.assertIn(
                            detail,
                            diagnostic,
                            "final live-path mismatch must report both actionable identities: "
                            f"path={manifest} mutation={mutation} detail={detail!r} "
                            f"diagnostic={diagnostic!r}",
                        )
                elif mutation == "pathname-replacement":
                    self.assertIn(
                        f"opened_identity={expected_opened_identity!r}",
                        diagnostic,
                        "pathname replacement must retain the actionable opened identity: "
                        f"path={manifest} mutation={mutation} diagnostic={diagnostic!r}",
                    )
                    self.assertRegex(
                        diagnostic,
                        r"final_opened_identity=\([^)]*[0-9][^)]*\)",
                        "pathname replacement must report a populated final descriptor "
                        f"identity: path={manifest} mutation={mutation} "
                        f"diagnostic={diagnostic!r}",
                    )
                elif mutation in ("pathname-removal", "pathname-symlink"):
                    self.assertIn(
                        f"opened_identity={expected_opened_identity!r}",
                        diagnostic,
                        "unreadable final live path must retain the opened identity: "
                        f"path={manifest} mutation={mutation} diagnostic={diagnostic!r}",
                    )
                for descriptor in descriptors:
                    self.assert_fd_closed(
                        descriptor,
                        f"final {mutation} rejection must close every fd path={manifest}",
                    )

    def test_manifest_snapshot_fails_closed_without_required_open_capability(self) -> None:
        with tempfile.TemporaryDirectory() as name:
            root = Path(name)
            manifest = self.write_manifest_source(root, b"raw")
            for flag_name in ("O_NOFOLLOW", "O_NONBLOCK"):
                with self.subTest(flag=flag_name):
                    original = getattr(os, flag_name)
                    delattr(os, flag_name)
                    try:
                        with mock.patch(
                            "check_ci_workflow_manifest.os.open",
                            side_effect=AssertionError(f"opened without {flag_name}"),
                        ) as opened:
                            with self.assertRaisesRegex(
                                ContractError,
                                rf"{flag_name} is unavailable; refusing to open "
                                rf"manifest parent path={manifest.parent}",
                                msg=(
                                    "manifest capture must fail before open without capability: "
                                    f"path={manifest} flag={flag_name}"
                                ),
                            ):
                                manifest_bytes_snapshot(root)
                            opened.assert_not_called()
                    finally:
                        setattr(os, flag_name, original)

    def test_load_manifest_uses_fail_closed_descriptor_capture(self) -> None:
        repository = WorkflowRepository()
        original = os.O_NONBLOCK
        try:
            delattr(os, "O_NONBLOCK")
            with mock.patch(
                "check_ci_workflow_manifest.os.open",
                side_effect=AssertionError(
                    "load_manifest reached os.open without required O_NONBLOCK"
                ),
            ) as opened:
                with self.assertRaisesRegex(
                    ContractError,
                    rf"O_NONBLOCK is unavailable; refusing to open manifest parent "
                    rf"path={repository.manifest_path().parent}",
                    msg=(
                        "the production load_manifest boundary must compose fail-closed "
                        "descriptor capture when O_NONBLOCK is absent: "
                        f"root={repository.root} manifest={repository.manifest_path()}"
                    ),
                ):
                    load_manifest(repository.root)
                opened.assert_not_called()
        finally:
            setattr(os, "O_NONBLOCK", original)
            repository.close()

    def test_manifest_snapshot_interruptions_after_binding_close_fd(self) -> None:
        for stage in (
            "first-fstat",
            "identity-1",
            "identity-2",
            "read",
            "final-fstat",
            "identity-3",
            "final-metadata",
            "identity-4",
            "parent-final-fstat",
            "identity-5",
            "identity-6",
        ):
            with self.subTest(stage=stage), tempfile.TemporaryDirectory() as name:
                root = Path(name)
                manifest = self.write_manifest_source(root, b"raw")
                real_open = os.open
                real_fstat = os.fstat
                real_identity = file_identity
                real_metadata = manifest_file_metadata
                real_read = os.read
                descriptors: list[int] = []
                manifest_descriptors: list[int] = []
                fstat_calls = 0
                identity_calls = 0
                metadata_calls = 0

                def record_open(path, flags, *args, **kwargs):
                    descriptor = real_open(path, flags, *args, **kwargs)
                    descriptors.append(descriptor)
                    if Path(path).name == manifest.name:
                        manifest_descriptors.append(descriptor)
                    return descriptor

                def staged_fstat(descriptor: int):
                    nonlocal fstat_calls
                    if manifest_descriptors:
                        fstat_calls += 1
                    if stage == "first-fstat" and fstat_calls == 1:
                        raise KeyboardInterrupt("injected first fstat interruption")
                    if stage == "final-fstat" and fstat_calls == 2:
                        raise KeyboardInterrupt("injected final fstat interruption")
                    if stage == "parent-final-fstat" and fstat_calls == 3:
                        raise KeyboardInterrupt("injected parent final fstat interruption")
                    return real_fstat(descriptor)

                def staged_identity(metadata: os.stat_result):
                    nonlocal identity_calls
                    if manifest_descriptors:
                        identity_calls += 1
                    if stage.startswith("identity-") and identity_calls == int(
                        stage.removeprefix("identity-")
                    ):
                        raise KeyboardInterrupt(
                            f"injected identity call {identity_calls} interruption"
                        )
                    return real_identity(metadata)

                def staged_metadata(
                    parent_descriptor: int, path: Path, display: str, maximum: int
                ):
                    nonlocal metadata_calls
                    metadata_calls += 1
                    if stage == "final-metadata" and metadata_calls == 2:
                        raise KeyboardInterrupt("injected final metadata interruption")
                    return real_metadata(parent_descriptor, path, display, maximum)

                read_effect = (
                    KeyboardInterrupt("injected read interruption")
                    if stage == "read"
                    else real_read
                )
                with mock.patch(
                    "check_ci_workflow_manifest.os.open", side_effect=record_open
                ), mock.patch(
                    "check_ci_workflow_manifest.os.fstat", side_effect=staged_fstat
                ), mock.patch(
                    "check_ci_workflow_manifest.file_identity", side_effect=staged_identity
                ), mock.patch(
                    "check_ci_workflow_manifest.os.read", side_effect=read_effect
                ), mock.patch(
                    "check_ci_workflow_manifest.manifest_file_metadata",
                    side_effect=staged_metadata,
                ):
                    with self.assertRaises(
                        KeyboardInterrupt,
                        msg=(
                            "post-binding interruption must propagate after fd cleanup: "
                            f"path={manifest} stage={stage} fstat_calls={fstat_calls} "
                            f"identity_calls={identity_calls} metadata_calls={metadata_calls} "
                            f"descriptors={descriptors!r}"
                        ),
                    ):
                        manifest_bytes_snapshot(root)
                self.assertEqual(
                    len(descriptors),
                    2,
                    "interruption must occur after parent and manifest descriptor binding: "
                    f"path={manifest} stage={stage} descriptors={descriptors!r} "
                    f"fstat_calls={fstat_calls} identity_calls={identity_calls} "
                    f"metadata_calls={metadata_calls}",
                )
                for descriptor in descriptors:
                    self.assert_fd_closed(
                        descriptor,
                        f"post-binding interruption must close every fd "
                        f"path={manifest} stage={stage}",
                    )

    def test_manifest_interrupt_immediately_after_descriptor_store_closes_fd(self) -> None:
        with tempfile.TemporaryDirectory() as name:
            root = Path(name)
            manifest = self.write_manifest_source(root, b"raw")
            real_open = os.open
            opened: list[tuple[Path, int]] = []
            interrupted_descriptor: int | None = None

            def record_open(path, flags, *args, **kwargs):
                descriptor = real_open(path, flags, *args, **kwargs)
                opened.append((Path(path), descriptor))
                return descriptor

            def trace(frame, event, _argument):
                nonlocal interrupted_descriptor
                if frame.f_code is not manifest_bytes_snapshot.__code__:
                    return trace
                if event != "line":
                    return trace
                descriptor = frame.f_locals.get("descriptor")
                if descriptor is not None:
                    interrupted_descriptor = descriptor
                    raise KeyboardInterrupt(
                        "injected at the first traced line after manifest descriptor binding"
                    )
                return trace

            with mock.patch(
                "check_ci_workflow_manifest.os.open", side_effect=record_open
            ):
                sys.settrace(trace)
                try:
                    with self.assertRaises(
                        KeyboardInterrupt,
                        msg=(
                            "opcode regression must interrupt immediately after the returned "
                            f"manifest fd is bound: path={manifest} opened={opened!r}"
                        ),
                    ):
                        manifest_bytes_snapshot(root)
                finally:
                    sys.settrace(None)
            manifest_descriptors = [
                descriptor
                for path, descriptor in opened
                if path.name == manifest.name
            ]
            self.assertEqual(
                manifest_descriptors,
                [interrupted_descriptor],
                "opcode fixture must interrupt after the exact manifest descriptor binding: "
                f"path={manifest} opened={opened!r} "
                f"interrupted_descriptor={interrupted_descriptor!r}",
            )
            for opened_path, descriptor in opened:
                self.assert_fd_closed(
                    descriptor,
                    "sentinel-protected try/finally must close every bound descriptor after "
                    f"the post-STORE_FAST interruption: manifest={manifest} "
                    f"opened_path={opened_path} opened={opened!r}",
                )

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
            any(
                "regular file could not be opened safely" in error
                or "could not be opened with no-follow nonblocking read-only flags" in error
                for error in errors
            ),
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

    def test_symlinked_manifest_parent_is_rejected_at_checker_boundary(self) -> None:
        repository = WorkflowRepository()
        with tempfile.TemporaryDirectory() as external_name:
            external = Path(external_name) / "scripts"
            try:
                scripts = repository.root / "scripts"
                shutil.copytree(scripts, external)
                shutil.rmtree(scripts)
                scripts.symlink_to(external, target_is_directory=True)
                result = repository.run()
                output = result.stdout + result.stderr
                self.assertEqual(
                    result.returncode,
                    1,
                    "the executable checker must reject a manifest reached through a "
                    "symlinked scripts parent instead of accepting external bytes: "
                    f"root={repository.root} parent={scripts} target={external} "
                    f"stdout={result.stdout!r} stderr={result.stderr!r}",
                )
                for detail in (str(scripts), "real directory", "symbolic link"):
                    self.assertIn(
                        detail,
                        output,
                        "symlinked manifest-parent rejection must identify the unsafe "
                        f"component: root={repository.root} parent={scripts} "
                        f"target={external} detail={detail!r} output={output!r}",
                    )
            finally:
                repository.close()

    def test_manifest_parent_descriptor_defeats_transient_symlink_swap(self) -> None:
        with tempfile.TemporaryDirectory() as name, tempfile.TemporaryDirectory() as external_name:
            root = Path(name)
            manifest = self.write_manifest_source(root, b"reviewed-parent-bytes")
            scripts = manifest.parent
            retained = root / "retained-scripts"
            external = Path(external_name) / "scripts"
            external.mkdir()
            external_manifest = external / manifest.name
            external_manifest.write_bytes(b"external-parent-bytes")
            swapped = False

            def swap_parent(_path: Path) -> None:
                nonlocal swapped
                scripts.rename(retained)
                scripts.symlink_to(external, target_is_directory=True)
                swapped = True

            def restore_parent(_path: Path) -> None:
                scripts.unlink()
                retained.rename(scripts)

            with self.assertRaises(
                ContractError,
                msg=(
                    "a transient manifest-parent symlink swap must fail closed after "
                    f"reading only the pinned directory: root={root} parent={scripts} "
                    f"external={external}"
                ),
            ) as raised:
                manifest_bytes_snapshot(
                    root,
                    before_open_hook=swap_parent,
                    after_open_hook=restore_parent,
                )
            self.assertTrue(
                swapped,
                "transient parent-symlink fixture must run between pinned metadata and "
                f"manifest open: root={root} parent={scripts} external={external}",
            )
            diagnostic = str(raised.exception)
            self.assertIn(
                "manifest parent identity changed during capture",
                diagnostic,
                "transient parent swap must be detected after pinned-directory capture: "
                f"root={root} parent={scripts} external={external} "
                f"external_bytes={external_manifest.read_bytes()!r} "
                f"diagnostic={diagnostic!r}",
            )

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
