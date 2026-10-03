#!/usr/bin/env python3
"""Capability manifest and admission: what a checked program may ask of its host, decided before it runs.

``docs/language/semantics/admission.json`` states the rules.  The manifest is derived twice, from
the resolved program and from its IR, and the two must agree: a host can recompute it from verified
IR and need not trust a frontend's account.
"""

from __future__ import annotations

from typing import Any

from reference_machine import shown


def request_text(operation: str, arguments: list[Any] | None) -> str:
    if arguments is None:
        return f"{operation}(*)"
    return f"{operation}({', '.join(shown(argument) for argument in arguments)})"


def _finish(effects: list[dict[str, Any]], awaits: set[str], events: set[str]) -> dict[str, Any]:
    unique = {request_text(entry["operation"], entry["arguments"]): entry for entry in effects}
    return {"effects": [unique[key] for key in sorted(unique)], "awaits": sorted(awaits), "events": sorted(events)}


def manifest_of_program(program: dict[str, Any]) -> dict[str, Any]:
    """Every host operation in code reachable from the entry, from the resolved program."""
    effects: list[dict[str, Any]] = []
    awaits: set[str] = set()
    events: set[str] = set()
    visited: set[str] = set()

    def reach(name: str) -> None:
        if name in program["definitions"] and name not in visited:
            visited.add(name)
            visit(program["definitions"][name]["body"])

    def visit(value: Any) -> None:
        if isinstance(value, list):
            for item in value:
                visit(item)
            return
        if not isinstance(value, dict):
            return
        form = value.get("form")
        if form == "effect":
            arguments = value["arguments"]
            literal = [argument["value"] for argument in arguments] if all(argument["form"] == "literal" for argument in arguments) else None
            effects.append({"operation": value["operation"], "arguments": literal})
        elif form == "await":
            awaits.add(value["operation"])
        elif form == "emit":
            events.add(value["operation"])
        elif form == "call" and isinstance(value["callee"], str):
            reach(value["callee"])
        elif form == "lambda" and "function" in value:
            reach(value["function"])
            return
        for key, item in value.items():
            if key not in {"static_type", "static_mode"}:
                visit(item)

    visit(program["body"])
    return _finish(effects, awaits, events)


def manifest_of_ir(module: dict[str, Any], operations: dict[str, Any]) -> dict[str, Any]:
    """The same manifest, read from IR: reachable functions and the requests they contain."""
    effects: list[dict[str, Any]] = []
    awaits: set[str] = set()
    events: set[str] = set()
    pending = [module["entry"]]
    visited: set[str] = set()
    while pending:
        name = pending.pop()
        if name in visited:
            continue
        visited.add(name)
        for block in module["functions"][name]["blocks"]:
            instructions = block["instructions"]
            for index, instruction in enumerate(instructions):
                op = instruction["op"]
                if op in {"call", "tail_call", "make_closure", "make_fiber"}:
                    pending.append(instruction["function"])
                elif op == "emit":
                    events.add(instruction["operation"])
                elif op == "capability_request":
                    if not instruction["check"]:
                        awaits.add(instruction["operation"])
                        continue
                    # The verifier has shown that a static request is preceded by its constants.
                    before = instructions[index - instruction["arity"] : index] if instruction["arity"] else []
                    effects.append({"operation": instruction["operation"], "arguments": [item["value"] for item in before] if instruction["static"] else None})
    del operations
    return _finish(effects, awaits, events)


def grant_allows(grants: dict[str, Any], operation: str, arguments: list[Any] | None) -> bool:
    """Whether a grant covers a request.

    ``arguments`` is None for a request whose arguments are not static; only an unrestricted grant
    covers it, because nothing bounds what it will ask for.
    """
    grant = grants.get(operation, False)
    if grant is True:
        return True
    if not grant:
        return False
    return arguments is not None and list(arguments) in grant["arguments"]


def admit(manifest: dict[str, Any], host: dict[str, Any]) -> dict[str, Any]:
    """Decide before execution whether every statically known request is granted, prompting where the host allows."""
    grants = dict(host.get("grants", {}))
    answers = host.get("admission", {}).get("prompt", {})
    log: list[str] = []
    refused: list[str] = []
    for entry in manifest["effects"]:
        operation, arguments = entry["operation"], entry["arguments"]
        if grant_allows(grants, operation, arguments):
            continue
        text = request_text(operation, arguments)
        answer = answers.get(operation)
        log.append(f"prompt:{text}:{answer or 'unanswered'}")
        if answer == "allow":
            current = grants.get(operation)
            if arguments is None or current is True:
                grants[operation] = True
            else:
                allowed = list(current["arguments"]) if isinstance(current, dict) else []
                grants[operation] = {"arguments": allowed + [list(arguments)]}
        else:
            refused.append(text)
    return {"grants": grants, "log": log, "refused": refused}
