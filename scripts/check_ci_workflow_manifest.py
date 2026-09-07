#!/usr/bin/env python3
"""Verify the exact bounded bytes of Finch's reviewed GitHub workflows."""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import stat
import sys
from pathlib import Path
from typing import Any, BinaryIO


ROOT = Path(__file__).resolve().parent.parent
WORKFLOW_DIRECTORY = Path(".github/workflows")
MANIFEST_PATH = Path("scripts/ci_workflow_manifest.json")
SCHEMA = "finch-ci-workflow-manifest:v2"
MAX_WORKFLOW_BYTES = 128 * 1024
MAX_TOTAL_WORKFLOW_BYTES = 1024 * 1024
MAX_MANIFEST_BYTES = 512 * 1024
HASH_CHUNK_BYTES = 64 * 1024


class ContractError(Exception):
    """Actionable failure in the reviewed workflow contract."""


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


def regular_file_metadata(path: Path, display: str, maximum: int) -> os.stat_result:
    try:
        metadata = path.lstat()
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


def open_regular_file(
    path: Path, root: Path, maximum: int
) -> tuple[BinaryIO, os.stat_result, str]:
    display = path.relative_to(root).as_posix()
    metadata = regular_file_metadata(path, display, maximum)
    flags = os.O_RDONLY | getattr(os, "O_CLOEXEC", 0) | getattr(os, "O_NOFOLLOW", 0)
    try:
        descriptor = os.open(path, flags)
    except OSError as error:
        raise ContractError(
            f"{display}: regular file could not be opened safely: {error}"
        ) from error
    try:
        opened = os.fstat(descriptor)
        if not stat.S_ISREG(opened.st_mode):
            raise ContractError(f"{display}: opened path is not a regular file")
        if (opened.st_dev, opened.st_ino, opened.st_size) != (
            metadata.st_dev,
            metadata.st_ino,
            metadata.st_size,
        ):
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


def hash_stream(stream: BinaryIO, expected_size: int, display: str) -> str:
    digest = hashlib.sha256()
    consumed = 0
    while chunk := stream.read(HASH_CHUNK_BYTES):
        consumed += len(chunk)
        if consumed > expected_size:
            raise ContractError(f"{display}: file grew while its reviewed bytes were hashed")
        digest.update(chunk)
    if consumed != expected_size:
        raise ContractError(
            f"{display}: file size changed while hashing; expected={expected_size} "
            f"actual={consumed}"
        )
    return digest.hexdigest()


def exact_digest(path: Path, root: Path, maximum: int) -> tuple[str, int]:
    stream, opened, display = open_regular_file(path, root, maximum)
    with stream:
        digest = hash_stream(stream, opened.st_size, display)
    return digest, opened.st_size


def workflow_records(root: Path) -> dict[str, dict[str, Any]]:
    directory = root / WORKFLOW_DIRECTORY
    try:
        directory_metadata = directory.lstat()
    except OSError as error:
        raise ContractError(f"{WORKFLOW_DIRECTORY}: directory metadata failed: {error}") from error
    if stat.S_ISLNK(directory_metadata.st_mode) or not stat.S_ISDIR(directory_metadata.st_mode):
        raise ContractError(f"{WORKFLOW_DIRECTORY}: must be a real directory, not a link")
    paths = sorted((*directory.glob("*.yml"), *directory.glob("*.yaml")))
    if not paths:
        raise ContractError(f"{WORKFLOW_DIRECTORY}: no workflow documents were found")
    metadata_by_path = {
        path: regular_file_metadata(
            path, path.relative_to(root).as_posix(), MAX_WORKFLOW_BYTES
        )
        for path in paths
    }
    total_bytes = sum(metadata.st_size for metadata in metadata_by_path.values())
    if total_bytes > MAX_TOTAL_WORKFLOW_BYTES:
        raise ContractError(
            f"{WORKFLOW_DIRECTORY}: {total_bytes} aggregate bytes exceeds the reviewed "
            f"{MAX_TOTAL_WORKFLOW_BYTES}-byte bound"
        )
    records: dict[str, dict[str, Any]] = {}
    for path in paths:
        digest, size = exact_digest(path, root, MAX_WORKFLOW_BYTES)
        records[path.name] = {
            "sha256": digest,
            "bytes": size,
            "activated_fixtures": EXPECTED_FIXTURE_MEMBERSHIP.get(path.name),
        }
    return records


def load_manifest(root: Path) -> dict[str, Any]:
    path = root / MANIFEST_PATH
    display = MANIFEST_PATH.as_posix()
    try:
        stream, metadata, display = open_regular_file(path, root, MAX_MANIFEST_BYTES)
        with stream:
            contents = read_exact_bytes(stream, metadata.st_size, display).decode("utf-8")
        manifest = json.loads(contents, object_pairs_hook=json_object)
    except (OSError, UnicodeError, json.JSONDecodeError, ContractError, RecursionError) as error:
        raise ContractError(f"{display}: reviewed manifest could not be loaded: {error}") from error
    if not isinstance(manifest, dict):
        raise ContractError(f"{display}: manifest root must be an object")
    return manifest


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


def compare_contract(root: Path) -> list[str]:
    try:
        actual = workflow_records(root)
        manifest = load_manifest(root)
    except ContractError as error:
        return [str(error)]

    errors: list[str] = []
    errors.extend(evidence_errors(set(actual)))
    expected_root_keys = {"schema", "limits", "pr_active_workflows", "workflows", "fixtures"}
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
    expected_limits = {
        "max_workflow_bytes": MAX_WORKFLOW_BYTES,
        "max_total_workflow_bytes": MAX_TOTAL_WORKFLOW_BYTES,
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
