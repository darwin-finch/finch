#!/usr/bin/env python3
"""Enforce Finch's reviewed root Cargo lockfile and nested-lock ignore boundary."""

from __future__ import annotations

import argparse
import os
import stat
import subprocess
import sys
from dataclasses import dataclass
from pathlib import Path


ROOT = Path(__file__).resolve().parent.parent
GIT_TIMEOUT_SECONDS = 5
MAX_NESTED_MANIFESTS = 16


class ContractError(Exception):
    """A root lockfile repository invariant was violated."""


@dataclass(frozen=True)
class IgnoreMatch:
    """The verbose Git ignore decision for one path."""

    source: str
    pattern: str
    raw: str


def git(root: Path, *arguments: str) -> subprocess.CompletedProcess[str]:
    environment = {
        key: value for key, value in os.environ.items() if not key.startswith("GIT_")
    }
    environment["GIT_CONFIG_NOSYSTEM"] = "1"
    return subprocess.run(
        [
            "git",
            "-c",
            "core.hooksPath=/dev/null",
            "-c",
            "core.excludesFile=/dev/null",
            "-c",
            "core.fsmonitor=false",
            *arguments,
        ],
        cwd=root,
        env=environment,
        check=False,
        capture_output=True,
        text=True,
        timeout=GIT_TIMEOUT_SECONDS,
    )


def ignore_match(root: Path, path: Path) -> tuple[int, IgnoreMatch | None, str]:
    result = git(root, "check-ignore", "--no-index", "--verbose", "--", str(path))
    raw = result.stdout.strip() or result.stderr.strip()
    if result.returncode != 0:
        return result.returncode, None, raw
    try:
        rule, _matched_path = raw.split("\t", 1)
        source, _line, pattern = rule.rsplit(":", 2)
    except ValueError as error:
        raise ContractError(
            f"Git returned an unparseable ignore decision for path={path}: {raw!r}"
        ) from error
    return result.returncode, IgnoreMatch(source, pattern, raw), raw


def check_root_lock(root: Path) -> None:
    tracked = git(root, "ls-files", "--error-unmatch", "--", "Cargo.lock")
    if tracked.returncode != 0:
        raise ContractError(
            "Cargo.lock must be tracked so clean checkouts use the reviewed dependency graph"
        )

    lock_path = root / "Cargo.lock"
    try:
        metadata = lock_path.lstat()
    except OSError as error:
        raise ContractError(
            "Cargo.lock is tracked but missing from the worktree; restore the reviewed "
            f"dependency graph: {error}"
        ) from error
    if not stat.S_ISREG(metadata.st_mode):
        raise ContractError(
            "Cargo.lock must be a regular file in the worktree; "
            f"path={lock_path} mode={stat.filemode(metadata.st_mode)}"
        )

    root_status, root_match, root_diagnostic = ignore_match(root, Path("Cargo.lock"))
    if root_status == 0 and root_match is not None and not root_match.pattern.startswith("!"):
        raise ContractError(
            "root /Cargo.lock must be admitted by .gitignore; "
            f"winning_match={root_match.raw!r}"
        )
    if root_status != 1:
        if root_status != 0 or root_match is None:
            raise ContractError(
                "root /Cargo.lock ignore status could not be determined: "
                f"status={root_status} diagnostic={root_diagnostic!r}"
            )

    manifests = git(root, "ls-files", "-z", "--", ":(glob)**/Cargo.toml")
    if manifests.returncode != 0:
        raise ContractError(
            "tracked nested Cargo manifests could not be enumerated: "
            f"status={manifests.returncode} diagnostic={manifests.stderr.strip()!r}"
        )
    nested_manifests = [
        Path(path)
        for path in manifests.stdout.split("\0")
        if path and Path(path) != Path("Cargo.toml")
    ]
    if len(nested_manifests) > MAX_NESTED_MANIFESTS:
        raise ContractError(
            "tracked nested Cargo manifest count exceeds the reviewed checker bound: "
            f"count={len(nested_manifests)} maximum={MAX_NESTED_MANIFESTS}"
        )

    ignore_file = (root / ".gitignore").resolve()
    for nested_manifest in nested_manifests:
        nested_lock = nested_manifest.parent / "Cargo.lock"
        nested_status, nested_match, nested_diagnostic = ignore_match(root, nested_lock)
        if (
            nested_status != 0
            or nested_match is None
            or nested_match.pattern.startswith("!")
        ):
            raise ContractError(
                "nested standalone-workspace lockfile must remain ignored: "
                f"path={nested_lock} status={nested_status} diagnostic={nested_diagnostic!r}"
            )
        source = nested_match.source
        source_path = (root / source).resolve() if not Path(source).is_absolute() else Path(source)
        if source_path != ignore_file:
            raise ContractError(
                "nested lockfile ignore must come from the reviewed .gitignore, not private "
                f"Git excludes: path={nested_lock} winning_match={nested_match.raw!r}"
            )


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--root", type=Path, default=ROOT)
    arguments = parser.parse_args()
    root = arguments.root.resolve()
    try:
        check_root_lock(root)
    except (ContractError, subprocess.TimeoutExpired) as error:
        print(f"root Cargo.lock contract failed: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
