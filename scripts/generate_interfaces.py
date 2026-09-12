#!/usr/bin/env python3
"""Generate each subsystem's INTERFACE.md from its facade, and check it stays true.

An agent working in one subsystem should be able to read what another offers without opening
its source. INTERFACE.md lists every item the facade re-exports, with its signature and doc
summary and no bodies. It is generated, and CI fails when it drifts from the code.

Usage: generate_interfaces.py [--write] [--root PATH]
"""

from __future__ import annotations

import argparse
import re
import subprocess
import sys
import tomllib
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
MANIFEST = "subsystems.toml"
GENERATED_BY = "scripts/generate_interfaces.py"

# `pub use path::{A, B};` or `pub use path::Name;`, possibly spanning lines.
REEXPORT = re.compile(r"^pub(?:\(crate\))?\s+use\s+([^;]+);", re.M)
ITEM_KINDS = ("struct", "enum", "trait", "type", "fn", "const", "static", "union", "mod")
SECTIONS = (
    ("Types", ("struct", "enum", "union", "type")),
    ("Traits", ("trait",)),
    ("Functions", ("fn",)),
    ("Constants", ("const", "static")),
    ("Modules", ("mod",)),
)


def tracked_files(root: Path) -> list[str]:
    result = subprocess.run(
        ["git", "-C", str(root), "ls-files", "-z"], capture_output=True, check=True,
    )
    return sorted(path for path in result.stdout.decode().split("\0") if path)


def exported_names(facade: str) -> list[tuple[str, str]]:
    """(module path, item name) for every name the facade re-exports."""
    names: list[tuple[str, str]] = []
    for match in REEXPORT.finditer(facade):
        body = " ".join(match.group(1).split())
        if body.startswith("self::"):
            body = body[len("self::"):]
        head, _, group = body.partition("{")
        module = head.strip().rstrip(":").strip()
        if group:
            items = [item.strip() for item in group.rstrip("}").split(",")]
        else:
            module, _, last = body.rpartition("::")
            items = [last.strip()]
        for item in items:
            item = item.split(" as ")[0].strip()
            if item and item != "self":
                names.append((module.strip(":"), item))
    return names


def local_items(facade: str) -> list[tuple[str, str, str]]:
    """(kind, name, rendered) for public items defined in the facade file itself."""
    found = []
    for kind, name, rendered, doc in scan_items(facade):
        found.append((kind, name, render(rendered, doc)))
    return found


def scan_items(source: str) -> list[tuple[str, str, str, str]]:
    """(kind, name, signature, doc) for every `pub` item defined in one file."""
    items: list[tuple[str, str, str, str]] = []
    pattern = re.compile(
        r"^(?P<indent>[ \t]*)pub(?:\s*\([^)]*\))?\s+(?:async\s+|unsafe\s+|extern\s+\"[^\"]*\"\s+)*"
        r"(?P<kind>" + "|".join(ITEM_KINDS) + r")\s+(?P<name>[A-Za-z_][A-Za-z0-9_]*)",
        re.M,
    )
    for match in pattern.finditer(source):
        if match.group("indent"):  # nested in an impl or another item
            continue
        signature = signature_at(source, match.start())
        doc = doc_above(source, match.start())
        items.append((match.group("kind"), match.group("name"), signature, doc))
    return items


def without_comments(source: str) -> str:
    """Blank `//` comments, keeping offsets, so commas in prose cannot look like syntax."""
    return re.sub(r"//[^\n]*", lambda match: " " * len(match.group(0)), source)


def enum_variants(source: str, brace: int) -> str:
    """Variant names of an enum body starting at its opening brace; they are part of the contract."""
    source = without_comments(source)
    depth, index, variants, current = 0, brace, [], ""
    while index < len(source):
        char = source[index]
        if char in "{([<":
            depth += 1
            if depth == 1:
                index += 1
                continue
        elif char in "})]>":
            depth -= 1
            if depth == 0:
                break
        elif depth == 1 and char == ",":
            variants.append(current)
            current = ""
            index += 1
            continue
        if depth >= 1:
            current += char
        index += 1
    variants.append(current)
    names = []
    for variant in variants:
        match = re.search(r"([A-Za-z_][A-Za-z0-9_]*)\s*$", variant.split("(")[0].split("{")[0].strip())
        if match:
            names.append(match.group(1))
    return ", ".join(names)


def trait_methods(source: str, brace: int) -> str:
    """Method signatures inside a trait body; they are the contract for implementers."""
    source = without_comments(source)
    depth, index, end = 0, brace, brace
    while index < len(source):
        if source[index] == "{":
            depth += 1
        elif source[index] == "}":
            depth -= 1
            if depth == 0:
                end = index
                break
        index += 1
    body = source[brace + 1:end]
    methods = []
    for match in re.finditer(r"^[ \t]*fn\s+[A-Za-z_][A-Za-z0-9_]*", body, re.M):
        method = signature_at(body, match.start()).removesuffix(" { … }").rstrip(";")
        methods.append(f"    {method};")
    return "\n".join(methods)


def signature_at(source: str, start: int) -> str:
    """The item's signature: everything up to its body or terminating semicolon."""
    depth = 0
    index = start
    while index < len(source):
        char = source[index]
        if char in "([":
            depth += 1
        elif char in ")]":
            depth -= 1
        elif char == "<" and source[index - 1] != "-":
            depth += 1
        elif char == ">" and source[index - 1] not in "-=":
            depth -= 1
        elif depth <= 0 and char in "{;":
            body = " ".join(source[start:index].split()).replace("( ", "(").replace(" )", ")").replace(" ,", ",").replace(",)", ")")
            if char != "{":
                return f"{body};"
            kind = next((word for word in body.split() if word in ITEM_KINDS), "")
            if kind == "enum":
                variants = enum_variants(source, index)
                return f"{body} {{ {variants} }}" if variants else f"{body} {{ … }}"
            if kind == "trait":
                methods = trait_methods(source, index)
                return f"{body} {{\n{methods}\n}}" if methods else f"{body} {{ … }}"
            return f"{body} {{ … }}"
        index += 1
    return " ".join(source[start:index].split())


def doc_above(source: str, start: int) -> str:
    """The first sentence-ish line of the doc comment directly above an item."""
    lines = source[:start].splitlines()
    doc: list[str] = []
    for line in reversed(lines):
        stripped = line.strip()
        if stripped.startswith("///"):
            doc.append(stripped[3:].strip())
        elif stripped.startswith("#[") or not stripped:
            if doc:
                break
            continue
        else:
            break
    if not doc:
        return ""
    summary = " ".join(reversed(doc))
    sentence = re.split(r"(?<=[.!?])\s", summary)[0]
    return sentence if len(sentence) <= 160 else sentence[:157].rstrip() + "…"


def render(signature: str, doc: str) -> str:
    return f"/// {doc}\n{signature}" if doc else signature


def subsystem_sources(root: Path, files: list[str], record: dict) -> dict[str, str]:
    prefixes = tuple(record.get("paths", []))
    return {
        path: (root / path).read_text(errors="replace")
        for path in files
        if path.endswith(".rs") and path.startswith(prefixes)
    }


def interface_text(root: Path, files: list[str], manifest: dict, record: dict) -> str:
    facade_path = record["facade"]
    facade = (root / facade_path).read_text()
    sources = subsystem_sources(root, files, record)
    definitions: dict[str, list[tuple[str, str, str]]] = {}
    for path, text in sources.items():
        for kind, name, signature, doc in scan_items(text):
            definitions.setdefault(name, []).append((kind, signature, doc))

    rendered: list[tuple[str, str, str]] = []
    missing: list[str] = []
    for _, name in exported_names(facade):
        candidates = definitions.get(name)
        if not candidates:
            missing.append(name)
            continue
        kind, signature, doc = candidates[0]
        rendered.append((kind, name, render(signature, doc)))
    rendered.extend(local_items(facade))

    exported = {name for _, name in exported_names(facade)} | {name for _, name, _ in local_items(facade)}
    referenced = {
        word for _, _, text in rendered
        for line in text.splitlines() if not line.lstrip().startswith("///")
        # Only the part before a body: enum variant and trait method names are not type references.
        for word in re.findall(r"\b[A-Z][A-Za-z0-9_]*\b", line.split(" { ")[0])
    }
    unnameable = sorted((referenced & set(definitions)) - exported)

    depends = record.get("depends_on", []) or []
    debt = [entry.get("to") for entry in record.get("debt", [])]
    allowed = ", ".join(f"`{name}`" for name in sorted(depends)) or "nothing"
    debt_text = f" Debt: {', '.join(f'`{name}`' for name in sorted(debt))}." if debt else ""
    capsule = next(iter(record.get("instructions", [])), None)

    lines = [
        f"# {record['id']} — public interface",
        "",
        f"Generated from [`{facade_path}`]({Path(facade_path).name}) by `{GENERATED_BY}`; "
        "CI fails if it drifts. Edit the code, then regenerate.",
        "",
        f"- **Facade:** `{facade_path}`",
    ]
    if capsule:
        lines.append(f"- **Capsule:** [`{Path(capsule).name}`]({Path(capsule).name})")
    lines.append(f"- **May depend on:** {allowed}.{debt_text}")
    lines.append("")
    lines.append(
        "Everything below is what callers outside this subsystem can reach. Implementation "
        "modules are private; their contents are deliberately absent."
    )
    for title, kinds in SECTIONS:
        section = sorted((name, text) for kind, name, text in rendered if kind in kinds)
        if not section:
            continue
        lines.extend(["", f"## {title}", "", "```rust"])
        lines.extend(text for _, text in section)
        lines.append("```")
    if unnameable:
        lines.extend([
            "", "## Referenced but not exported", "",
            "These types appear in the signatures above but the facade does not export them, so a "
            "caller can hold a value and never name its type. Export them or change the signature: "
            + ", ".join(f"`{name}`" for name in unnameable),
        ])
    if missing:
        lines.extend(["", "## Unresolved", "",
                      "The generator could not find a definition for: " + ", ".join(f"`{n}`" for n in sorted(missing))])
    return "\n".join(lines) + "\n"


def interfaces(root: Path) -> list[tuple[Path, str]]:
    manifest = tomllib.loads((root / MANIFEST).read_text())
    files = tracked_files(root)
    generated = []
    for record in manifest.get("subsystem", []):
        if "facade" not in record or "interface" not in record:
            continue
        generated.append((root / record["interface"], interface_text(root, files, manifest, record)))
    return generated


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--write", action="store_true", help="update the files instead of checking")
    parser.add_argument("--root", type=Path, default=ROOT)
    arguments = parser.parse_args()
    root = arguments.root.resolve()
    stale: list[str] = []
    for path, text in interfaces(root):
        if arguments.write:
            path.write_text(text)
            continue
        current = path.read_text() if path.is_file() else ""
        if current != text:
            relative = path.relative_to(root)
            first = next(
                (f"{expected!r} != {actual!r}" for expected, actual in
                 zip(text.splitlines(), current.splitlines()) if expected != actual),
                "file length differs",
            )
            stale.append(f"{relative} is stale: {first}")
    if stale:
        for entry in stale:
            print(f"interfaces: {entry}", file=sys.stderr)
        print(
            f"interfaces: regenerate with `python3 {GENERATED_BY} --write` and commit the result",
            file=sys.stderr,
        )
        return 1
    print(f"interfaces: {len(interfaces(root))} subsystem interface(s) match their facade")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
