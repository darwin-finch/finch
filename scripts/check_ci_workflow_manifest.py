#!/usr/bin/env python3
"""Verify the exact bounded bytes of Finch's reviewed GitHub workflows."""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import stat
import sys
from dataclasses import dataclass
from pathlib import Path
from collections.abc import Callable
from typing import Any, BinaryIO


ROOT = Path(__file__).resolve().parent.parent
WORKFLOW_DIRECTORY = Path(".github/workflows")
MANIFEST_PATH = Path("scripts/ci_workflow_manifest.json")
SCHEMA = "finch-ci-workflow-manifest:v3"
WORKFLOW_CANONICAL_EOL = "lf"
MAX_WORKFLOW_BYTES = 128 * 1024
MAX_TOTAL_WORKFLOW_BYTES = 1024 * 1024
MAX_WORKFLOW_FILES = 64
MAX_WORKFLOW_DIRECTORY_ENTRIES = 128
MAX_MANIFEST_BYTES = 512 * 1024
HASH_CHUNK_BYTES = 64 * 1024
WORKFLOW_DIRECTORY_FD_SUPPORTED = (
    os.name != "nt"
    and os.open in os.supports_dir_fd
    and os.stat in os.supports_dir_fd
    and os.scandir in os.supports_fd
)


class ContractError(Exception):
    """Actionable failure in the reviewed workflow contract."""


FileIdentity = tuple[int, int, int, int, int]


@dataclass(frozen=True)
class RepositorySnapshot:
    """Bounded, identity-validated source bytes used by checks and test fixtures."""

    manifest_bytes: bytes
    manifest: dict[str, Any]
    workflow_bytes: dict[str, bytes]
    workflow_records: dict[str, dict[str, Any]]


CANONICAL_CHECKS = (
    "Build Release (aarch64-apple-darwin)",
    "Build Release (x86_64-unknown-linux-gnu)",
    "Runtime Authority (Ubuntu)",
    "Security Audit",
    "Test (macos-14, default)",
    "Test (macos-14, no-default-features)",
    "Test (ubuntu-24.04, default)",
    "Test (ubuntu-24.04, no-default-features)",
    "Toolchain and formatting contract",
    "Toolchain and formatting contract (Windows)",
)
HYGIENE_CHECKS = ("Tracked tree (macos-14)", "Tracked tree (ubuntu-24.04)")
BRAIN_CHECKS = (
    "Isolation boundaries (macos-14)",
    "Isolation boundaries (ubuntu-24.04)",
)
LEGACY_SPREADSHEET_CHECKS = ("Spreadsheet advisory audit",)
LEGACY_SSH_CHECKS = (
    "Compile and test (macos-14, default)",
    "Compile and test (macos-14, no-default-features)",
    "Compile and test (ubuntu-24.04, default)",
    "Compile and test (ubuntu-24.04, no-default-features)",
    "Dependency graph (aarch64-apple-darwin, default)",
    "Dependency graph (aarch64-apple-darwin, no-default-features)",
    "Dependency graph (x86_64-pc-windows-msvc, default)",
    "Dependency graph (x86_64-pc-windows-msvc, no-default-features)",
    "Dependency graph (x86_64-unknown-linux-gnu, default)",
    "Dependency graph (x86_64-unknown-linux-gnu, no-default-features)",
    "Downstream API absence (macos-14, default)",
    "Downstream API absence (macos-14, no-default-features)",
    "Downstream API absence (ubuntu-24.04, default)",
    "Downstream API absence (ubuntu-24.04, no-default-features)",
    "Release build (aarch64-apple-darwin)",
    "Release build (x86_64-unknown-linux-gnu)",
    "SSH absence contract (macos-14)",
    "SSH absence contract (ubuntu-24.04)",
    "SSH absence contract (windows-2025)",
    "SSH advisory audit",
)
OAUTH_CHECKS = ("macos-oauth", "oauth", "windows-compile")
CHATGPT_AUTH_CHECKS = (
    "focused-auth (macos-14)",
    "focused-auth (ubuntu-24.04)",
    "windows-verifier-compile",
)


def checks(*groups: tuple[str, ...]) -> list[str]:
    return sorted(item for group in groups for item in group)


BASELINE_CHECKS = (LEGACY_SPREADSHEET_CHECKS, LEGACY_SSH_CHECKS)
EXPECTED_FIXTURES: dict[str, dict[str, Any]] = {
    "readme_only": {
        "changed_paths": ["README.md"],
        "expected_checks": checks(
            CANONICAL_CHECKS,
            HYGIENE_CHECKS,
            *BASELINE_CHECKS,
            ("Current docs links, claims, and shell syntax",),
        ),
        "expected_count": 34,
    },
    "ordinary_source": {
        "changed_paths": ["src/models/mod.rs"],
        "expected_checks": checks(
            CANONICAL_CHECKS, HYGIENE_CHECKS, BRAIN_CHECKS, *BASELINE_CHECKS
        ),
        "expected_count": 35,
    },
    "brain_effect": {
        "changed_paths": ["src/brain/store.rs", "src/server/handlers.rs"],
        "expected_checks": checks(
            CANONICAL_CHECKS,
            HYGIENE_CHECKS,
            BRAIN_CHECKS,
            *BASELINE_CHECKS,
            ("effect-audit",),
        ),
        "expected_count": 36,
    },
    "manifest_dependency": {
        "changed_paths": ["Cargo.toml", "Cargo.lock"],
        "expected_checks": checks(
            CANONICAL_CHECKS,
            HYGIENE_CHECKS,
            BRAIN_CHECKS,
            *BASELINE_CHECKS,
            OAUTH_CHECKS,
            CHATGPT_AUTH_CHECKS,
        ),
        "expected_count": 41,
    },
    "public_api": {
        "changed_paths": ["src/lib.rs"],
        "expected_checks": checks(
            CANONICAL_CHECKS,
            HYGIENE_CHECKS,
            BRAIN_CHECKS,
            *BASELINE_CHECKS,
            OAUTH_CHECKS,
        ),
        "expected_count": 38,
    },
}

EXPECTED_PR_ACTIVE_WORKFLOWS = [
    "ci.yml",
    "docs.yml",
    "issue-105-oauth.yml",
    "issue-163-effect-audit.yml",
    "issue-185-spreadsheet-advisories.yml",
    "issue-186-ssh-removal.yml",
    "issue-187-subagent-fanout.yml",
    "issue-201-chatgpt-auth.yml",
    "issue-227-setup-preservation.yml",
    "issue-245-cargo-slot.yml",
    "issue-46-atomic-conversation.yml",
    "issue-56-brain-isolation.yml",
    "repository-hygiene.yml",
]

EXPECTED_FIXTURE_MEMBERSHIP = {
    "ci.yml": sorted(EXPECTED_FIXTURES),
    "docs.yml": ["readme_only"],
    "issue-104-chooser-catalog.yml": [],
    "issue-105-oauth.yml": ["manifest_dependency", "public_api"],
    "issue-163-effect-audit.yml": ["brain_effect"],
    "issue-185-spreadsheet-advisories.yml": sorted(EXPECTED_FIXTURES),
    "issue-186-ssh-removal.yml": sorted(EXPECTED_FIXTURES),
    "issue-187-subagent-fanout.yml": [],
    "issue-201-chatgpt-auth.yml": ["manifest_dependency"],
    "issue-227-setup-preservation.yml": [],
    "issue-245-cargo-slot.yml": [],
    "issue-46-atomic-conversation.yml": [],
    "issue-56-brain-isolation.yml": [
        "brain_effect",
        "manifest_dependency",
        "ordinary_source",
        "public_api",
    ],
    "issue-72-capability-contract.yml": [],
    "release.yml": [],
    "repository-hygiene.yml": sorted(EXPECTED_FIXTURES),
}


def json_object(pairs: list[tuple[str, Any]]) -> dict[str, Any]:
    result: dict[str, Any] = {}
    for key, value in pairs:
        if key in result:
            raise ContractError(f"manifest contains duplicate JSON key {key!r}")
        result[key] = value
    return result


def regular_file_metadata(
    path: Path,
    display: str,
    maximum: int,
    directory_fd: int | None = None,
) -> os.stat_result:
    try:
        if directory_fd is None:
            metadata = path.lstat()
        else:
            metadata = os.stat(path.name, dir_fd=directory_fd, follow_symlinks=False)
    except OSError as error:
        raise ContractError(f"{display}: file metadata could not be read: {error}") from error
    if stat.S_ISLNK(metadata.st_mode):
        raise ContractError(f"{display}: must be a regular file, not a symbolic link")
    if not stat.S_ISREG(metadata.st_mode):
        raise ContractError(
            f"{display}: must be a regular file; found mode {stat.filemode(metadata.st_mode)!r}"
        )
    if metadata.st_size > maximum:
        raise ContractError(
            f"{display}: {metadata.st_size} bytes exceeds the reviewed {maximum}-byte bound"
        )
    return metadata


def file_identity(metadata: os.stat_result) -> FileIdentity:
    return (
        metadata.st_dev,
        metadata.st_ino,
        metadata.st_size,
        metadata.st_mtime_ns,
        metadata.st_ctime_ns,
    )


def open_regular_file(
    path: Path,
    root: Path,
    maximum: int,
    before_open_hook: Callable[[Path], None] | None = None,
    directory_fd: int | None = None,
) -> tuple[BinaryIO, os.stat_result, str]:
    display = path.relative_to(root).as_posix()
    metadata = regular_file_metadata(path, display, maximum, directory_fd)
    if before_open_hook is not None:
        before_open_hook(path)
    flags = os.O_RDONLY | getattr(os, "O_CLOEXEC", 0) | getattr(os, "O_NOFOLLOW", 0)
    try:
        if directory_fd is None:
            descriptor = os.open(path, flags)
        else:
            descriptor = os.open(path.name, flags, dir_fd=directory_fd)
    except OSError as error:
        raise ContractError(
            f"{display}: regular file could not be opened safely: {error}"
        ) from error
    try:
        opened = os.fstat(descriptor)
        if not stat.S_ISREG(opened.st_mode):
            raise ContractError(f"{display}: opened path is not a regular file")
        if file_identity(opened) != file_identity(metadata):
            raise ContractError(f"{display}: file identity changed before reading")
        return os.fdopen(descriptor, "rb"), opened, display
    except Exception:
        os.close(descriptor)
        raise


def read_exact_bytes(stream: BinaryIO, expected_size: int, display: str) -> bytes:
    contents = stream.read(expected_size + 1)
    if len(contents) != expected_size:
        raise ContractError(
            f"{display}: file size changed while reading; expected={expected_size} "
            f"actual={len(contents)}"
        )
    return contents


def hash_canonical_workflow_stream(
    stream: BinaryIO, expected_size: int, display: str
) -> tuple[str, int]:
    """Hash reviewed workflow bytes with Git's declared CRLF-to-LF checkout semantics."""
    _, digest, canonical_size = capture_canonical_workflow_stream(
        stream, expected_size, display
    )
    return digest, canonical_size


def capture_canonical_workflow_stream(
    stream: BinaryIO, expected_size: int, display: str
) -> tuple[bytes, str, int]:
    """Capture and hash reviewed workflow bytes through one bounded stream."""
    digest = hashlib.sha256()
    contents = bytearray()
    consumed = 0
    canonical_size = 0
    pending_carriage_return = False
    while consumed < expected_size:
        request_bytes = min(HASH_CHUNK_BYTES, expected_size - consumed)
        chunk = stream.read(request_bytes)
        if not chunk:
            break
        consumed += len(chunk)
        contents.extend(chunk)
        if pending_carriage_return:
            chunk = b"\r" + chunk
            pending_carriage_return = False
        if chunk.endswith(b"\r"):
            chunk = chunk[:-1]
            pending_carriage_return = True
        canonical = chunk.replace(b"\r\n", b"\n")
        canonical_size += len(canonical)
        digest.update(canonical)
    if consumed != expected_size:
        raise ContractError(
            f"{display}: file size changed while hashing; expected={expected_size} "
            f"actual={consumed}"
        )
    if pending_carriage_return:
        canonical_size += 1
        digest.update(b"\r")
    if stream.read(1):
        raise ContractError(f"{display}: file grew while its reviewed bytes were hashed")
    return bytes(contents), digest.hexdigest(), canonical_size


def exact_digest(
    path: Path,
    root: Path,
    maximum: int,
    before_open_hook: Callable[[Path], None] | None = None,
    after_open_hook: Callable[[Path], None] | None = None,
    directory_fd: int | None = None,
) -> tuple[str, int, int, FileIdentity]:
    _, digest, canonical_size, physical_size, identity = exact_workflow(
        path, root, maximum, before_open_hook, after_open_hook, directory_fd
    )
    return digest, canonical_size, physical_size, identity


def exact_workflow(
    path: Path,
    root: Path,
    maximum: int,
    before_open_hook: Callable[[Path], None] | None = None,
    after_open_hook: Callable[[Path], None] | None = None,
    directory_fd: int | None = None,
) -> tuple[bytes, str, int, int, FileIdentity]:
    """Read and hash one bounded regular workflow from the same opened inode."""
    stream, opened, display = open_regular_file(
        path, root, maximum, before_open_hook, directory_fd
    )
    with stream:
        if after_open_hook is not None:
            after_open_hook(path)
        contents, digest, canonical_size = capture_canonical_workflow_stream(
            stream, opened.st_size, display
        )
    return (
        contents,
        digest,
        canonical_size,
        opened.st_size,
        file_identity(opened),
    )


def workflow_snapshot(
    root: Path,
    phase_hook: Callable[[], None] | None = None,
    after_hash_hook: Callable[[], None] | None = None,
    before_open_hook: Callable[[Path], None] | None = None,
    after_open_hook: Callable[[Path], None] | None = None,
    final_scan_hook: Callable[[], None] | None = None,
    post_scan_hook: Callable[[], None] | None = None,
) -> tuple[dict[str, bytes], dict[str, dict[str, Any]]]:
    """Capture bounded workflow bytes after complete directory and leaf validation."""
    directory_identity, paths, directory_fd = _workflow_paths(root, post_scan_hook)
    try:
        return _capture_workflow_snapshot(
            root,
            directory_identity,
            paths,
            directory_fd,
            phase_hook,
            after_hash_hook,
            before_open_hook,
            after_open_hook,
            final_scan_hook,
        )
    finally:
        if directory_fd is not None:
            os.close(directory_fd)


def _capture_workflow_snapshot(
    root: Path,
    directory_identity: FileIdentity,
    paths: list[Path],
    directory_fd: int | None,
    phase_hook: Callable[[], None] | None,
    after_hash_hook: Callable[[], None] | None,
    before_open_hook: Callable[[Path], None] | None,
    after_open_hook: Callable[[Path], None] | None,
    final_scan_hook: Callable[[], None] | None,
) -> tuple[dict[str, bytes], dict[str, dict[str, Any]]]:
    metadata_by_path = {
        path: regular_file_metadata(
            path,
            path.relative_to(root).as_posix(),
            MAX_WORKFLOW_BYTES,
            directory_fd,
        )
        for path in paths
    }
    total_bytes = sum(metadata.st_size for metadata in metadata_by_path.values())
    if total_bytes > MAX_TOTAL_WORKFLOW_BYTES:
        raise ContractError(
            f"{WORKFLOW_DIRECTORY}: {total_bytes} aggregate bytes exceeds the reviewed "
            f"{MAX_TOTAL_WORKFLOW_BYTES}-byte bound"
        )
    if phase_hook is not None:
        phase_hook()
    records: dict[str, dict[str, Any]] = {}
    contents_by_path: dict[Path, bytes] = {}
    opened_identities: dict[Path, FileIdentity] = {}
    opened_total_bytes = 0
    for path in paths:
        stream, opened, display = open_regular_file(
            path, root, MAX_WORKFLOW_BYTES, before_open_hook, directory_fd
        )
        with stream:
            if after_open_hook is not None:
                after_open_hook(path)
            opened_total_bytes += opened.st_size
            if opened_total_bytes > MAX_TOTAL_WORKFLOW_BYTES:
                raise ContractError(
                    f"{WORKFLOW_DIRECTORY}: {opened_total_bytes} aggregate opened bytes "
                    f"exceeds the reviewed {MAX_TOTAL_WORKFLOW_BYTES}-byte bound before "
                    f"capturing {display}"
                )
            contents, digest, canonical_size = capture_canonical_workflow_stream(
                stream, opened.st_size, display
            )
        opened_identity = file_identity(opened)
        initial_identity = file_identity(metadata_by_path[path])
        if opened_identity != initial_identity:
            raise ContractError(
                f"{path.relative_to(root)}: file identity changed after enumeration"
            )
        contents_by_path[path] = contents
        opened_identities[path] = opened_identity
        records[path.name] = {
            "sha256": digest,
            "bytes": canonical_size,
            "activated_fixtures": EXPECTED_FIXTURE_MEMBERSHIP.get(path.name),
        }
    if after_hash_hook is not None:
        after_hash_hook()
    final_directory_identity, final_paths = workflow_paths(root, final_scan_hook)
    initial_names = [path.name for path in paths]
    final_names = [path.name for path in final_paths]
    if final_names != initial_names:
        raise ContractError(
            f"{WORKFLOW_DIRECTORY}: workflow entry set changed while checking; "
            f"before={initial_names!r} after={final_names!r}"
        )
    for path in final_paths:
        display = path.relative_to(root).as_posix()
        final_metadata = regular_file_metadata(path, display, MAX_WORKFLOW_BYTES)
        if file_identity(final_metadata) != opened_identities[path]:
            raise ContractError(f"{display}: file identity changed after hashing")
    if final_directory_identity != directory_identity:
        raise ContractError(f"{WORKFLOW_DIRECTORY}: directory identity changed while checking")
    return ({path.name: contents_by_path[path] for path in paths}, records)


def bounded_workflow_names(entries: Any) -> list[str]:
    """Pull workflow entry names only through the first rejected element."""
    names: list[str] = []
    entry_count = 0
    for entry in entries:
        entry_count += 1
        if entry_count > MAX_WORKFLOW_DIRECTORY_ENTRIES:
            raise ContractError(
                f"{WORKFLOW_DIRECTORY}: directory entry count exceeds the reviewed "
                f"{MAX_WORKFLOW_DIRECTORY_ENTRIES}-entry bound"
            )
        if not entry.name.endswith((".yml", ".yaml")):
            continue
        names.append(entry.name)
        if len(names) > MAX_WORKFLOW_FILES:
            raise ContractError(
                f"{WORKFLOW_DIRECTORY}: workflow count exceeds the reviewed "
                f"{MAX_WORKFLOW_FILES}-file bound"
            )
    return names


def open_workflow_directory(root: Path) -> tuple[os.stat_result, int | None]:
    """Open and identity-pin the workflow directory where CPython supports it."""
    directory = root / WORKFLOW_DIRECTORY
    try:
        directory_metadata = directory.lstat()
    except OSError as error:
        raise ContractError(f"{WORKFLOW_DIRECTORY}: directory metadata failed: {error}") from error
    if stat.S_ISLNK(directory_metadata.st_mode) or not stat.S_ISDIR(directory_metadata.st_mode):
        raise ContractError(f"{WORKFLOW_DIRECTORY}: must be a real directory, not a link")
    if not WORKFLOW_DIRECTORY_FD_SUPPORTED:
        return directory_metadata, None
    flags = os.O_RDONLY | getattr(os, "O_CLOEXEC", 0) | getattr(os, "O_DIRECTORY", 0)
    flags |= getattr(os, "O_NOFOLLOW", 0)
    try:
        directory_fd = os.open(directory, flags)
    except OSError as error:
        raise ContractError(f"{WORKFLOW_DIRECTORY}: directory open failed: {error}") from error
    try:
        opened_metadata = os.fstat(directory_fd)
        if file_identity(opened_metadata) == file_identity(directory_metadata):
            return opened_metadata, directory_fd
    except OSError as error:
        os.close(directory_fd)
        raise ContractError(
            f"{WORKFLOW_DIRECTORY}: opened directory metadata failed: {error}"
        ) from error
    except BaseException:
        os.close(directory_fd)
        raise
    os.close(directory_fd)
    raise ContractError(f"{WORKFLOW_DIRECTORY}: directory identity changed before opening")


def _workflow_paths(
    root: Path, post_scan_hook: Callable[[], None] | None = None
) -> tuple[FileIdentity, list[Path], int | None]:
    directory = root / WORKFLOW_DIRECTORY
    directory_metadata, directory_fd = open_workflow_directory(root)
    try:
        scan_target: Path | int = directory if directory_fd is None else directory_fd
        with os.scandir(scan_target) as entries:
            names = bounded_workflow_names(entries)
            if post_scan_hook is not None:
                post_scan_hook()
    except ContractError:
        if directory_fd is not None:
            os.close(directory_fd)
        raise
    except OSError as error:
        if directory_fd is not None:
            os.close(directory_fd)
        raise ContractError(f"{WORKFLOW_DIRECTORY}: enumeration failed: {error}") from error
    except BaseException:
        if directory_fd is not None:
            os.close(directory_fd)
        raise
    try:
        post_scan_metadata = directory.lstat()
    except OSError as error:
        if directory_fd is not None:
            os.close(directory_fd)
        raise ContractError(
            f"{WORKFLOW_DIRECTORY}: post-enumeration directory metadata failed: {error}"
        ) from error
    if (
        stat.S_ISLNK(post_scan_metadata.st_mode)
        or not stat.S_ISDIR(post_scan_metadata.st_mode)
    ):
        if directory_fd is not None:
            os.close(directory_fd)
        raise ContractError(f"{WORKFLOW_DIRECTORY}: must remain a real directory, not a link")
    if file_identity(post_scan_metadata) != file_identity(directory_metadata):
        if directory_fd is not None:
            os.close(directory_fd)
        raise ContractError(
            f"{WORKFLOW_DIRECTORY}: directory identity changed during enumeration"
        )
    paths = sorted(directory / name for name in names)
    if not paths:
        if directory_fd is not None:
            os.close(directory_fd)
        raise ContractError(f"{WORKFLOW_DIRECTORY}: no workflow documents were found")
    return file_identity(post_scan_metadata), paths, directory_fd


def workflow_paths(
    root: Path, post_scan_hook: Callable[[], None] | None = None
) -> tuple[FileIdentity, list[Path]]:
    directory_identity, paths, directory_fd = _workflow_paths(root, post_scan_hook)
    if directory_fd is not None:
        os.close(directory_fd)
    return directory_identity, paths


def workflow_records(
    root: Path,
    phase_hook: Callable[[], None] | None = None,
    after_hash_hook: Callable[[], None] | None = None,
    before_open_hook: Callable[[Path], None] | None = None,
    after_open_hook: Callable[[Path], None] | None = None,
    final_scan_hook: Callable[[], None] | None = None,
) -> dict[str, dict[str, Any]]:
    _, records = workflow_snapshot(
        root,
        phase_hook,
        after_hash_hook,
        before_open_hook,
        after_open_hook,
        final_scan_hook,
    )
    return records


def load_manifest(
    root: Path,
    before_open_hook: Callable[[Path], None] | None = None,
    after_open_hook: Callable[[Path], None] | None = None,
) -> tuple[dict[str, Any], FileIdentity]:
    _, manifest, identity = manifest_snapshot(
        root, before_open_hook, after_open_hook
    )
    return manifest, identity


def manifest_snapshot(
    root: Path,
    before_open_hook: Callable[[Path], None] | None = None,
    after_open_hook: Callable[[Path], None] | None = None,
) -> tuple[bytes, dict[str, Any], FileIdentity]:
    """Capture bounded raw and parsed manifest bytes from one validated inode."""
    path = root / MANIFEST_PATH
    display = MANIFEST_PATH.as_posix()
    try:
        stream, metadata, display = open_regular_file(
            path, root, MAX_MANIFEST_BYTES, before_open_hook
        )
        with stream:
            if after_open_hook is not None:
                after_open_hook(path)
            contents = read_exact_bytes(stream, metadata.st_size, display)
        manifest = json.loads(contents.decode("utf-8"), object_pairs_hook=json_object)
    except (
        OSError,
        UnicodeError,
        ValueError,
        json.JSONDecodeError,
        ContractError,
        RecursionError,
    ) as error:
        raise ContractError(f"{display}: reviewed manifest could not be loaded: {error}") from error
    if not isinstance(manifest, dict):
        raise ContractError(f"{display}: manifest root must be an object")
    return contents, manifest, file_identity(metadata)


def revalidate_manifest(root: Path, initial_identity: FileIdentity) -> None:
    path = root / MANIFEST_PATH
    display = MANIFEST_PATH.as_posix()
    metadata = regular_file_metadata(path, display, MAX_MANIFEST_BYTES)
    if file_identity(metadata) != initial_identity:
        raise ContractError(
            f"{display}: file identity changed while workflows were being checked"
        )


def repository_snapshot(
    root: Path,
    workflow_phase_hook: Callable[[], None] | None = None,
    after_workflow_hook: Callable[[], None] | None = None,
    workflow_before_open_hook: Callable[[Path], None] | None = None,
    workflow_after_open_hook: Callable[[Path], None] | None = None,
    manifest_before_open_hook: Callable[[Path], None] | None = None,
    manifest_after_open_hook: Callable[[Path], None] | None = None,
    workflow_post_scan_hook: Callable[[], None] | None = None,
) -> RepositorySnapshot:
    """Capture one bounded source snapshot after all identities revalidate."""
    manifest_bytes, manifest, manifest_identity = manifest_snapshot(
        root, manifest_before_open_hook, manifest_after_open_hook
    )
    workflow_bytes, records = workflow_snapshot(
        root,
        phase_hook=workflow_phase_hook,
        before_open_hook=workflow_before_open_hook,
        after_open_hook=workflow_after_open_hook,
        post_scan_hook=workflow_post_scan_hook,
    )
    if after_workflow_hook is not None:
        after_workflow_hook()
    revalidate_manifest(root, manifest_identity)
    return RepositorySnapshot(manifest_bytes, manifest, workflow_bytes, records)


def evidence_errors(actual_names: set[str]) -> list[str]:
    errors: list[str] = []
    if EXPECTED_PR_ACTIVE_WORKFLOWS != sorted(set(EXPECTED_PR_ACTIVE_WORKFLOWS)):
        errors.append("internal PR-active workflow inventory must be sorted and unique")
    missing_active = set(EXPECTED_PR_ACTIVE_WORKFLOWS) - actual_names
    if missing_active:
        errors.append(
            "internal PR-active workflow inventory names missing files: "
            f"{sorted(missing_active)!r}"
        )
    if set(EXPECTED_FIXTURE_MEMBERSHIP) != actual_names:
        errors.append(
            "internal workflow-to-fixture inventory does not cover the exact workflow set: "
            f"expected={sorted(actual_names)!r} actual={sorted(EXPECTED_FIXTURE_MEMBERSHIP)!r}"
        )
    fixture_names = set(EXPECTED_FIXTURES)
    for name, fixture in EXPECTED_FIXTURES.items():
        if set(fixture) != {"changed_paths", "expected_checks", "expected_count"}:
            errors.append(f"internal fixture {name!r} has unexpected fields")
            continue
        paths = fixture["changed_paths"]
        expected_checks = fixture["expected_checks"]
        if not isinstance(paths, list) or not paths or len(paths) != len(set(paths)):
            errors.append(f"internal fixture {name!r} changed paths must be nonempty and unique")
        if not isinstance(expected_checks, list) or expected_checks != sorted(set(expected_checks)):
            errors.append(f"internal fixture {name!r} check names must be sorted and unique")
        if fixture["expected_count"] != len(expected_checks):
            errors.append(
                f"internal fixture {name!r} count does not match its check names: "
                f"count={fixture['expected_count']!r} names={len(expected_checks)}"
            )
    for workflow, membership in EXPECTED_FIXTURE_MEMBERSHIP.items():
        if membership != sorted(set(membership)) or not set(membership) <= fixture_names:
            errors.append(
                f"internal workflow {workflow!r} fixture membership must be sorted, unique, "
                "and name only reviewed fixtures"
            )
    return errors


def compare_contract(
    root: Path,
    workflow_phase_hook: Callable[[], None] | None = None,
    after_workflow_hook: Callable[[], None] | None = None,
    workflow_before_open_hook: Callable[[Path], None] | None = None,
    workflow_after_open_hook: Callable[[Path], None] | None = None,
    manifest_before_open_hook: Callable[[Path], None] | None = None,
    manifest_after_open_hook: Callable[[Path], None] | None = None,
) -> list[str]:
    try:
        snapshot = repository_snapshot(
            root,
            workflow_phase_hook,
            after_workflow_hook,
            workflow_before_open_hook,
            workflow_after_open_hook,
            manifest_before_open_hook,
            manifest_after_open_hook,
        )
    except ContractError as error:
        return [str(error)]

    manifest = snapshot.manifest
    actual = snapshot.workflow_records
    errors: list[str] = []
    errors.extend(evidence_errors(set(actual)))
    expected_root_keys = {
        "schema",
        "workflow_canonical_eol",
        "limits",
        "pr_active_workflows",
        "workflows",
        "fixtures",
    }
    if set(manifest) != expected_root_keys:
        errors.append(
            f"{MANIFEST_PATH}: root keys changed; expected={sorted(expected_root_keys)!r} "
            f"actual={sorted(manifest)!r}"
        )
    if manifest.get("schema") != SCHEMA:
        errors.append(
            f"{MANIFEST_PATH}: schema must be {SCHEMA!r}, "
            f"found {manifest.get('schema')!r}"
        )
    if manifest.get("workflow_canonical_eol") != WORKFLOW_CANONICAL_EOL:
        errors.append(
            f"{MANIFEST_PATH}: workflow canonical EOL must be "
            f"{WORKFLOW_CANONICAL_EOL!r}, "
            f"found {manifest.get('workflow_canonical_eol')!r}"
        )
    expected_limits = {
        "max_workflow_bytes": MAX_WORKFLOW_BYTES,
        "max_total_workflow_bytes": MAX_TOTAL_WORKFLOW_BYTES,
        "max_workflow_files": MAX_WORKFLOW_FILES,
        "max_workflow_directory_entries": MAX_WORKFLOW_DIRECTORY_ENTRIES,
        "max_manifest_bytes": MAX_MANIFEST_BYTES,
    }
    if manifest.get("limits") != expected_limits:
        errors.append(
            f"{MANIFEST_PATH}: reviewed limits changed; expected={expected_limits!r} "
            f"actual={manifest.get('limits')!r}"
        )

    reviewed = manifest.get("workflows")
    if not isinstance(reviewed, dict):
        return errors + [f"{MANIFEST_PATH}: 'workflows' must be a filename-to-record object"]
    actual_names = set(actual)
    expected_names = set(reviewed)
    for name in sorted(expected_names - actual_names):
        errors.append(f"{WORKFLOW_DIRECTORY / name}: reviewed workflow file is missing")
    for name in sorted(actual_names - expected_names):
        errors.append(
            f"{WORKFLOW_DIRECTORY / name}: unreviewed workflow file was added; "
            f"review actual GitHub allocation before updating {MANIFEST_PATH}"
        )
    for name in sorted(actual_names & expected_names):
        expected = reviewed[name]
        if not isinstance(expected, dict):
            errors.append(f"{MANIFEST_PATH}: workflow record {name!r} must be an object")
            continue
        if set(expected) != {"sha256", "bytes", "activated_fixtures"}:
            errors.append(f"{MANIFEST_PATH}: workflow record {name!r} has unexpected keys")
        for field in ("sha256", "bytes", "activated_fixtures"):
            if expected.get(field) != actual[name][field]:
                errors.append(
                    f"{WORKFLOW_DIRECTORY / name}: reviewed {field} changed; "
                    f"expected={expected.get(field)!r} actual={actual[name][field]!r}; "
                    f"review actual GitHub allocation before updating {MANIFEST_PATH}"
                )

    if manifest.get("pr_active_workflows") != EXPECTED_PR_ACTIVE_WORKFLOWS:
        errors.append(
            f"{MANIFEST_PATH}: PR-active workflow inventory changed; "
            f"expected={EXPECTED_PR_ACTIVE_WORKFLOWS!r} "
            f"actual={manifest.get('pr_active_workflows')!r}"
        )
    if manifest.get("fixtures") != EXPECTED_FIXTURES:
        errors.append(
            f"{MANIFEST_PATH}: representative fixture inventories changed; "
            f"expected={EXPECTED_FIXTURES!r} actual={manifest.get('fixtures')!r}"
        )
    return errors


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--root", type=Path, default=ROOT)
    parser.add_argument("--print-records", action="store_true")
    arguments = parser.parse_args()
    root = arguments.root.resolve()
    if arguments.print_records:
        try:
            print(json.dumps(workflow_records(root), indent=2, sort_keys=True))
            return 0
        except ContractError as error:
            print(f"CI workflow manifest: {error}", file=sys.stderr)
            return 1
    errors = compare_contract(root)
    if errors:
        for error in errors:
            print(f"CI workflow manifest: {error}", file=sys.stderr)
        return 1
    print("CI workflow manifest: exact bounded workflow inventory passed")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
