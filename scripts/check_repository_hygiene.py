#!/usr/bin/env python3
"""Reject tracked native binaries and common transient repository artifacts."""

from __future__ import annotations

import argparse
import hashlib
import re
import struct
import subprocess
import sys
from pathlib import Path, PurePosixPath


ROOT = Path(__file__).resolve().parent.parent
DEFAULT_ALLOWLIST = Path(".github/repository-hygiene-allowlist.tsv")
ALLOWLIST_HEADER = (
    "path",
    "sha256",
    "size",
    "platform",
    "license",
    "provenance",
    "generation_reason",
)
MAX_ALLOWLIST_ENTRIES = 20
MAGIC_PREFIX_BYTES = 4096

MACH_O_MAGICS = {
    bytes.fromhex(value)
    for value in (
        "feedface",
        "feedfacf",
        "cefaedfe",
        "cffaedfe",
        "cafebabe",
        "bebafeca",
        "cafebabf",
        "bfbafeca",
    )
}
GENERATED_SUFFIXES = (
    ".a",
    ".bak",
    ".core",
    ".dll",
    ".dmp",
    ".dump",
    ".dylib",
    ".exe",
    ".gcda",
    ".gcno",
    ".lib",
    ".log",
    ".o",
    ".obj",
    ".orig",
    ".pdb",
    ".profdata",
    ".profraw",
    ".pyc",
    ".rej",
    ".rlib",
    ".so",
    ".swo",
    ".swp",
)
CACHE_COMPONENTS = {"__pycache__", ".mypy_cache", ".pytest_cache", ".ruff_cache"}
GENERATED_NAMES = {".coverage", "coverage.xml", "lcov.info"}
CORE_NAME = re.compile(r"core(?:\.\d+)?$")


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", type=Path, default=ROOT, help="Git worktree to inspect")
    parser.add_argument(
        "--allowlist",
        type=Path,
        default=DEFAULT_ALLOWLIST,
        help="Allowlist path relative to the worktree",
    )
    return parser.parse_args()


def tracked_paths(root: Path) -> list[PurePosixPath]:
    result = subprocess.run(
        ["git", "-C", str(root), "ls-files", "-z"],
        check=True,
        capture_output=True,
    )
    return [PurePosixPath(raw.decode("utf-8")) for raw in result.stdout.split(b"\0") if raw]


def sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for block in iter(lambda: handle.read(64 * 1024), b""):
            digest.update(block)
    return digest.hexdigest()


def load_allowlist(
    root: Path, manifest_path: Path, tracked: set[PurePosixPath]
) -> tuple[set[PurePosixPath], list[str]]:
    errors: list[str] = []
    path = manifest_path if manifest_path.is_absolute() else root / manifest_path
    try:
        lines = [line for line in path.read_text().splitlines() if line and not line.startswith("#")]
    except OSError as error:
        return set(), [f"cannot read allowlist {path}: {error}"]
    if not lines or tuple(lines[0].split("\t")) != ALLOWLIST_HEADER:
        return set(), [f"{path}: expected tab-separated header {ALLOWLIST_HEADER}"]

    entries: set[PurePosixPath] = set()
    for line_number, line in enumerate(lines[1:], start=2):
        fields = line.split("\t")
        if len(fields) != len(ALLOWLIST_HEADER) or any(not field.strip() for field in fields):
            errors.append(f"{path}:{line_number}: every provenance field is required")
            continue
        raw_name, expected_hash, raw_size, platform, license_name, provenance, reason = fields
        name = PurePosixPath(raw_name)
        if name.is_absolute() or ".." in name.parts or name.as_posix() != raw_name:
            errors.append(f"{path}:{line_number}: path must be a normalized repository-relative path")
            continue
        if name in entries:
            errors.append(f"{path}:{line_number}: duplicate allowlist path {name}")
            continue
        entries.add(name)
        if name not in tracked:
            errors.append(f"{path}:{line_number}: allowlisted path is not tracked: {name}")
            continue
        fixture = root.joinpath(*name.parts)
        if fixture.is_symlink() or not fixture.is_file():
            errors.append(f"{path}:{line_number}: allowlisted path must be a regular file: {name}")
            continue
        try:
            expected_size = int(raw_size)
        except ValueError:
            errors.append(f"{path}:{line_number}: size must be a non-negative integer")
            continue
        if expected_size < 0 or fixture.stat().st_size != expected_size:
            errors.append(
                f"{path}:{line_number}: size mismatch for {name}: "
                f"expected {raw_size}, found {fixture.stat().st_size}"
            )
        if not re.fullmatch(r"[0-9a-f]{64}", expected_hash):
            errors.append(f"{path}:{line_number}: sha256 must be 64 lowercase hexadecimal digits")
        elif sha256(fixture) != expected_hash:
            errors.append(f"{path}:{line_number}: sha256 mismatch for {name}")
        # Keep these names visible in diagnostics and make accidental empty metadata impossible.
        _ = platform, license_name, provenance, reason

    if len(entries) > MAX_ALLOWLIST_ENTRIES:
        errors.append(
            f"{path}: {len(entries)} entries exceeds the reviewed limit of {MAX_ALLOWLIST_ENTRIES}"
        )
    return entries, errors


def generated_name_reason(path: PurePosixPath) -> str | None:
    name = path.name.lower()
    if any(part.lower() in CACHE_COMPONENTS for part in path.parts):
        return "generated cache path"
    if name in GENERATED_NAMES or CORE_NAME.fullmatch(name):
        return "generated diagnostic/coverage filename"
    if name.endswith("~") or name.endswith(GENERATED_SUFFIXES):
        return "generated binary/transient suffix"
    return None


def native_magic_reason(path: Path) -> str | None:
    if path.is_symlink() or not path.is_file():
        return None
    with path.open("rb") as handle:
        prefix = handle.read(MAGIC_PREFIX_BYTES)
    if prefix[:4] in MACH_O_MAGICS:
        return "Mach-O magic"
    if prefix.startswith(b"\x7fELF"):
        return "ELF magic"
    if prefix.startswith(b"!<arch>\n"):
        return "static archive magic"
    if prefix.startswith(b"BC\xc0\xde"):
        return "LLVM bitcode magic"
    if prefix.startswith(b"MZ") and len(prefix) >= 64:
        pe_offset = struct.unpack_from("<I", prefix, 0x3C)[0]
        if pe_offset + 4 <= len(prefix) and prefix[pe_offset : pe_offset + 4] == b"PE\0\0":
            return "PE magic"
    return None


def check(root: Path, allowlist_path: Path) -> list[str]:
    root = root.resolve()
    tracked = tracked_paths(root)
    allowed, errors = load_allowlist(root, allowlist_path, set(tracked))
    for name in tracked:
        if name in allowed:
            continue
        reason = generated_name_reason(name)
        if reason is None:
            reason = native_magic_reason(root.joinpath(*name.parts))
        if reason is not None:
            errors.append(f"tracked artifact {name}: {reason}")
    return errors


def main() -> int:
    args = parse_args()
    try:
        errors = check(args.root, args.allowlist)
    except (OSError, subprocess.CalledProcessError, UnicodeDecodeError) as error:
        print(f"repository hygiene check failed to inspect the worktree: {error}", file=sys.stderr)
        return 2
    if errors:
        for error in errors:
            print(f"repository hygiene: {error}", file=sys.stderr)
        return 1
    print("repository hygiene: tracked tree passed")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
