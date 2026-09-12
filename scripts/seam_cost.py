#!/usr/bin/env python3
"""Measure what it would cost to declare a path as its own subsystem.

Picking a seam by how its directory reads is how you pick a bad one. `src/ipc/*_codec.rs` looks
like a low-level wire boundary and is in fact 5,483 lines that translate domain types from five
other subsystems, so declaring it would take five debt edges on the first day.

This reports the two numbers that decide the question instead:

- **outgoing** — the distinct subsystems the candidate references. Each one that is not already
  below it becomes a debt edge. Above two, it is not a cheap cut, whatever the directory is named.
- **incoming** — who reaches into the candidate, and by which names. A short list of names used
  from outside is a facade waiting to happen; a long one means the boundary is not there yet.

Usage: seam_cost.py src/tools/mcp/ [more/paths ...]
"""

from __future__ import annotations

import argparse
import subprocess
import sys
from collections import Counter, defaultdict
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from generate_interfaces import module_directories, owning_module  # noqa: E402
from rust_scan import crate_references, strip_comments_and_tests, tracked_files  # noqa: E402

ROOT = Path(__file__).resolve().parent.parent


def rust_sources(root: Path, files: list[str]) -> dict[str, str]:
    return {
        path: (root / path).read_text(errors="replace")
        for path in files
        if path.startswith("src/") and path.endswith(".rs")
    }


def module_path_of(path: str) -> str:
    """The `crate::` path a source file is reachable at."""
    parts = path[len("src/"):].removesuffix(".rs").split("/")
    if parts and parts[-1] == "mod":
        parts.pop()
    return "::".join(parts)


def report(root: Path, candidate: str, directories: list[str], files: list[str], sources: dict[str, str]) -> None:
    def owner_of(path: str) -> str:
        return owning_module(directories, path) or "?"

    inside = sorted(path for path in sources if path.startswith(candidate))
    if not inside:
        print(f"{candidate}: no tracked Rust files under this path")
        return
    lines = sum(sources[path].count("\n") for path in inside)
    owners = {owner_of(path) for path in inside}

    file_by_module = {module_path_of(path): path for path in sources}

    def owner_of_module(module: str) -> tuple[str | None, str | None]:
        parts = module.split("::")
        for depth in range(len(parts), 0, -1):
            prefix = "::".join(parts[:depth])
            path = file_by_module.get(prefix)
            if path:
                return owner_of(path), path
        return None, None

    outgoing: Counter[str] = Counter()
    for path in inside:
        for _, module in crate_references(strip_comments_and_tests(sources[path])):
            owner, target = owner_of_module(module)
            if target and not target.startswith(candidate) and owner:
                outgoing[owner] += 1

    incoming: dict[str, Counter[str]] = defaultdict(Counter)
    prefix = module_path_of(candidate.rstrip("/") + "/mod.rs")
    for path, text in sources.items():
        if path.startswith(candidate):
            continue
        for _, module in crate_references(strip_comments_and_tests(text)):
            if module == prefix or module.startswith(f"{prefix}::"):
                incoming[owner_of(path)][path] += 1

    print(f"\n{candidate}  —  {len(inside)} files, {lines} lines, currently owned by {sorted(owners)}")
    print(f"  outgoing: {len(outgoing)} subsystem(s)" + ("  ← above two is not a cheap cut" if len(outgoing) > 2 else ""))
    for owner, count in outgoing.most_common():
        print(f"    {owner:<12} {count} reference(s)")
    if not outgoing:
        print("    none — nothing here reaches outside the candidate")
    total = sum(sum(counter.values()) for counter in incoming.values())
    print(f"  incoming: {total} reference(s) from {len(incoming)} subsystem(s)")
    for owner, counter in sorted(incoming.items(), key=lambda item: -sum(item[1].values())):
        top = ", ".join(f"{path}×{count}" for path, count in counter.most_common(3))
        print(f"    {owner:<12} {sum(counter.values()):>4}  {top}")


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("paths", nargs="+", help="candidate paths, e.g. src/tools/mcp/")
    parser.add_argument("--root", default=str(ROOT))
    arguments = parser.parse_args()
    root = Path(arguments.root).resolve()
    files = tracked_files(root)
    directories = module_directories(files)
    sources = rust_sources(root, files)
    for candidate in arguments.paths:
        report(root, candidate.rstrip("/") + "/" if not candidate.endswith(".rs") else candidate,
               directories, files, sources)
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except subprocess.CalledProcessError as error:
        print(f"seam_cost: {error}", file=sys.stderr)
        raise SystemExit(2) from error
