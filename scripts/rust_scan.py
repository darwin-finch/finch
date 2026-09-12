#!/usr/bin/env python3
"""Read Rust source well enough to find its cross-module references, without a compiler.

Comments and string and char literals are blanked before anything is matched, so their text cannot
look like syntax; `#[cfg(test)]` modules are dropped so test-only imports are not mistaken for
production dependencies. This is a lint heuristic: it does not expand macros or follow `use`
re-exports, so a dependency hidden behind either escapes it.
"""

from __future__ import annotations

import re
import subprocess
from pathlib import Path


class ScanError(Exception):
    pass


CRATE_PATH = re.compile(r"\bcrate::((?:[a-z_][a-z0-9_]*)(?:::[a-z_][a-z0-9_]*)*)")
CRATE_GROUP = re.compile(r"\bcrate::\s*\{")
GROUP_ITEM = re.compile(r"\s*((?:[a-z_][a-z0-9_]*)(?:::[a-z_][a-z0-9_]*)*)")
RAW_STRING = re.compile(r'[bc]?r(#*)"')
CHAR_LITERAL = re.compile(r"'(?:\\(?:u\{[0-9a-fA-F]+\}|x[0-9a-fA-F]{2}|.)|[^\\'\n])'")
TEST_MODULE_BLOCK = re.compile(
    r"#\[cfg\(test\)\]\s*(?:#\[[^\]]*\]\s*)*(?:pub(?:\([^)]*\))?\s+)?mod\s+\w+\s*\{"
)
TEST_MODULE_FILE = re.compile(
    r"#\[cfg\(test\)\]\s*(?:#\[[^\]]*\]\s*)*(?:pub(?:\([^)]*\))?\s+)?mod\s+(\w+)\s*;"
)


def tracked_files(root: Path) -> list[str]:
    result = subprocess.run(
        ["git", "-C", str(root), "ls-files", "-z"], capture_output=True, check=False,
    )
    if result.returncode:
        raise ScanError(f"cannot list tracked files: {result.stderr.decode().strip()}")
    return sorted(path for path in result.stdout.decode().split("\0") if path)


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
    """(offset, module path) for `crate::a::b` paths and each item of `crate::{a::x, b}`.

    The whole lower-case chain is kept, not just its first segment, so a subsystem declared on a
    nested path such as `src/tools/mcp/` can be told apart from its parent.
    """
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
