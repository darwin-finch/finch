#!/usr/bin/env python3
"""Measure what it would cost to declare a path as its own subsystem.

Picking a seam by how its directory reads is how you pick a bad one. Before the IPC dependency
inversion, `src/ipc/*_codec.rs` looked like a low-level wire boundary but translated domain types
from five other subsystems, so declaring it would have taken five debt edges on the first day.

This reports the two numbers that decide the question instead:

- **outgoing** — the distinct subsystems the candidate references. Each one that is not already
  below it becomes a debt edge. Above two, it is not a cheap cut, whatever the directory is named.
- **incoming** — who reaches into the candidate, and by which names. A short list of names used
  from outside is a facade waiting to happen; a long one means the boundary is not there yet.

Usage: seam_cost.py src/tools/mcp/ [more/paths ...]
"""

from __future__ import annotations

import argparse
import re
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
        if path.endswith(".rs") and (
            path.startswith("src/")
            or (path.startswith("crates/") and "/src/" in path)
        )
    }


def package_source_path(path: str) -> tuple[str, str] | None:
    """Return the package identity and package-relative Rust source path."""
    if path.startswith("src/"):
        return ".", path[len("src/"):]
    parts = path.split("/")
    if len(parts) >= 4 and parts[0] == "crates" and parts[2] == "src":
        return "/".join(parts[:2]), "/".join(parts[3:])
    return None


def module_path_of(path: str) -> str:
    """The package-relative `crate::` path a source file is reachable at."""
    packaged = package_source_path(path)
    if packaged is None:
        return ""
    _, relative = packaged
    parts = relative.removesuffix(".rs").split("/")
    if parts and parts[-1] in ("mod", "lib"):
        parts.pop()
    return "::".join(parts)


def workspace_package_facades(sources: dict[str, str]) -> tuple[dict[str, str], dict[str, str]]:
    """Return Rust crate names and source-package identities for workspace library facades."""
    facades: dict[str, str] = {}
    crates: dict[str, str] = {}
    for path in sources:
        packaged = package_source_path(path)
        if packaged is None or module_path_of(path) != "":
            continue
        package, _ = packaged
        facades[package] = path
        if package != ".":
            crates[Path(package).name.replace("-", "_")] = package
    return facades, crates


def workspace_package_aliases(
    sources: dict[str, str], facades: dict[str, str], crates: dict[str, str]
) -> dict[str, dict[str, str]]:
    """Map facade re-export aliases such as root `vm` to their workspace package."""
    aliases: dict[str, dict[str, str]] = defaultdict(dict)
    for package, facade in facades.items():
        text = strip_comments_and_tests(sources[facade])
        for dependency, alias in re.findall(
            r"\bpub\s+use\s+([a-z_][a-z0-9_]*)\s+as\s+([a-z_][a-z0-9_]*)\s*;",
            text,
        ):
            target = crates.get(dependency)
            if target:
                aliases[package][alias] = target
    return aliases


def direct_package_references(text: str, crates: dict[str, str]) -> list[str]:
    """Workspace packages named directly, rather than through a local `crate::` alias."""
    return [
        package
        for name, package in crates.items()
        for _ in re.finditer(rf"(?<![:A-Za-z0-9_]){re.escape(name)}\s*::", text)
    ]


def report(root: Path, candidate: str, directories: list[str], files: list[str], sources: dict[str, str]) -> None:
    def owner_of(path: str) -> str:
        return owning_module(directories, path) or "?"

    inside = sorted(path for path in sources if path.startswith(candidate))
    if not inside:
        print(f"{candidate}: no tracked Rust files under this path")
        return
    lines = sum(sources[path].count("\n") for path in inside)
    owners = {owner_of(path) for path in inside}

    file_by_module = {
        (package_source_path(path)[0], module_path_of(path)): path
        for path in sources
        if package_source_path(path) is not None
    }
    package_facades, workspace_crates = workspace_package_facades(sources)
    package_aliases = workspace_package_aliases(sources, package_facades, workspace_crates)

    def owner_of_module(source: str, module: str) -> tuple[str | None, str | None]:
        packaged = package_source_path(source)
        if packaged is None:
            return None, None
        package, _ = packaged
        parts = module.split("::")
        for depth in range(len(parts), 0, -1):
            prefix = "::".join(parts[:depth])
            path = file_by_module.get((package, prefix))
            if path:
                return owner_of(path), path
        alias = module.split("::", 1)[0]
        target_package = package_aliases.get(package, {}).get(alias)
        if target_package:
            target = package_facades[target_package]
            return owner_of(target), target
        return None, None

    def references_from(path: str) -> list[tuple[str | None, str | None]]:
        text = strip_comments_and_tests(sources[path])
        references = [owner_of_module(path, module) for _, module in crate_references(text)]
        references.extend(
            (owner_of(package_facades[package]), package_facades[package])
            for package in direct_package_references(text, workspace_crates)
        )
        return references

    outgoing: Counter[str] = Counter()
    for path in inside:
        for owner, target in references_from(path):
            if target and not target.startswith(candidate) and owner:
                outgoing[owner] += 1

    incoming: dict[str, Counter[str]] = defaultdict(Counter)
    for path in sources:
        if path.startswith(candidate):
            continue
        for _, target in references_from(path):
            if target and target.startswith(candidate):
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
