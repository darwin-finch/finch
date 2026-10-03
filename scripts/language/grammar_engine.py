#!/usr/bin/env python3
"""Execute the normative Finch grammar artifacts.

This is the reference reader for notation ``Finch-PEG-1`` (``docs/language/grammar/notation.json``).
It interprets ``common.json`` plus one frontend grammar directly; no production is restated in
Python.  The reader is parser-directed: a terminal is matched only where a production asks for it,
alternatives are ordered, repetition is greedy, and every node keeps a half-open UTF-8 byte span.
"""

from __future__ import annotations

import json
import re
import unicodedata
from functools import lru_cache
from pathlib import Path
from typing import Any, Callable


LANG = Path(__file__).resolve().parents[2] / "docs/language"

# Unicode White_Space property (PropList.txt); Python's str.isspace() is a different set.
WHITE_SPACE = frozenset(
    [0x09, 0x0A, 0x0B, 0x0C, 0x0D, 0x20, 0x85, 0xA0, 0x1680, *range(0x2000, 0x200B), 0x2028, 0x2029, 0x202F, 0x205F, 0x3000]
)


class GrammarError(Exception):
    """The grammar artifact itself is defective (not the source being read)."""


class ReaderError(Exception):
    """The source is rejected by the reader."""

    def __init__(self, code: str, message: str, offset: int, expected: list[str] | None = None):
        super().__init__(f"{code} at byte {offset}: {message}")
        self.code = code
        self.message = message
        self.offset = offset
        self.expected = expected or []


def _class_body(predicate: Callable[[int], bool]) -> str:
    parts: list[str] = []
    start: int | None = None
    for code_point in range(0x110000):
        inside = not 0xD800 <= code_point <= 0xDFFF and predicate(code_point)
        if inside and start is None:
            start = code_point
        elif not inside and start is not None:
            parts.append(_range(start, code_point - 1))
            start = None
    if start is not None:
        parts.append(_range(start, 0x10FFFF))
    return "".join(parts)


def _range(low: int, high: int) -> str:
    def spell(code_point: int) -> str:
        return f"\\U{code_point:08x}"

    return spell(low) if low == high else f"{spell(low)}-{spell(high)}"


@lru_cache(maxsize=None)
def unicode_class(name: str) -> str:
    if name == "XID_Start":
        return _class_body(lambda c: c != 0x5F and chr(c).isidentifier())
    if name == "XID_Continue":
        return _class_body(lambda c: ("a" + chr(c)).isidentifier())
    if name == "White_Space":
        return _class_body(lambda c: c in WHITE_SPACE)
    raise GrammarError(f"unsupported Unicode property {name!r}")


def translate_pattern(pattern: str) -> str:
    """Translate the PCRE2 UTF+UCP subset used by the artifacts into Python ``re`` syntax."""
    out: list[str] = []
    index = 0
    in_class = False
    while index < len(pattern):
        if pattern.startswith("\\p{", index):
            close = pattern.index("}", index)
            body = unicode_class(pattern[index + 3 : close])
            out.append(body if in_class else f"[{body}]")
            index = close + 1
            continue
        character = pattern[index]
        if character == "\\":
            out.append(pattern[index : index + 2])
            index += 2
            continue
        if character == "[" and not in_class:
            in_class = True
        elif character == "]" and in_class:
            in_class = False
        out.append(character)
        index += 1
    return "".join(out)


_NOTATION_TOKEN = re.compile(r"\s*('[^']*'|\"[^\"]*\"|[()|?*+!]|[A-Za-z_][A-Za-z0-9_]*)")


def parse_notation(text: str) -> tuple:
    """Parse one Finch-PEG-1 alternative string into an expression tree."""
    tokens: list[str] = []
    position = 0
    while position < len(text):
        if text[position:].strip() == "":
            break
        match = _NOTATION_TOKEN.match(text, position)
        if match is None:
            raise GrammarError(f"cannot tokenize grammar expression {text!r} at {position}")
        tokens.append(match.group(1))
        position = match.end()
    cursor = 0

    def choice() -> tuple:
        nonlocal cursor
        options = [sequence()]
        while cursor < len(tokens) and tokens[cursor] == "|":
            cursor += 1
            options.append(sequence())
        return options[0] if len(options) == 1 else ("alt", options)

    def sequence() -> tuple:
        nonlocal cursor
        items: list[tuple] = []
        while cursor < len(tokens) and tokens[cursor] not in {")", "|"}:
            items.append(postfix())
        return items[0] if len(items) == 1 else ("seq", items)

    def postfix() -> tuple:
        nonlocal cursor
        token = tokens[cursor]
        cursor += 1
        if token == "!":
            return ("not", postfix())
        if token == "(":
            node = choice()
            if cursor >= len(tokens) or tokens[cursor] != ")":
                raise GrammarError(f"unbalanced group in grammar expression {text!r}")
            cursor += 1
        elif token[0] in "'\"":
            node = ("lit", token[1:-1])
        elif token in {")", "|", "?", "*", "+"}:
            raise GrammarError(f"misplaced {token!r} in grammar expression {text!r}")
        else:
            node = ("sym", token)
        while cursor < len(tokens) and tokens[cursor] in {"?", "*", "+"}:
            node = ({"?": "opt", "*": "star", "+": "plus"}[tokens[cursor]], node)
            cursor += 1
        return node

    result = choice()
    if cursor != len(tokens):
        raise GrammarError(f"trailing tokens in grammar expression {text!r}")
    return result


def scan_raw_string(source: str, position: int) -> int | None:
    if not source.startswith("r", position):
        return None
    cursor = position + 1
    hashes = 0
    while cursor < len(source) and source[cursor] == "#":
        hashes += 1
        cursor += 1
    if hashes > 255 or cursor >= len(source) or source[cursor] != '"':
        return None
    closing = '"' + "#" * hashes
    end = source.find(closing, cursor + 1)
    if end < 0:
        raise ReaderError("F-LEX-UNTERMINATED-STRING", "raw string has no matching close", position)
    return end + len(closing)


def _scan_escaped(source: str, cursor: int, closing: str, start: int) -> int:
    while cursor < len(source):
        if source.startswith(closing, cursor):
            return cursor + len(closing)
        if source[cursor] == "\\":
            match = re.compile(r"\\(?:[\\\"nrt0]|x[0-9A-Fa-f]{2}|u\{[0-9A-Fa-f]+\})").match(source, cursor)
            if match is None:
                raise ReaderError("F-LEX-INVALID-ESCAPE", "invalid string escape", cursor)
            if match.group(0).startswith("\\u"):
                scalar = int(match.group(0)[3:-1], 16)
                if scalar > 0x10FFFF or 0xD800 <= scalar <= 0xDFFF:
                    raise ReaderError("F-LEX-INVALID-ESCAPE", "escape does not denote a Unicode scalar", cursor)
            cursor = match.end()
            continue
        cursor += 1
    raise ReaderError("F-LEX-UNTERMINATED-STRING", "string has no closing delimiter", start)


def scan_triple_string(source: str, position: int, allow_prefix: bool) -> int | None:
    cursor = position + 1 if allow_prefix and source.startswith('s"""', position) else position
    if not source.startswith('"""', cursor):
        return None
    return _scan_escaped(source, cursor + 3, '"""', position)


def scan_coforth_string(source: str, position: int) -> int | None:
    cursor = position
    prefixed = source.startswith('s"', cursor)
    if prefixed:
        cursor += 1
    if not source.startswith('"', cursor) or source.startswith('"""', cursor):
        return None
    cursor += 1
    if prefixed and source.startswith(" ", cursor):
        cursor += 1
    return _scan_escaped(source, cursor, '"', position)


def scan_balanced(source: str, position: int, opening: str, closing: str) -> int | None:
    if not source.startswith(opening, position):
        return None
    depth = 0
    cursor = position
    while cursor < len(source):
        character = source[cursor]
        if character == '"':
            cursor = _scan_escaped(source, cursor + 1, '"', cursor)
            continue
        if character == opening:
            depth += 1
        elif character == closing:
            depth -= 1
            if depth == 0:
                return cursor + 1
        cursor += 1
    raise ReaderError("F-LEX-UNBALANCED-COMMENT", "parenthesized comment is not closed", position)


class Grammar:
    """One frontend grammar merged with the grammars it imports."""

    def __init__(self, syntax: str, directory: Path | None = None):
        directory = directory or LANG / "grammar"
        own = json.loads((directory / f"{syntax}.json").read_text())
        merged_tokens: dict[str, Any] = {}
        merged_productions: dict[str, Any] = {}
        reserved: set[str] = set()
        for imported in own.get("imports", []):
            parent = json.loads((directory / imported).read_text())
            merged_tokens.update(parent["tokens"])
            merged_productions.update(parent["productions"])
            reserved.update(parent["reserved_words"])
        merged_tokens.update(own["tokens"])
        merged_productions.update(own["productions"])
        reserved.update(own["reserved_words"])
        self.syntax = syntax
        self.raw = own
        self.tokens = merged_tokens
        self.reserved = frozenset(reserved)
        self.root_entrypoints = own.get("root_entrypoints", {})
        self.semantic_nodes = {name: rule.get("semantic_node") for name, rule in merged_productions.items()}
        self.productions = {
            name: [parse_notation(alternative) for alternative in rule["alternatives"]]
            for name, rule in merged_productions.items()
        }
        self.matchers: dict[str, Callable[[str, int], int | None]] = {}
        for name, token in merged_tokens.items():
            self.matchers[name] = self._matcher(name, token)
        self.skip_tokens = [name for name, token in merged_tokens.items() if token.get("skip")]
        self.delimiter = re.compile(translate_pattern(merged_tokens["delimiter"]["pattern"]))
        self.word_character = re.compile(f"[{unicode_class('XID_Continue')}]|(?:{translate_pattern(merged_tokens['word_extra']['pattern'])})")
        self.white_space = re.compile(f"[{unicode_class('White_Space')}]")
        literals: set[str] = set()

        def collect(node: tuple) -> None:
            if node[0] == "lit":
                literals.add(node[1])
            elif node[0] in {"seq", "alt"}:
                for child in node[1]:
                    collect(child)
            elif node[0] in {"opt", "star", "plus", "not"}:
                collect(node[1])

        for alternatives in self.productions.values():
            for alternative in alternatives:
                collect(alternative)
        self.literals = frozenset(literals)
        self._check_symbols()
        self._check_left_recursion()

    def _matcher(self, name: str, token: dict[str, Any]) -> Callable[[str, int], int | None]:
        scanner = token.get("scanner")
        if scanner == "raw-string":
            return scan_raw_string
        if scanner == "triple-string":
            allow_prefix = self.syntax == "coforth"
            return lambda source, position: scan_triple_string(source, position, allow_prefix)
        if scanner == "coforth-string":
            return scan_coforth_string
        if scanner == "balanced":
            return lambda source, position: scan_balanced(source, position, "(", ")")
        if scanner is not None:
            raise GrammarError(f"token {name} names unknown scanner {scanner!r}")
        compiled = re.compile(translate_pattern(token["pattern"]))

        def match(source: str, position: int) -> int | None:
            found = compiled.match(source, position)
            return None if found is None or found.end() == position else found.end()

        return match

    def _check_symbols(self) -> None:
        def visit(node: tuple, production: str) -> None:
            if node[0] == "sym":
                if node[1] not in self.productions and node[1] not in self.tokens and node[1] not in {"EOF", "EPSILON"}:
                    raise GrammarError(f"production {production} references unknown symbol {node[1]}")
            elif node[0] in {"seq", "alt"}:
                for child in node[1]:
                    visit(child, production)
            elif node[0] in {"opt", "star", "plus", "not"}:
                visit(node[1], production)

        for name, alternatives in self.productions.items():
            for alternative in alternatives:
                visit(alternative, name)

    def _check_left_recursion(self) -> None:
        nullable: dict[str, bool] = {name: False for name in self.productions}

        def node_nullable(node: tuple) -> bool:
            kind = node[0]
            if kind == "lit":
                return False
            if kind == "sym":
                if node[1] == "EPSILON" or node[1] == "EOF":
                    return True
                return nullable.get(node[1], False)
            if kind == "seq":
                return all(node_nullable(child) for child in node[1])
            if kind == "alt":
                return any(node_nullable(child) for child in node[1])
            return kind in {"opt", "star", "not"} or node_nullable(node[1])

        changed = True
        while changed:
            changed = False
            for name, alternatives in self.productions.items():
                value = any(node_nullable(alternative) for alternative in alternatives)
                if value != nullable[name]:
                    nullable[name] = value
                    changed = True

        def first_symbols(node: tuple) -> set[str]:
            kind = node[0]
            if kind == "lit":
                return set()
            if kind == "sym":
                return {node[1]} if node[1] in self.productions else set()
            if kind == "seq":
                found: set[str] = set()
                for child in node[1]:
                    found |= first_symbols(child)
                    if not node_nullable(child):
                        break
                return found
            if kind == "alt":
                found = set()
                for child in node[1]:
                    found |= first_symbols(child)
                return found
            return first_symbols(node[1])

        edges = {name: set().union(*(first_symbols(alt) for alt in alternatives)) for name, alternatives in self.productions.items()}
        for origin in edges:
            pending = list(edges[origin])
            seen: set[str] = set()
            while pending:
                current = pending.pop()
                if current == origin:
                    raise GrammarError(f"production {origin} is left-recursive")
                if current in seen:
                    continue
                seen.add(current)
                pending.extend(edges[current])

    def unreachable_productions(self, roots: list[str]) -> list[str]:
        seen: set[str] = set()
        pending = list(roots)

        def symbols(node: tuple) -> set[str]:
            if node[0] == "sym":
                return {node[1]}
            if node[0] == "lit":
                return set()
            if node[0] in {"seq", "alt"}:
                return set().union(*(symbols(child) for child in node[1])) if node[1] else set()
            return symbols(node[1])

        while pending:
            current = pending.pop()
            if current in seen or current not in self.productions:
                continue
            seen.add(current)
            for alternative in self.productions[current]:
                pending.extend(symbols(alternative))
        return sorted(set(self.productions) - seen)


class _Parse:
    def __init__(self, grammar: Grammar, source: str):
        self.grammar = grammar
        self.source = source
        self.memo: dict[tuple[str, int], Any] = {}
        self.active: set[tuple[str, int]] = set()
        self.farthest = 0
        self.expected: set[str] = set()
        self.used_alternatives: set[tuple[str, int]] = set()

    def fail(self, position: int, what: str) -> None:
        if position > self.farthest:
            self.farthest = position
            self.expected = {what}
        elif position == self.farthest:
            self.expected.add(what)

    def skip(self, position: int, before_literal: str | None) -> int:
        grammar = self.grammar
        while True:
            advanced = False
            for name in grammar.skip_tokens:
                if name == "parenthesized_comment" and before_literal == "(":
                    continue
                end = grammar.matchers[name](self.source, position)
                if end is not None and end > position:
                    position = end
                    advanced = True
            if not advanced:
                return position

    def at_boundary(self, end: int) -> bool:
        source = self.source
        if end >= len(source):
            return True
        if not self.grammar.word_character.match(source[end - 1]):
            return True
        following = source[end]
        if self.grammar.white_space.match(following) or self.grammar.delimiter.match(following):
            return True
        return any(
            (self.grammar.matchers[name](source, end) or end) > end
            for name in self.grammar.skip_tokens
            if name != "parenthesized_comment"
        )

    def literal(self, text: str, position: int) -> tuple[dict[str, Any], int] | None:
        start = self.skip(position, text)
        source = self.source
        if not source.startswith(text, start):
            self.fail(start, repr(text))
            return None
        end = start + len(text)
        if not text[0].isalpha():
            for other in self.grammar.literals:
                if len(other) > len(text) and other.startswith(text) and not other[0].isalpha() and source.startswith(other, start):
                    if self.at_boundary(start + len(other)):
                        self.fail(start, repr(text))
                        return None
        if not self.at_boundary(end):
            self.fail(start, repr(text))
            return None
        return {"token": "literal", "text": text, "start": start, "end": end}, end

    def token(self, name: str, position: int) -> tuple[dict[str, Any], int] | None:
        start = self.skip(position, None)
        end = self.grammar.matchers[name](self.source, start)
        if end is None or not self.at_boundary(end):
            self.fail(start, name)
            return None
        text = self.source[start:end]
        if name == "identifier":
            if text in self.grammar.reserved:
                self.fail(start, name)
                return None
            if unicodedata.normalize("NFC", text) != text:
                raise ReaderError("F-LEX-IDENTIFIER-NOT-NFC", f"identifier {text!r} is not NFC", start)
        return {"token": name, "text": text, "start": start, "end": end}, end

    def rule(self, name: str, position: int) -> tuple[dict[str, Any], int] | None:
        key = (name, position)
        if key in self.memo:
            return self.memo[key]
        if key in self.active:
            raise GrammarError(f"left recursion reached production {name}")
        self.active.add(key)
        result = None
        for index, alternative in enumerate(self.grammar.productions[name]):
            outcome = self.run(alternative, position)
            if outcome is not None:
                children, end = outcome
                start = children[0]["start"] if children else self.skip(position, None)
                node = {"rule": name, "alternative": index, "start": start, "end": max(end, start) if children else start, "children": children}
                if children:
                    node["end"] = children[-1]["end"]
                result = (node, end)
                break
        self.active.discard(key)
        self.memo[key] = result
        return result

    def run(self, node: tuple, position: int) -> tuple[list[dict[str, Any]], int] | None:
        kind = node[0]
        if kind == "lit":
            matched = self.literal(node[1], position)
            return None if matched is None else ([matched[0]], matched[1])
        if kind == "sym":
            name = node[1]
            if name == "EPSILON":
                return [], position
            if name == "EOF":
                end = self.skip(position, None)
                if end != len(self.source):
                    self.fail(end, "EOF")
                    return None
                return [], end
            matched = self.rule(name, position) if name in self.grammar.productions else self.token(name, position)
            return None if matched is None else ([matched[0]], matched[1])
        if kind == "seq":
            children: list[dict[str, Any]] = []
            cursor = position
            for child in node[1]:
                outcome = self.run(child, cursor)
                if outcome is None:
                    return None
                children.extend(outcome[0])
                cursor = outcome[1]
            return children, cursor
        if kind == "alt":
            for child in node[1]:
                outcome = self.run(child, position)
                if outcome is not None:
                    return outcome
            return None
        if kind == "opt":
            outcome = self.run(node[1], position)
            return ([], position) if outcome is None else outcome
        if kind == "not":
            farthest, expected = self.farthest, set(self.expected)
            outcome = self.run(node[1], position)
            self.farthest, self.expected = farthest, expected
            return ([], position) if outcome is None else None
        children = []
        cursor = position
        count = 0
        while True:
            outcome = self.run(node[1], cursor)
            if outcome is None or outcome[1] == cursor:
                break
            children.extend(outcome[0])
            cursor = outcome[1]
            count += 1
        if kind == "plus" and count == 0:
            return None
        return children, cursor


def byte_offsets(source: str) -> list[int]:
    offsets = [0]
    for character in source:
        offsets.append(offsets[-1] + len(character.encode("utf-8")))
    return offsets


def _rebase(node: dict[str, Any], offsets: list[int]) -> None:
    node["start"], node["end"] = offsets[node["start"]], offsets[node["end"]]
    for child in node.get("children", []):
        _rebase(child, offsets)


@lru_cache(maxsize=None)
def load_grammar(syntax: str) -> Grammar:
    return Grammar(syntax)


def parse(syntax: str, root: str, source: str | bytes, production: str | None = None) -> dict[str, Any]:
    """Read ``source`` with the named frontend, starting at the envelope-selected root.

    ``root`` is one of library, executable, submission, or script.  ``production`` overrides the
    entrypoint for corpus tests of a single production.  Returned spans are UTF-8 byte offsets.
    """
    if isinstance(source, bytes):
        try:
            source = source.decode("utf-8")
        except UnicodeDecodeError as error:
            raise ReaderError("F-LEX-INVALID-UTF8", "source is not valid UTF-8", error.start) from error
    grammar = load_grammar(syntax)
    if production is None:
        if root not in grammar.root_entrypoints:
            raise ReaderError("F-LEX-UNKNOWN-ROOT", f"unknown source root {root!r}", 0)
        production = grammar.root_entrypoints[root]
    assert production is not None
    state = _Parse(grammar, source)
    offsets = byte_offsets(source)
    outcome = state.rule(production, 0)
    if outcome is not None and state.skip(outcome[1], None) != len(source):
        state.fail(state.skip(outcome[1], None), "EOF")
        outcome = None
    if outcome is None:
        raise ReaderError(
            "F-LEX-SYNTAX",
            f"expected one of {sorted(state.expected)}",
            offsets[min(state.farthest, len(source))],
            sorted(state.expected),
        )
    tree = outcome[0]
    _rebase(tree, offsets)
    return tree


def rules_used(tree: dict[str, Any]) -> set[str]:
    found: set[str] = set()

    def visit(node: dict[str, Any]) -> None:
        if "rule" in node:
            found.add(node["rule"])
            for child in node["children"]:
                visit(child)

    visit(tree)
    return found
