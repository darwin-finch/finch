#!/usr/bin/env python3
"""Sessions: a sequence of submissions against one persistent set of declarations.

A session is what a host keeps between turns of a REPL.  Each submission is checked and run against
the declarations earlier submissions committed, and it commits its own declarations only when it
completes.  A function declared again in a later turn shadows the earlier one for later turns;
functions committed earlier keep the revision they were checked against, so no published
declaration ever changes meaning.

``link`` implements that by giving every committed function an internal name that includes its
revision and rewriting references accordingly.  The linked program is an ordinary program for the
resolver, static pass, rule machine, and IR.
"""

from __future__ import annotations

from typing import Any

from reference_machine import MachineError, pattern_binders, yields_itself


def revision_name(name: str, turn: int) -> str:
    return f"{name}@{turn}"


def link(ast: dict[str, Any], visible: dict[str, str], turn: int) -> tuple[list[dict[str, Any]], list[dict[str, Any]], dict[str, str]]:
    """Rewrite one submission's names to revisions.

    Returns the submission's declarations, its body items, and the names it would make visible.
    """
    items = ast["items"] if ast["form"] == "sequence" else [ast]
    declared = {item["name"]: revision_name(item["name"], turn) for item in items if item["form"] == "function"}
    names = {**visible, **declared}

    def rename(value: Any, local: frozenset[str]) -> Any:
        if isinstance(value, list):
            return [rename(item, local) for item in value]
        if not isinstance(value, dict) or "form" not in value:
            return value
        form = value["form"]
        if form == "call" and isinstance(value["callee"], str) and value["callee"] not in local and value["callee"] in names:
            return {**value, "callee": names[value["callee"]], "arguments": rename(value["arguments"], local)}
        if form == "read" and value["place"] not in local and value["place"] in names:
            return {**value, "place": names[value["place"]]}
        if form == "let":
            return {**value, "initializer": rename(value["initializer"], local), "body": rename(value["body"], local | {value["name"]})}
        if form in {"match-arm", "catch"}:
            return {**value, "body": rename(value["body"], local | pattern_binders(value["pattern"]))}
        if form == "lambda":
            inner = local | {parameter["name"] for parameter in value["parameters"]}
            return {**value, "body": rename(value["body"], inner)}
        if form == "fiber":
            return {**value, "body": rename(value["body"], local | {value["resume"]})}
        if form == "function":
            inner = frozenset(parameter["name"] for parameter in value["parameters"])
            return {**value, "name": declared[value["name"]], "body": rename(value["body"], inner)}
        return {key: rename(item, local) for key, item in value.items()}

    declarations = [rename(item, frozenset()) for item in items if item["form"] in {"function", "variant"}]
    body = [rename(item, frozenset()) for item in items if item["form"] not in {"function", "variant"}]
    return declarations, body, declared


class Session:
    def __init__(self) -> None:
        self.turn = 0
        self.declarations: list[dict[str, Any]] = []
        self.visible: dict[str, str] = {}
        self.variants: set[str] = set()

    def prepare(self, ast: dict[str, Any]) -> tuple[dict[str, Any], list[dict[str, Any]], dict[str, str]]:
        """The program for this turn: committed declarations, then this submission."""
        self.turn += 1
        declarations, body, declared = link(ast, self.visible, self.turn)
        for item in declarations:
            if item["form"] == "variant" and item["name"] in self.variants:
                raise MachineError(f"variant {item['name']} is already declared in this session", "F-DIAG-DUPLICATE-DECLARATION")
        if not body:
            body = [{"form": "literal", "value": None}]
        program = {"form": "sequence", "items": self.declarations + declarations + body}
        return program, declarations, declared

    def words(self) -> dict[str, tuple[int, int, str]]:
        """Stack effect and result type of every visible declaration, for the Co-Forth reader."""
        table: dict[str, tuple[int, int, str]] = {}
        internal = {item["name"]: item for item in self.declarations if item["form"] == "function"}
        for name, revision in self.visible.items():
            function = internal[revision]
            if yields_itself(function["body"]):
                table[name] = (len(function["parameters"]), 1, "generator")
                continue
            table[name] = (len(function["parameters"]), 0 if function["result"] == "unit" else 1, function["result"])
        for item in self.declarations:
            if item["form"] == "variant":
                for case in item["cases"]:
                    table[case["name"]] = (len(case["payload"]), 1, item["name"])
        return table

    def commit(self, declarations: list[dict[str, Any]], declared: dict[str, str]) -> None:
        self.declarations.extend(declarations)
        self.visible.update(declared)
        self.variants.update(item["name"] for item in declarations if item["form"] == "variant")
