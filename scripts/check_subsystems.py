#!/usr/bin/env python3
"""Check subsystems.toml: every tracked file has one owner, and the cross-subsystem
dependency record matches the production imports in the tree.

The import scan is a lint heuristic, not a compiler. It reads `crate::<module>` paths in
production Rust after blanking comments and string/char literals, inline
`#[cfg(test)] mod … { … }` blocks, and files declared by `#[cfg(test)] mod name;`. It does not
expand macros or follow `use` re-exports, so a dependency hidden behind either can escape it.
"""

from __future__ import annotations

import argparse
import re
import subprocess
import sys
import tomllib
from collections import defaultdict
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
MANIFEST = "subsystems.toml"
SUPPORTED_VERSION = 1
GLOBAL = "global"
EXCLUDED = "excluded"

CRATE_PATH = re.compile(r"\bcrate::([a-z_][a-z0-9_]*)")
CRATE_GROUP = re.compile(r"\bcrate::\s*\{")
GROUP_ITEM = re.compile(r"\s*([a-z_][a-z0-9_]*)")
RAW_STRING = re.compile(r'[bc]?r(#*)"')
CHAR_LITERAL = re.compile(r"'(?:\\(?:u\{[0-9a-fA-F]+\}|x[0-9a-fA-F]{2}|.)|[^\\'\n])'")
ROUTING_HEADING = "### Subsystem capsules"
# Capsules supplement the root instructions; they may link these sections but never restate them.
UNIVERSAL_HEADINGS = (
    "## Invariants", "## Development Guidelines", "### Testing (mandatory)",
    "### Reporting status to a human", "## Key Principles",
)
PUBLIC_MODULE = re.compile(r"^[ \t]*pub(?:[ \t]*\([^)]*\))?[ \t]+mod[ \t]+(\w+)", re.M)
TEST_MODULE_BLOCK = re.compile(
    r"#\[cfg\(test\)\]\s*(?:#\[[^\]]*\]\s*)*(?:pub(?:\([^)]*\))?\s+)?mod\s+\w+\s*\{"
)
TEST_MODULE_FILE = re.compile(
    r"#\[cfg\(test\)\]\s*(?:#\[[^\]]*\]\s*)*(?:pub(?:\([^)]*\))?\s+)?mod\s+(\w+)\s*;"
)


class ManifestError(Exception):
    pass


def tracked_files(root: Path) -> list[str]:
    result = subprocess.run(
        ["git", "-C", str(root), "ls-files", "-z"], capture_output=True, check=False,
    )
    if result.returncode:
        raise ManifestError(f"cannot list tracked files: {result.stderr.decode().strip()}")
    return sorted(path for path in result.stdout.decode().split("\0") if path)


def load_manifest(root: Path) -> dict:
    path = root / MANIFEST
    try:
        return tomllib.loads(path.read_text())
    except FileNotFoundError as error:
        raise ManifestError(f"{MANIFEST} is missing") from error
    except tomllib.TOMLDecodeError as error:
        raise ManifestError(f"{MANIFEST} is not valid TOML: {error}") from error


def valid_records(manifest: dict) -> list[dict]:
    """Subsystem records with a usable id; the rest are reported by manifest_shape_errors."""
    return [
        record for record in manifest.get("subsystem", [])
        if isinstance(record, dict) and isinstance(record.get("id"), str) and record["id"]
    ]


def owner_entries(manifest: dict) -> list[tuple[str, str]]:
    """(path, owner) pairs; a path ending in '/' is a prefix, anything else an exact file."""
    entries = [
        (path, record["id"]) for record in valid_records(manifest)
        for path in record.get("paths", [])
    ]
    entries += [(path, GLOBAL) for path in manifest.get("global", {}).get("paths", [])]
    entries += [(item.get("path", ""), EXCLUDED) for item in manifest.get("excluded", [])]
    return entries


def matches(entry: str, path: str) -> bool:
    return path.startswith(entry) if entry.endswith("/") else path == entry


def resolve(path: str, entries: list[tuple[str, str]]) -> tuple[str | None, list[str]]:
    """Longest matching entry wins; two matches of equal length are ambiguous."""
    candidates = [(entry, owner) for entry, owner in entries if matches(entry, path)]
    if not candidates:
        return None, []
    longest = max(len(entry) for entry, _ in candidates)
    winners = sorted({owner for entry, owner in candidates if len(entry) == longest})
    return (winners[0] if len(winners) == 1 else None), winners


def manifest_shape_errors(manifest: dict, files: list[str], root: Path) -> list[str]:
    errors: list[str] = []
    if manifest.get("version") != SUPPORTED_VERSION:
        errors.append(f"version must be {SUPPORTED_VERSION}; actual={manifest.get('version')!r}")
    records = manifest.get("subsystem", [])
    for position, record in enumerate(records, start=1):
        if not isinstance(record, dict) or not isinstance(record.get("id"), str) or not record["id"]:
            errors.append(f"subsystem record {position} needs a string id")
    records = valid_records(manifest)
    ids = [record.get("id") for record in records]
    duplicates = sorted({i for i in ids if ids.count(i) > 1})
    if duplicates:
        errors.append(f"duplicate subsystem ids: {duplicates}")
    if {GLOBAL, EXCLUDED} & set(ids):
        errors.append(f"subsystem ids {GLOBAL!r} and {EXCLUDED!r} are reserved")
    for item in manifest.get("excluded", []):
        if not item.get("reason"):
            errors.append(f"excluded path {item.get('path')!r} needs a reason")

    known = set(ids)
    tracked = set(files)
    layers = {record.get("id"): record.get("layer") for record in records}
    for record in records:
        rid = record.get("id")
        for field in ("docs", "instructions"):
            for target in record.get(field, []):
                if target not in tracked:
                    errors.append(f"subsystem {rid!r}: {field} target is not a tracked file: {target}")
        declared: set[str] = set()
        for target in record.get("depends_on", []):
            if target not in known:
                errors.append(f"subsystem {rid!r}: depends_on names unknown subsystem {target!r}")
            elif not isinstance(layers.get(rid), int) or not isinstance(layers.get(target), int):
                errors.append(f"subsystem {rid!r}: depends_on {target!r} needs layers on both records")
            elif layers[target] >= layers[rid]:
                errors.append(
                    f"subsystem {rid!r} (layer {layers[rid]}): depends_on {target!r} "
                    f"(layer {layers[target]}) must point strictly down-layer; record it as debt"
                )
            if target in declared:
                errors.append(f"subsystem {rid!r}: edge to {target!r} is declared twice")
            declared.add(target)
        for debt in record.get("debt", []):
            target = debt.get("to")
            if target not in known:
                errors.append(f"subsystem {rid!r}: debt names unknown subsystem {target!r}")
            if not isinstance(debt.get("issue"), int):
                errors.append(f"subsystem {rid!r}: debt edge to {target!r} needs an integer issue")
            if target in declared:
                errors.append(f"subsystem {rid!r}: edge to {target!r} is declared twice")
            declared.add(target)
        if rid in declared:
            errors.append(f"subsystem {rid!r} declares an edge to itself")

    entries = owner_entries(manifest)
    for entry, owner in entries:
        if not entry:
            errors.append(f"{owner}: empty path entry")
        elif not any(matches(entry, path) for path in files):
            errors.append(f"{owner}: path entry matches no tracked file: {entry}")
    for path in files:
        owner, winners = resolve(path, entries)
        if not winners:
            errors.append(f"unowned tracked file: {path}")
        elif owner is None:
            errors.append(f"ambiguous owner for {path}: {winners}")
    return errors


def blank_comments_and_literals(text: str) -> str:
    """Replace comments and string/char literal contents with spaces, keeping newlines.

    Handles nested block comments, escapes, raw strings (`r#"…"#`), and tells char literals
    from lifetimes, so braces and `crate::` inside literals never reach the scan.
    """
    out = list(text)
    length = len(text)

    def blank(start: int, end: int) -> None:
        for position in range(start, min(end, length)):
            if out[position] != "\n":
                out[position] = " "

    index = 0
    while index < length:
        char = text[index]
        pair = text[index:index + 2]
        if pair == "//":
            end = text.find("\n", index)
            end = length if end < 0 else end
            blank(index, end)
            index = end
        elif pair == "/*":
            depth, end = 1, index + 2
            while end < length and depth:
                if text.startswith("/*", end):
                    depth, end = depth + 1, end + 2
                elif text.startswith("*/", end):
                    depth, end = depth - 1, end + 2
                else:
                    end += 1
            blank(index, end)
            index = end
        elif char in "rbc" and (index == 0 or not (text[index - 1].isalnum() or text[index - 1] == "_")):
            raw = RAW_STRING.match(text, index)
            if raw is None:
                index += 1
                continue
            closing = '"' + raw.group(1)
            end = text.find(closing, raw.end())
            end = length if end < 0 else end + len(closing)
            blank(raw.end(), end - len(closing))
            index = end
        elif char == '"':
            end = index + 1
            while end < length and text[end] != '"':
                end += 2 if text[end] == "\\" else 1
            blank(index + 1, end)
            index = end + 1
        elif char == "'":
            literal = CHAR_LITERAL.match(text, index)
            if literal is None:  # a lifetime or label
                index += 1
                continue
            blank(index + 1, literal.end() - 1)
            index = literal.end()
        else:
            index += 1
    return "".join(out)


def strip_comments_and_tests(text: str) -> str:
    """Blank comments, literals, and inline test modules while keeping line numbers stable."""
    text = blank_comments_and_literals(text)
    pieces: list[str] = []
    position = 0
    for match in TEST_MODULE_BLOCK.finditer(text):
        if match.start() < position:
            continue
        depth, index = 1, match.end()
        while index < len(text) and depth:
            depth += {"{": 1, "}": -1}.get(text[index], 0)
            index += 1
        pieces.append(text[position:match.start()])
        pieces.append(re.sub(r"[^\n]", " ", text[match.start():index]))
        position = index
    pieces.append(text[position:])
    return "".join(pieces)


def crate_references(text: str) -> list[tuple[int, str]]:
    """(offset, top-level module) for `crate::m` paths and each item of `crate::{a::x, b}`."""
    found = [(match.start(), match.group(1)) for match in CRATE_PATH.finditer(text)]
    for group in CRATE_GROUP.finditer(text):
        depth, index, item_start = 1, group.end(), group.end()
        while index < len(text) and depth:
            char = text[index]
            if char == "{":
                depth += 1
            elif char == "}":
                depth -= 1
            if depth == 1 and char == "," or depth == 0:
                item = GROUP_ITEM.match(text, item_start, index)
                if item:
                    found.append((item.start(1), item.group(1)))
                item_start = index + 1
            index += 1
    return found


def test_module_files(sources: dict[str, str]) -> set[str]:
    """Files declared by `#[cfg(test)] mod name;` are test code."""
    found: set[str] = set()
    for path, text in sources.items():
        parent = Path(path).parent
        stem = Path(path).stem
        base = parent if stem in ("mod", "lib", "main") else parent / stem
        for name in TEST_MODULE_FILE.findall(blank_comments_and_literals(text)):
            found.add((base / f"{name}.rs").as_posix())
            found.add((base / name / "mod.rs").as_posix())
    return found


def observed_edges(
    manifest: dict, files: list[str], root: Path,
) -> tuple[dict[tuple[str, str], list[str]], list[str]]:
    entries = owner_entries(manifest)
    graph_ids = {record["id"] for record in valid_records(manifest) if "layer" in record}
    errors: list[str] = []
    rust = [path for path in files if path.startswith("src/") and path.endswith(".rs")]
    sources = {path: (root / path).read_text(errors="replace") for path in rust}
    test_files = test_module_files(sources)

    # Edge targets are attributed by top-level module, so a module split across owners would
    # misattribute edges; refuse that until the scan resolves deeper paths.
    owners_by_module: dict[str, set[str]] = defaultdict(set)
    for path in rust:
        parts = path.split("/")
        module = parts[1][:-3] if len(parts) == 2 else parts[1]
        owner, _ = resolve(path, entries)
        if owner:
            owners_by_module[module].add(owner)
    module_owner: dict[str, str] = {}
    for module, owners in sorted(owners_by_module.items()):
        if len(owners) > 1 and module != "bin":
            errors.append(f"src/{module}: top-level module is split across owners {sorted(owners)}")
        module_owner[module] = sorted(owners)[0]

    edges: dict[tuple[str, str], list[str]] = defaultdict(list)
    for path in rust:
        owner, _ = resolve(path, entries)
        if owner in (None, GLOBAL, EXCLUDED) or path in test_files:
            continue
        if owner not in graph_ids:
            errors.append(f"{path}: production Rust owned by {owner!r}, which has no layer")
            continue
        text = strip_comments_and_tests(sources[path])
        for offset, module in crate_references(text):
            target = module_owner.get(module)
            if target in (None, GLOBAL, EXCLUDED, owner):
                continue
            line = text.count("\n", 0, offset) + 1
            edges[(owner, target)].append(f"{path}:{line} crate::{module}")
    return edges, errors


def dependency_errors(manifest: dict, edges: dict[tuple[str, str], list[str]]) -> list[str]:
    layers = {record["id"]: record.get("layer") for record in valid_records(manifest)}
    declared: dict[tuple[str, str], str] = {}
    for record in valid_records(manifest):
        for target in record.get("depends_on", []):
            declared[(record["id"], target)] = "depends_on"
        for debt in record.get("debt", []):
            declared[(record["id"], debt.get("to"))] = "debt"
    errors: list[str] = []
    for (source, target), references in sorted(edges.items()):
        if (source, target) not in declared:
            shown = "; ".join(references[:3])
            direction = (
                "down-layer; add to depends_on"
                if isinstance(layers.get(source), int) and isinstance(layers.get(target), int)
                and layers[target] < layers[source]
                else "not down-layer; fix the import or record it as debt with an issue"
            )
            errors.append(
                f"undeclared dependency {source} (layer {layers.get(source)}) -> {target} "
                f"(layer {layers.get(target)}), {direction}: {shown}"
            )
    for (source, target), kind in sorted(declared.items()):
        if (source, target) not in edges:
            errors.append(
                f"stale {kind} edge {source} -> {target}: no production reference remains; "
                f"remove it from {MANIFEST}"
            )
    return errors


def alias_errors(root: Path, files: list[str]) -> list[str]:
    """Every directory with instruction files serves the same text under AGENTS.md and CLAUDE.md.

    The root keeps its historical form: AGENTS.md is a symlink to CLAUDE.md. Nested capsules use
    the import form instead: AGENTS.md holds the text and CLAUDE.md is exactly `@AGENTS.md`
    (Claude Code expands the import; AGENTS.md-only agents read the file). Symlinks are not
    allowed below the root because Finch's `tree-list` rejects them, which would break workspace
    listing of the capsule's directory.
    """
    tracked = set(files)
    errors: list[str] = []
    if "AGENTS.md" not in tracked:
        errors.append("AGENTS.md must be a symlink to its sibling CLAUDE.md; the root AGENTS.md is missing")
    for path in files:
        name = Path(path).name
        if name not in ("AGENTS.md", "CLAUDE.md"):
            continue
        agents = Path(path).with_name("AGENTS.md").as_posix()
        claude = Path(path).with_name("CLAUDE.md").as_posix()
        if "/" not in path:  # repository root
            if name == "AGENTS.md":
                link = root / path
                if not link.is_symlink() or link.readlink().as_posix() != "CLAUDE.md":
                    errors.append("AGENTS.md must be a symlink to its sibling CLAUDE.md so both agent entry points agree")
            continue
        if (root / path).is_symlink():
            errors.append(
                f"{path} must not be a symlink: Finch's tree-list rejects symlinks, so use a real "
                "AGENTS.md and a CLAUDE.md containing only `@AGENTS.md`"
            )
        elif name == "AGENTS.md" and claude not in tracked:
            errors.append(f"{path} needs a sibling CLAUDE.md containing only `@AGENTS.md` so Claude Code reads it too")
        elif name == "CLAUDE.md":
            text = (root / path).read_text() if (root / path).is_file() else ""
            if agents not in tracked or text.strip() != "@AGENTS.md":
                errors.append(
                    f"{path} must contain only `@AGENTS.md` next to a real AGENTS.md, so both agent "
                    "entry points read one text"
                )
    return errors


def capsule_errors(manifest: dict, root: Path) -> list[str]:
    """Every capsule is routed from the root instructions and never restates universal sections."""
    capsules = [
        (record["id"], path) for record in valid_records(manifest)
        for path in record.get("instructions", [])
    ]
    if not capsules:
        return []
    instructions = root / "CLAUDE.md"
    text = instructions.read_text() if instructions.is_file() else ""
    section = re.search(
        rf"^{re.escape(ROUTING_HEADING)}\s*$(.*?)(?=^#{{1,3}} |\Z)", text, re.M | re.S,
    )
    errors: list[str] = []
    if section is None:
        errors.append(f"CLAUDE.md: the {ROUTING_HEADING!r} routing table is missing; it must list every capsule")
    for owner, path in capsules:
        if section is not None and path not in section.group(1):
            errors.append(f"subsystem {owner!r}: capsule {path} is missing from the root routing table")
        capsule = root / path
        body = capsule.read_text() if capsule.is_file() else ""
        for heading in UNIVERSAL_HEADINGS:
            # Match the heading text at any level and case, so `### invariants (vm)` also counts.
            title = re.escape(heading.lstrip("#").strip())
            if re.search(rf"^#+\s*{title}(?!\w)", body, re.M | re.I):
                errors.append(
                    f"{path}: capsule restates the root's universal section {heading!r}; link to it instead"
                )
    return errors


def facade_errors(manifest: dict, files: list[str], root: Path) -> list[str]:
    """A facade file exposes its subsystem only through `pub use`; no child module is public.

    Private modules already make reaching past the facade a compile error; this guards the one
    thing the compiler cannot: someone re-adding `pub mod` to the facade file.
    """
    errors: list[str] = []
    for record in valid_records(manifest):
        facade = record.get("facade")
        if facade is None:
            continue
        owner = record["id"]
        if not isinstance(facade, str) or facade not in files:
            errors.append(f"subsystem {owner!r}: facade must name a tracked file; actual={facade!r}")
            continue
        text = blank_comments_and_literals((root / facade).read_text(errors="replace"))
        for match in PUBLIC_MODULE.finditer(text):
            line = text.count("\n", 0, match.start()) + 1
            errors.append(
                f"{facade}:{line}: public module `{match.group(1)}` in the {owner!r} facade; "
                "make it private and re-export the items callers need with `pub use`"
            )
    return errors


def check(root: Path) -> list[str]:
    try:
        manifest = load_manifest(root)
        files = tracked_files(root)
    except ManifestError as error:
        return [str(error)]
    errors = manifest_shape_errors(manifest, files, root)
    edges, scan_errors = observed_edges(manifest, files, root)
    errors += scan_errors
    errors += dependency_errors(manifest, edges)
    errors += alias_errors(root, files)
    errors += capsule_errors(manifest, root)
    errors += facade_errors(manifest, files, root)
    return errors


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--root", type=Path, default=ROOT)
    arguments = parser.parse_args()
    errors = check(arguments.root.resolve())
    if errors:
        for error in errors:
            print(f"subsystems: {error}", file=sys.stderr)
        return 1
    print("subsystems: ownership and dependency record match the tree")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
