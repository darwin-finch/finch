#!/usr/bin/env python3
"""Enforce Finch's reviewed root Cargo lockfile and nested-lock ignore boundary."""

from __future__ import annotations

import argparse
import os
import selectors
import stat
import subprocess
import sys
import time
from dataclasses import dataclass
from pathlib import Path


ROOT = Path(__file__).resolve().parent.parent
GIT_TIMEOUT_SECONDS = 5
MAX_GIT_QUERIES = 4
MAX_GIT_OUTPUT_BYTES = 64 * 1024
MAX_NESTED_MANIFESTS = 16


class ContractError(Exception):
    """A root lockfile repository invariant was violated."""


@dataclass(frozen=True)
class IgnoreMatch:
    """The verbose Git ignore decision for one path."""

    source: str
    pattern: str
    raw: str


class GitQueries:
    """Run a fixed number of bounded, repository-local Git plumbing queries."""

    def __init__(self, root: Path) -> None:
        self.root = root
        self.count = 0

    def run(
        self, *arguments: str, input_data: str | None = None
    ) -> subprocess.CompletedProcess[str]:
        self.count += 1
        if self.count > MAX_GIT_QUERIES:
            raise ContractError(
                "root lock checker exceeded its Git query budget: "
                f"count={self.count} maximum={MAX_GIT_QUERIES}"
            )

        environment = {
            key: value for key, value in os.environ.items() if not key.startswith("GIT_")
        }
        environment["GIT_CONFIG_NOSYSTEM"] = "1"
        command = [
            "git",
            "-c",
            "core.hooksPath=/dev/null",
            "-c",
            "core.excludesFile=/dev/null",
            "-c",
            "core.fsmonitor=false",
            *arguments,
        ]
        process = subprocess.Popen(
            command,
            cwd=self.root,
            env=environment,
            stdin=subprocess.PIPE if input_data is not None else subprocess.DEVNULL,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
        )
        if input_data is not None:
            assert process.stdin is not None
            process.stdin.write(os.fsencode(input_data))
            process.stdin.close()

        streams = selectors.DefaultSelector()
        assert process.stdout is not None
        assert process.stderr is not None
        streams.register(process.stdout, selectors.EVENT_READ, "stdout")
        streams.register(process.stderr, selectors.EVENT_READ, "stderr")
        output = {"stdout": bytearray(), "stderr": bytearray()}
        deadline = time.monotonic() + GIT_TIMEOUT_SECONDS
        try:
            while streams.get_map():
                remaining = deadline - time.monotonic()
                if remaining <= 0:
                    raise subprocess.TimeoutExpired(command, GIT_TIMEOUT_SECONDS)
                ready = streams.select(remaining)
                if not ready:
                    raise subprocess.TimeoutExpired(command, GIT_TIMEOUT_SECONDS)
                for key, _events in ready:
                    chunk = os.read(key.fileobj.fileno(), 8192)
                    if not chunk:
                        streams.unregister(key.fileobj)
                        continue
                    buffer = output[key.data]
                    buffer.extend(chunk)
                    if len(buffer) > MAX_GIT_OUTPUT_BYTES:
                        raise ContractError(
                            "Git query output exceeded the checker resource bound: "
                            f"stream={key.data} bytes>{MAX_GIT_OUTPUT_BYTES} "
                            f"query={arguments!r}"
                        )
            returncode = process.wait(timeout=max(0.0, deadline - time.monotonic()))
        except (ContractError, subprocess.TimeoutExpired):
            process.kill()
            process.wait()
            raise
        finally:
            streams.close()

        return subprocess.CompletedProcess(
            command,
            returncode,
            stdout=os.fsdecode(bytes(output["stdout"])),
            stderr=os.fsdecode(bytes(output["stderr"])),
        )


def nul_paths(output: str) -> list[Path]:
    """Parse Git's NUL-delimited path output without path-name ambiguity."""
    return [Path(path) for path in output.split("\0") if path]


def ignore_matches(repository: GitQueries, paths: list[Path]) -> dict[Path, IgnoreMatch]:
    """Return the winning ignore rule for every matched path in one Git query."""
    result = repository.run(
        "check-ignore",
        "--no-index",
        "--verbose",
        "-z",
        "--stdin",
        input_data="".join(f"{path}\0" for path in paths),
    )
    if result.returncode not in (0, 1):
        raise ContractError(
            "Git ignore decisions could not be determined: "
            f"status={result.returncode} diagnostic={result.stderr.strip()!r}"
        )

    fields = result.stdout.split("\0")
    if fields and fields[-1] == "":
        fields.pop()
    if len(fields) % 4 != 0:
        raise ContractError(
            "Git returned unparseable NUL-delimited ignore decisions: "
            f"field_count={len(fields)} output={result.stdout!r}"
        )

    requested = set(paths)
    matches: dict[Path, IgnoreMatch] = {}
    for offset in range(0, len(fields), 4):
        source, line, pattern, matched_path = fields[offset : offset + 4]
        path = Path(matched_path)
        raw = f"{source}:{line}:{pattern}\t{matched_path}"
        if path not in requested:
            raise ContractError(
                "Git returned an ignore decision for an unrequested path: "
                f"winning_match={raw!r}"
            )
        if path in matches:
            raise ContractError(
                "Git returned duplicate ignore decisions for one path: "
                f"path={path} output={result.stdout!r}"
            )
        matches[path] = IgnoreMatch(source, pattern, raw)
    return matches


def repository_inventory(repository: GitQueries) -> list[Path]:
    """Read every tracked file relevant to this contract in one bounded snapshot."""
    result = repository.run(
        "ls-files",
        "-z",
        "--",
        ".gitignore",
        ":(glob)**/Cargo.lock",
        ":(glob)**/Cargo.toml",
    )
    if result.returncode != 0:
        raise ContractError(
            "tracked Cargo repository inventory could not be enumerated: "
            f"status={result.returncode} diagnostic={result.stderr.strip()!r}"
        )
    return nul_paths(result.stdout)


def check_root_lock(root: Path) -> None:
    repository = GitQueries(root)
    inventory = repository_inventory(repository)
    if Path(".gitignore") not in inventory:
        raise ContractError(
            ".gitignore must be tracked before it can provide reviewed nested-lock policy"
        )
    tracked_locks = [path for path in inventory if path.name == "Cargo.lock"]
    if Path("Cargo.lock") not in tracked_locks:
        raise ContractError(
            "Cargo.lock must be tracked at the repository root so clean checkouts use "
            f"the reviewed dependency graph: tracked={[str(path) for path in tracked_locks]!r}"
        )
    if tracked_locks != [Path("Cargo.lock")]:
        raise ContractError(
            "only root Cargo.lock may be tracked so clean checkouts use one reviewed "
            f"dependency graph: tracked={[str(path) for path in tracked_locks]!r}"
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

    nested_manifests = [
        path
        for path in inventory
        if path.name == "Cargo.toml" and path != Path("Cargo.toml")
    ]
    if len(nested_manifests) > MAX_NESTED_MANIFESTS:
        raise ContractError(
            "tracked nested Cargo manifest count exceeds the reviewed checker bound: "
            f"count={len(nested_manifests)} maximum={MAX_NESTED_MANIFESTS}"
        )

    root_lock = Path("Cargo.lock")
    nested_locks = [manifest.parent / "Cargo.lock" for manifest in nested_manifests]
    matches = ignore_matches(repository, [root_lock, *nested_locks])

    root_match = matches.get(root_lock)
    if root_match is not None and not root_match.pattern.startswith("!"):
        raise ContractError(
            "root /Cargo.lock must be admitted by .gitignore; "
            f"winning_match={root_match.raw!r}"
        )

    ignore_file = (root / ".gitignore").resolve()
    for nested_lock in nested_locks:
        nested_match = matches.get(nested_lock)
        if nested_match is None or nested_match.pattern.startswith("!"):
            raise ContractError(
                "nested standalone-workspace lockfile must remain ignored: "
                f"path={nested_lock} winning_match="
                f"{nested_match.raw if nested_match is not None else None!r}"
            )
        source = nested_match.source
        source_path = (root / source).resolve() if not Path(source).is_absolute() else Path(source)
        if source_path != ignore_file:
            raise ContractError(
                "nested lockfile ignore must come from the reviewed .gitignore, not private "
                f"Git excludes: path={nested_lock} winning_match={nested_match.raw!r}"
            )

    final_inventory = repository_inventory(repository)
    if final_inventory != inventory:
        raise ContractError(
            "tracked Cargo repository inventory changed during the root-lock check: "
            f"before={[str(path) for path in inventory]!r} "
            f"after={[str(path) for path in final_inventory]!r}"
        )
    final_matches = ignore_matches(repository, [root_lock, *nested_locks])
    if final_matches != matches:
        raise ContractError(
            "Cargo lock ignore policy changed during the root-lock check: "
            f"before={matches!r} after={final_matches!r}"
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
