#!/usr/bin/env python3
"""Structural verifier for Finch typed stack IR version 6.

It checks an IR module against ``docs/language/semantics/ir.json`` without running it: every
instruction is a declared one with exactly its declared fields, every block ends in exactly one
terminator, every target, region, slot, and callee exists, and the operand-stack height at each
instruction is the same along every path, never underflows, is exactly one at ``return``, and
matches each region's recorded depth at its handler.

This is the control-flow and shape part of the verifier the specification requires.  It does not
check operand types, effect containment, or exception sets.
"""

from __future__ import annotations

from typing import Any


def _count(expression: Any, instruction: dict[str, Any]) -> int:
    if isinstance(expression, int):
        return expression
    return {
        "len(fields)": lambda: len(instruction["fields"]),
        "len(modes)": lambda: len(instruction["modes"]),
        "count": lambda: instruction["count"],
        "arity": lambda: instruction["arity"],
        "arity + 1": lambda: instruction["arity"] + 1,
    }[expression]()


def verify_ir(module: dict[str, Any], specification: dict[str, Any]) -> list[str]:
    errors: list[str] = []
    table = {entry["op"]: entry for entry in specification["instructions"]}
    if module.get("version") != specification["ir_version"]:
        return [f"IR module has version {module.get('version')!r}, expected {specification['ir_version']}"]
    functions = module.get("functions", {})
    if module.get("entry") not in functions:
        errors.append(f"IR entry {module.get('entry')!r} is not a function of the module")
    for name, function in functions.items():
        errors.extend(f"function {name}: {error}" for error in _verify_function(function, functions, table))
    return errors


def _verify_function(function: dict[str, Any], functions: dict[str, Any], table: dict[str, Any]) -> list[str]:
    errors: list[str] = []
    blocks = function["blocks"]
    regions = function["regions"]

    def block_exists(target: Any, where: str) -> bool:
        if not isinstance(target, int) or not 0 <= target < len(blocks):
            errors.append(f"{where} names missing block {target!r}")
            return False
        return True

    for index, region in enumerate(regions):
        if region["kind"] not in {"catch", "cleanup", "suppress"}:
            errors.append(f"region {index} has unknown kind {region['kind']!r}")
        block_exists(region["target"], f"region {index}")
        seen = {index}
        parent = region["parent"]
        while parent is not None:
            if not isinstance(parent, int) or not 0 <= parent < len(regions) or parent in seen:
                errors.append(f"region {index} has an invalid or cyclic parent chain")
                break
            seen.add(parent)
            parent = regions[parent]["parent"]
    if function["parameters"] > function["locals"]:
        errors.append("has more parameters than local slots")
    if not block_exists(function["entry"], "entry"):
        return errors

    for index, block in enumerate(blocks):
        where = f"block {index}"
        if block["id"] != index:
            errors.append(f"{where} has id {block['id']}")
        if block["region"] is not None and not (isinstance(block["region"], int) and 0 <= block["region"] < len(regions)):
            errors.append(f"{where} names missing region {block['region']!r}")
        instructions = block["instructions"]
        if not instructions:
            errors.append(f"{where} is empty")
            continue
        for position, instruction in enumerate(instructions):
            entry = table.get(instruction.get("op"))
            at = f"{where} instruction {position}"
            if entry is None:
                errors.append(f"{at} has unknown operation {instruction.get('op')!r}")
                continue
            if set(instruction) - {"op"} != set(entry["fields"]):
                errors.append(f"{at} {instruction['op']} has fields {sorted(set(instruction) - {'op'})}, expected {sorted(entry['fields'])}")
                continue
            terminator = bool(entry.get("terminator"))
            if terminator != (position == len(instructions) - 1):
                errors.append(f"{at} {instruction['op']} is {'a' if terminator else 'not a'} terminator in the wrong position")
            for key in ("target", "then", "else"):
                if key in instruction:
                    block_exists(instruction[key], at)
            if "index" in instruction and instruction["op"] != "capture_get" and not 0 <= instruction["index"] < function["locals"]:
                errors.append(f"{at} uses local slot {instruction['index']} of {function['locals']}")
            if instruction["op"] in {"call", "tail_call", "make_closure", "make_generator"}:
                callee = functions.get(instruction["function"])
                if callee is None:
                    errors.append(f"{at} names missing function {instruction['function']!r}")
                elif "arity" in instruction and callee and instruction["arity"] != callee["parameters"]:
                    errors.append(f"{at} passes {instruction['arity']} arguments to {instruction['function']}, which takes {callee['parameters']}")
            first = -1 if instruction["op"] == "tail_call_closure" else 0
            if "adopt" in instruction and any(not first <= item < instruction["arity"] for item in instruction["adopt"]):
                errors.append(f"{at} adopts an argument index outside its arity")
            if instruction["op"] == "capability_request" and instruction["static"]:
                arity = instruction["arity"]
                before = instructions[max(0, position - arity) : position]
                if not instruction["check"]:
                    errors.append(f"{at} is an await, which has no static arguments")
                elif len(before) != arity or any(item["op"] != "constant" for item in before):
                    errors.append(f"{at} claims static arguments that are not the constants immediately before it")
            for pair in instruction.get("forward", []):
                lowest = -1 if instruction["op"] == "tail_call_closure" else 0
                if not (isinstance(pair, list) and len(pair) == 2 and -1 <= pair[0] < function["parameters"] and lowest <= pair[1] < instruction["arity"]):
                    errors.append(f"{at} forwards {pair!r}, which is not a parameter of this function and an argument of the call")
    if errors:
        return errors

    heights: dict[int, int] = {}
    pending: list[tuple[int, int, str]] = [(function["entry"], 0, "entry")]
    for index, region in enumerate(regions):
        pending.append((region["target"], region["depth"] + (1 if region["kind"] == "catch" else 0), f"region {index}"))
    while pending:
        block, height, source = pending.pop()
        if block in heights:
            if heights[block] != height:
                errors.append(f"block {block} is entered at operand height {heights[block]} and at {height} (from {source})")
            continue
        heights[block] = height
        for position, instruction in enumerate(blocks[block]["instructions"]):
            entry = table[instruction["op"]]
            pops = _count(entry["pops"], instruction)
            at = f"block {block} instruction {position} {instruction['op']}"
            if height < pops:
                errors.append(f"{at} needs {pops} operands, found {height}")
                break
            height -= pops
            op = instruction["op"]
            if not entry.get("terminator"):
                height += _count(entry["pushes"], instruction)
            elif op == "jump":
                pending.append((instruction["target"], height, at))
            elif op == "branch":
                pending.append((instruction["then"], height, at))
                pending.append((instruction["else"], height, at))
            elif op == "branch_variant":
                pending.append((instruction["then"], height + instruction["count"], at))
                pending.append((instruction["else"], height + 1, at))
            elif op in {"return", "tail_call", "tail_call_closure"} and height != 0:
                errors.append(f"{at} leaves {height} extra operands in the frame")
    return errors
