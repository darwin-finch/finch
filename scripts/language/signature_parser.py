#!/usr/bin/env python3
"""Structural parser and kind checker for canonical required-prelude signatures."""

from __future__ import annotations

import json
import re
from typing import Any


NAME = re.compile(r"^[A-Za-z_][A-Za-z0-9_?!-]*$")
APPLICATION = re.compile(r"([A-Za-z_][A-Za-z0-9_?!-]*)\s*<")


def split_top_level(text: str, separator: str) -> list[str]:
    parts: list[str] = []
    start = 0
    stack: list[str] = []
    quote = False
    escaped = False
    pairs = {">": "<", ")": "(", "]": "[", "}": "{"}
    for index, character in enumerate(text):
        if quote:
            if escaped:
                escaped = False
            elif character == "\\":
                escaped = True
            elif character == '"':
                quote = False
            continue
        if character == '"':
            quote = True
        elif character in "<([{":
            stack.append(character)
        elif character == ">" and index > 0 and text[index - 1] == "-":
            continue
        elif character in ">)]}":
            if not stack or stack.pop() != pairs[character]:
                raise ValueError(f"unbalanced delimiter {character!r}")
        elif character == separator and not stack:
            parts.append(text[start:index].strip())
            start = index + 1
    if quote or stack:
        raise ValueError("unterminated quote or delimiter")
    parts.append(text[start:].strip())
    return parts


def matching_angle(text: str, opening: int) -> int:
    depth = 0
    quote = False
    escaped = False
    for index in range(opening, len(text)):
        character = text[index]
        if quote:
            if escaped:
                escaped = False
            elif character == "\\":
                escaped = True
            elif character == '"':
                quote = False
            continue
        if character == '"':
            quote = True
        elif character == "<":
            depth += 1
        elif character == ">":
            depth -= 1
            if depth == 0:
                return index
    raise ValueError("unterminated generic argument list")


def parse_kind_signature(kind: str) -> tuple[list[str], str]:
    # Arrows are the only separators in kind text; their operands contain no arrows in brackets.
    chunks: list[str] = []
    start = 0
    depth = 0
    index = 0
    while index < len(kind):
        if kind[index] == "<":
            depth += 1
        elif kind[index] == ">":
            depth -= 1
        elif kind.startswith("->", index) and depth == 0:
            chunks.append(kind[start:index].strip())
            index += 2
            start = index
            continue
        index += 1
    chunks.append(kind[start:].strip())
    if not all(chunks):
        raise ValueError(f"invalid kind signature {kind!r}")
    return chunks[:-1], chunks[-1]


def generic_environment(header: str) -> dict[str, str]:
    environment: dict[str, str] = {}
    if not header.strip():
        return environment
    for entry in split_top_level(header, ","):
        if entry.startswith("effect "):
            name, kind = entry[7:].strip(), "EffectRow"
        elif entry.startswith("exceptions "):
            name, kind = entry[11:].strip(), "ExceptionSet"
        elif entry.startswith("region "):
            name, kind = entry[7:].strip(), "Region"
        elif entry.startswith("stack "):
            name, kind = entry[6:].strip(), "StackRow"
        elif entry.startswith("value "):
            match = re.fullmatch(r"value\s+([A-Za-z_][A-Za-z0-9_?!-]*)\s*:\s*(.+)", entry)
            if not match:
                raise ValueError(f"invalid value generic entry {entry!r}")
            name, kind = match.group(1), f"Const<{match.group(2).strip()}>"
        elif entry.startswith("infer "):
            match = re.fullmatch(r"infer\s+([A-Za-z_][A-Za-z0-9_?!-]*)\s*:\s*(.+)", entry)
            if not match:
                raise ValueError(f"invalid inferred generic entry {entry!r}")
            name, kind = match.group(1), match.group(2).strip()
        else:
            name = entry.split(":", 1)[0].split("where", 1)[0].strip()
            kind = "Type"
        if not NAME.fullmatch(name):
            raise ValueError(f"invalid generic name {name!r}")
        if name in environment:
            raise ValueError(f"duplicate generic name {name}")
        environment[name] = kind
    return environment


def leading_header(signature: str) -> tuple[dict[str, str], str]:
    text = signature.strip()
    if not text.startswith("<"):
        return {}, text
    end = matching_angle(text, 0)
    return generic_environment(text[1:end]), text[end + 1 :].strip()


def find_top_level(text: str, marker: str) -> int:
    stack: list[str] = []
    quote = False
    escaped = False
    pairs = {">": "<", ")": "(", "]": "[", "}": "{"}
    index = 0
    while index < len(text):
        character = text[index]
        if quote:
            if escaped:
                escaped = False
            elif character == "\\":
                escaped = True
            elif character == '"':
                quote = False
            index += 1
            continue
        if character == '"':
            quote = True
        elif character in "<([{":
            stack.append(character)
        elif character == ">" and index > 0 and text[index - 1] == "-":
            pass
        elif character in ">)]}":
            if not stack or stack.pop() != pairs[character]:
                raise ValueError(f"unbalanced delimiter {character!r}")
        if not stack and index <= len(text) - len(marker) and text.startswith(marker, index):
            return index
        index += 1
    if quote or stack:
        raise ValueError("unterminated quote or delimiter")
    return -1


def find_binding_colon(text: str) -> int:
    start = 0
    while start < len(text):
        found = find_top_level(text[start:], ":")
        if found < 0:
            return -1
        index = start + found
        if (index == 0 or text[index - 1] != ":") and (index + 1 == len(text) or text[index + 1] != ":"):
            return index
        start = index + 2
    return -1


def generic_parameters_ast(header: str) -> tuple[list[dict[str, Any]], dict[str, tuple[int, str]]]:
    parameters: list[dict[str, Any]] = []
    environment: dict[str, tuple[int, str]] = {}
    if not header.strip():
        return parameters, environment
    for ordinal, entry in enumerate(split_top_level(header, ",")):
        raw = entry.strip()
        bounds: list[str] = []
        value_type: str | None = None
        if raw.startswith("effect "):
            name, kind = raw[7:].strip(), "EffectRow"
        elif raw.startswith("exceptions "):
            name, kind = raw[11:].strip(), "ExceptionSet"
        elif raw.startswith("region "):
            name, kind = raw[7:].strip(), "Region"
        elif raw.startswith("stack "):
            name, kind = raw[6:].strip(), "StackRow"
        elif raw.startswith("value "):
            match = re.fullmatch(r"value\s+([A-Za-z_][A-Za-z0-9_?!-]*)\s*:\s*(.+)", raw)
            if not match:
                raise ValueError(f"invalid value generic entry {raw!r}")
            name, value_type = match.groups()
            kind = "Const"
        elif raw.startswith("infer "):
            match = re.fullmatch(r"infer\s+([A-Za-z_][A-Za-z0-9_?!-]*)\s*:\s*(.+)", raw)
            if not match:
                raise ValueError(f"invalid inferred generic entry {raw!r}")
            name, kind = match.groups()
        else:
            where = raw.split(" where ", 1)
            declaration = where[0]
            if len(where) == 2:
                bounds.append(where[1].strip())
            colon = find_binding_colon(declaration)
            if colon >= 0:
                name = declaration[:colon].strip()
                bounds.extend(part.strip() for part in split_top_level(declaration[colon + 1 :], "+"))
            else:
                name = declaration.strip()
            kind = "Type"
        if not NAME.fullmatch(name) or name in environment:
            raise ValueError(f"invalid or duplicate generic name {name!r}")
        environment[name] = (ordinal, kind)
        node: dict[str, Any] = {"ordinal": ordinal, "name": name, "kind": kind, "bounds": bounds}
        if value_type is not None:
            node["value_type"] = value_type.strip()
        parameters.append(node)
    return parameters, environment


def parse_type_ast(text: str, environment: dict[str, tuple[int, str]], known: set[str], associated: set[str] | None = None) -> dict[str, Any]:
    value = text.strip()
    qualifiers: list[str] = []
    for qualifier in ("scoped", "stable"):
        if value.startswith(qualifier + " "):
            qualifiers.append(qualifier)
            value = value[len(qualifier) + 1 :].strip()
    if value.startswith("&mut "):
        return {"kind": "borrow", "mutable": True, "qualifiers": qualifiers, "target": parse_type_ast(value[5:], environment, known, associated)}
    if value.startswith("&"):
        return {"kind": "borrow", "mutable": False, "qualifiers": qualifiers, "target": parse_type_ast(value[1:], environment, known, associated)}
    if qualifiers:
        return {"kind": "qualified", "qualifiers": qualifiers, "target": parse_type_ast(value, environment, known, associated)}
    projection = find_top_level(value, "::")
    if projection >= 0:
        associated_name = value[projection + 2 :].strip()
        if not NAME.fullmatch(associated_name):
            raise ValueError(f"invalid associated projection {value!r}")
        return {"kind": "projection", "base": parse_type_ast(value[:projection], environment, known, associated), "associated": associated_name}
    match = re.match(r"([A-Za-z_][A-Za-z0-9_?!-]*)\s*<", value)
    if match:
        opening = value.index("<", match.start())
        closing = matching_angle(value, opening)
        if closing != len(value) - 1:
            raise ValueError(f"trailing tokens after type application {value!r}")
        name = match.group(1)
        if name not in known and name not in {"throws", "tuple", "Const"}:
            raise ValueError(f"unknown generic constructor {name}")
        arguments = [] if not value[opening + 1 : closing].strip() else [
            parse_type_ast(argument, environment, known, associated)
            for argument in split_top_level(value[opening + 1 : closing], ",")
        ]
        return {"kind": "application", "constructor": name, "arguments": arguments}
    refined = re.fullmatch(r'([A-Za-z_][A-Za-z0-9_.?!-]*):"((?:[^"\\]|\\.)*)"', value)
    if refined:
        return {"kind": "refined-constant", "root": refined.group(1), "pattern": refined.group(2)}
    if re.fullmatch(r"-?[0-9]+", value):
        return {"kind": "integer-constant", "value": value}
    if value in {"non-suspending", "suspends"}:
        return {"kind": "suspension-constant", "value": value}
    if value in environment:
        ordinal, kind = environment[value]
        return {"kind": "generic", "ordinal": ordinal, "parameter_kind": kind}
    if associated and value in associated:
        return {"kind": "associated", "name": value}
    if not NAME.fullmatch(value):
        raise ValueError(f"invalid type expression {value!r}")
    if value not in known:
        raise ValueError(f"unknown required-prelude type {value}")
    return {"kind": "named", "identity": value}


def type_contains_loan(node: dict[str, Any]) -> bool:
    kind = node["kind"]
    if kind in {"borrow", "qualified"}:
        return True
    if kind == "application":
        return any(type_contains_loan(argument) for argument in node["arguments"])
    if kind == "projection":
        return type_contains_loan(node["base"])
    return False


def application_payload(item: str, name: str) -> str | None:
    prefix = name + "<"
    if not item.startswith(prefix):
        return None
    opening = len(name)
    closing = matching_angle(item, opening)
    if closing != len(item) - 1:
        raise ValueError(f"trailing tokens after {name} contract item")
    return item[opening + 1 : closing].strip()


def contract_generic(name: str, expected_kind: str, environment: dict[str, tuple[int, str]]) -> dict[str, Any]:
    if name not in environment:
        raise ValueError(f"unknown contract row parameter {name!r}")
    ordinal, actual_kind = environment[name]
    if actual_kind != expected_kind:
        raise ValueError(f"contract row parameter {name!r} has kind {actual_kind}, expected {expected_kind}")
    return {"kind": "generic", "ordinal": ordinal}


def state_target_ast(target: str, parameter_names: dict[str, int], environment: dict[str, tuple[int, str]]) -> dict[str, Any]:
    target = target.strip()
    if target == "result":
        return {"kind": "result"}
    argument = re.fullmatch(r"arg\(([A-Za-z_][A-Za-z0-9_?!-]*)\)", target)
    if argument:
        name = argument.group(1)
        if name not in parameter_names:
            raise ValueError(f"unknown state parameter {name!r}")
        return {"kind": "parameter", "ordinal": parameter_names[name]}
    if target not in environment:
        raise ValueError(f"unknown state region {target!r}")
    ordinal, kind = environment[target]
    if kind != "Region":
        raise ValueError(f"state target {target!r} has kind {kind}, expected Region")
    return {"kind": "region", "ordinal": ordinal}


def selector_ast(text: str, parameter_names: dict[str, int]) -> dict[str, Any]:
    value = text.strip()
    if value.startswith('"'):
        try:
            decoded = json.loads(value)
        except json.JSONDecodeError as error:
            raise ValueError(f"invalid selector string {value!r}") from error
        if not isinstance(decoded, str):
            raise ValueError(f"selector literal is not a string: {value!r}")
        return {"kind": "literal", "value": decoded}
    call = re.fullmatch(r"(root|arg|join|narrow)\((.*)\)", value, re.DOTALL)
    if not call:
        raise ValueError(f"invalid selector expression {value!r}")
    constructor, payload = call.groups()
    if constructor in {"root", "arg"}:
        if not NAME.fullmatch(payload.strip()):
            raise ValueError(f"invalid {constructor} selector name {payload!r}")
        name = payload.strip()
        if constructor == "arg":
            if name not in parameter_names:
                raise ValueError(f"unknown selector parameter {name!r}")
            return {"kind": "argument", "ordinal": parameter_names[name]}
        return {"kind": "root", "name": name}
    arguments = split_top_level(payload, ",")
    if len(arguments) != 2:
        raise ValueError(f"{constructor} selector requires exactly two arguments")
    if constructor == "join":
        return {
            "kind": "join",
            "base": selector_ast(arguments[0], parameter_names),
            "relative": selector_ast(arguments[1], parameter_names),
        }
    pattern = selector_ast(arguments[1], parameter_names)
    if pattern["kind"] != "literal":
        raise ValueError("narrow selector pattern must be a string literal")
    return {"kind": "narrow", "base": selector_ast(arguments[0], parameter_names), "pattern": pattern["value"]}


def parse_contract_ast(
    text: str,
    parameter_names: dict[str, int],
    environment: dict[str, tuple[int, str]],
    known: set[str],
    associated: set[str] | None = None,
) -> dict[str, Any]:
    items = split_top_level(text.strip(), "|")
    contract: dict[str, Any] = {"effects": None, "exceptions": None, "suspension": None, "return_loan_origin": None, "comptime": False}
    for raw in items:
        item = raw.strip()
        if item == "plain":
            supplied = {
                "effects": {"kind": "row", "labels": [], "tail": None},
                "exceptions": {"kind": "closed", "types": []},
                "suspension": {"kind": "closed", "value": "non-suspending"},
            }
            for key, value in supplied.items():
                if contract[key] is not None:
                    raise ValueError(f"plain duplicates contract axis {key}")
                contract[key] = value
        elif item == "inferred":
            for key in ("effects", "exceptions", "suspension"):
                if contract[key] is not None:
                    raise ValueError(f"inferred duplicates contract axis {key}")
                contract[key] = {"kind": "inferred"}
        elif item == "pure":
            if contract["effects"] is not None:
                raise ValueError("duplicate effect axis")
            contract["effects"] = {"kind": "pure"}
        elif item == "effects-infer":
            if contract["effects"] is not None:
                raise ValueError("duplicate effect axis")
            contract["effects"] = {"kind": "inferred"}
        elif (payload := application_payload(item, "effects")) is not None:
            if contract["effects"] is not None:
                raise ValueError("duplicate effect axis")
            tail = None if not payload else contract_generic(payload, "EffectRow", environment)
            contract["effects"] = {"kind": "row", "labels": [], "tail": tail}
        elif (payload := application_payload(item, "state")) is not None or (payload := application_payload(item, "state-read")) is not None:
            access = "read" if item.startswith("state-read<") else "write"
            if contract["effects"] is None:
                contract["effects"] = {"kind": "row", "labels": [], "tail": None}
            if contract["effects"]["kind"] != "row":
                raise ValueError("state label conflicts with effect predicate")
            contract["effects"]["labels"].append({"kind": "state", "access": access, "target": state_target_ast(payload, parameter_names, environment)})
        elif item == "nothrow":
            if contract["exceptions"] is not None:
                raise ValueError("duplicate exception axis")
            contract["exceptions"] = {"kind": "closed", "types": []}
        elif item == "throws-infer":
            if contract["exceptions"] is not None:
                raise ValueError("duplicate exception axis")
            contract["exceptions"] = {"kind": "inferred"}
        elif (payload := application_payload(item, "throws")) is not None:
            if contract["exceptions"] is not None:
                raise ValueError("duplicate exception axis")
            entries = [] if not payload else split_top_level(payload, ",")
            if len(entries) == 1 and entries[0] in environment and environment[entries[0]][1] == "ExceptionSet":
                contract["exceptions"] = contract_generic(entries[0], "ExceptionSet", environment)
            else:
                contract["exceptions"] = {
                    "kind": "closed",
                    "types": [parse_type_ast(entry, environment, known, associated) for entry in entries],
                }
        elif item in {"non-suspending", "suspends", "suspends-infer"}:
            if contract["suspension"] is not None:
                raise ValueError("duplicate suspension axis")
            contract["suspension"] = {"kind": "inferred"} if item == "suspends-infer" else {"kind": "closed", "value": item}
        elif item.startswith("returns-loan<arg(") and item.endswith(")>"):
            name = item[len("returns-loan<arg(") : -2]
            if name not in parameter_names or contract["return_loan_origin"] is not None:
                raise ValueError(f"invalid or duplicate return-loan origin {name!r}")
            contract["return_loan_origin"] = {"kind": "parameter", "ordinal": parameter_names[name]}
        elif item == "comptime":
            contract["comptime"] = True
            if contract["effects"] is not None:
                raise ValueError("comptime duplicates effect axis")
            contract["effects"] = {"kind": "comptime"}
        elif re.fullmatch(r"[A-Za-z_][A-Za-z0-9_.?!-]*\{.*\}", item):
            request, bindings = item.split("{", 1)
            entries = [] if bindings == "}" else split_top_level(bindings[:-1], ",")
            selectors = []
            selector_names: set[str] = set()
            for entry in entries:
                binding = split_top_level(entry, "=")
                if len(binding) != 2 or not NAME.fullmatch(binding[0]) or binding[0] in selector_names:
                    raise ValueError(f"invalid capability selector binding {entry!r}")
                selector_names.add(binding[0])
                selectors.append({"name": binding[0], "expression": selector_ast(binding[1], parameter_names)})
            if contract["effects"] is None:
                contract["effects"] = {"kind": "row", "labels": [], "tail": None}
            if contract["effects"]["kind"] != "row":
                raise ValueError("capability request conflicts with effect predicate")
            contract["effects"]["labels"].append({"kind": "capability", "request": request, "selectors": selectors})
        else:
            raise ValueError(f"unknown contract item {item!r}")
    if contract["effects"] is None or contract["exceptions"] is None or contract["suspension"] is None:
        raise ValueError(f"contract does not close all axes: {text!r}")
    return contract


def parse_callable_signature(signature: str, constructors: dict[str, tuple[list[str], str]], associated: set[str] | None = None) -> dict[str, Any]:
    text = signature.strip()
    header = ""
    if text.startswith("<"):
        end = matching_angle(text, 0)
        header, text = text[1:end], text[end + 1 :].strip()
    generics, environment = generic_parameters_ast(header)
    known = set(constructors)
    if not text.startswith("("):
        raise ValueError("callable signature must begin with a parameter list")
    close = 0
    depth = 0
    for index, character in enumerate(text):
        if character == "(":
            depth += 1
        elif character == ")":
            depth -= 1
            if depth == 0:
                close = index
                break
    if not close or not text[close + 1 :].lstrip().startswith("->"):
        raise ValueError("callable signature lacks result arrow")
    parameters_text = text[1:close]
    remainder = text[close + 1 :].lstrip()[2:].strip()
    contract_at = find_top_level(remainder, " ! ")
    if contract_at < 0:
        raise ValueError("callable signature lacks contract")
    result_and_where, contract_text = remainder[:contract_at].strip(), remainder[contract_at + 3 :].strip()
    where_at = find_top_level(result_and_where, " where ")
    result_text = result_and_where if where_at < 0 else result_and_where[:where_at]
    evidence_text = "" if where_at < 0 else result_and_where[where_at + 7 :]
    parameters: list[dict[str, Any]] = []
    parameter_names: dict[str, int] = {}
    raw_parameters = [] if not parameters_text.strip() else split_top_level(parameters_text, ",")
    for ordinal, raw_parameter in enumerate(raw_parameters):
        raw = raw_parameter.strip()
        colon = find_binding_colon(raw)
        name = None
        if colon >= 0:
            name, raw = raw[:colon].strip(), raw[colon + 1 :].strip()
            if not NAME.fullmatch(name) or name in parameter_names:
                raise ValueError(f"invalid or duplicate parameter name {name!r}")
            parameter_names[name] = ordinal
        ownership = "value"
        for prefix, mode in (("steal ", "steal"), ("borrow-mut ", "borrow-mut"), ("borrow ", "borrow"), ("copy ", "copy")):
            if raw.startswith(prefix):
                ownership, raw = mode, raw[len(prefix) :].strip()
                break
        if raw.startswith("&mut "):
            ownership = "borrow-mut"
        elif raw.startswith("&"):
            ownership = "borrow"
        parameters.append({"ordinal": ordinal, "name": name, "ownership": ownership, "type": parse_type_ast(raw, environment, known, associated)})
    evidence = [] if not evidence_text else [parse_type_ast(item, environment, known, associated) for item in split_top_level(evidence_text, "+")]
    for generic in generics:
        generic["bounds"] = [parse_type_ast(bound, environment, known, associated) for bound in generic["bounds"]]
        if "value_type" in generic:
            generic["value_type"] = parse_type_ast(generic["value_type"], environment, known, associated)
    result = parse_type_ast(result_text, environment, known, associated)
    contract = parse_contract_ast(contract_text, parameter_names, environment, known, associated)
    result_contains_loan = type_contains_loan(result)
    if result_contains_loan != (contract["return_loan_origin"] is not None):
        raise ValueError("loan-bearing result and returns-loan origin must occur together")
    if contract["return_loan_origin"] is not None:
        origin = contract["return_loan_origin"]["ordinal"]
        if parameters[origin]["ownership"] not in {"borrow", "borrow-mut"}:
            raise ValueError("return-loan origin must identify a borrowed parameter")
    return {
        "generic_parameters": generics,
        "parameters": parameters,
        "result": result,
        "evidence": evidence,
        "contract": contract,
    }


def parse_concept_signature(signature: str, constructors: dict[str, tuple[list[str], str]]) -> dict[str, Any]:
    match = re.fullmatch(r"(?s)(.*?)\bconcept\s+([A-Za-z_][A-Za-z0-9_?!-]*)\s*<(.*)", signature.strip())
    if not match:
        raise ValueError("concept declaration lacks name and generic header")
    prefix, name, tail = match.groups()
    modifiers = [word for word in prefix.split() if word]
    allowed_modifiers = {"intrinsic", "closed", "marker", "unsafe", "trusted-law"}
    if set(modifiers) - allowed_modifiers or len(modifiers) != len(set(modifiers)):
        raise ValueError(f"invalid concept modifiers {modifiers!r}")
    synthetic = "<" + tail
    close = matching_angle(synthetic, 0)
    header = synthetic[1:close]
    generics, environment = generic_parameters_ast(header)
    remainder = synthetic[close + 1 :].strip()
    body = ""
    body_start = remainder.find("{")
    if body_start >= 0:
        if not remainder.endswith("}"):
            raise ValueError("concept body is not closed")
        body = remainder[body_start + 1 : -1].strip()
        inheritance = remainder[:body_start].strip()
    else:
        inheritance = remainder
    if inheritance.startswith(":"):
        inheritance = inheritance[1:].strip()
    elif inheritance:
        raise ValueError(f"unexpected concept header suffix {inheritance!r}")
    known = set(constructors)
    for generic in generics:
        generic["bounds"] = [parse_type_ast(bound, environment, known) for bound in generic["bounds"]]
        if "value_type" in generic:
            generic["value_type"] = parse_type_ast(generic["value_type"], environment, known)
    bases = [] if not inheritance else [parse_type_ast(item, environment, known) for item in split_top_level(inheritance, "+")]
    members = [] if not body else split_top_level(body, ";")
    associated: set[str] = set()
    for member in members:
        item = member.strip()
        if item.startswith("associated "):
            declaration = item[len("associated ") :].strip()
            colon = find_binding_colon(declaration)
            associated_name = declaration if colon < 0 else declaration[:colon].strip()
            if not NAME.fullmatch(associated_name) or associated_name in associated:
                raise ValueError(f"invalid or duplicate associated name {associated_name!r}")
            associated.add(associated_name)
    requirements: list[dict[str, Any]] = []
    for member in members:
        item = member.strip()
        if not item:
            raise ValueError("empty concept member")
        if item.startswith("associated "):
            declaration = item[len("associated ") :].strip()
            colon = find_binding_colon(declaration)
            associated_name = declaration if colon < 0 else declaration[:colon].strip()
            bound = None if colon < 0 else parse_type_ast(declaration[colon + 1 :], environment, known, associated)
            requirements.append({"kind": "associated", "name": associated_name, "bound": bound})
            continue
        opening = item.find("(")
        if opening <= 0:
            raise ValueError(f"invalid concept member {item!r}")
        operation_name = item[:opening].strip()
        if not NAME.fullmatch(operation_name):
            raise ValueError(f"invalid concept operation name {operation_name!r}")
        callable_text = item[opening:]
        if header:
            callable_text = f"<{header}>{callable_text}"
        requirements.append({"kind": "operation", "name": operation_name, "signature": parse_callable_signature(callable_text, constructors, associated)})
    return {
        "name": name, "modifiers": modifiers, "generic_parameters": generics,
        "bases": bases, "requirements": requirements,
    }


def validate_trusted_laws(concept: dict[str, Any], signature_ast: dict[str, Any]) -> None:
    laws = concept.get("trusted_laws", [])
    if laws and "trusted-law" not in signature_ast["modifiers"]:
        raise ValueError("trusted laws require the trusted-law concept modifier")
    generic_count = len(signature_ast["generic_parameters"])
    names: set[str] = set()
    for law in laws:
        name = law.get("name")
        if not isinstance(name, str) or not NAME.fullmatch(name) or name in names:
            raise ValueError(f"invalid or duplicate trusted-law name {name!r}")
        names.add(name)
        parameters = law.get("parameters")
        if not isinstance(parameters, list):
            raise ValueError(f"trusted law {name} lacks parameters")
        parameter_names: set[str] = set()
        for ordinal, parameter in enumerate(parameters):
            if parameter.get("ordinal") != ordinal or not NAME.fullmatch(parameter.get("name", "")) or parameter["name"] in parameter_names:
                raise ValueError(f"trusted law {name} has a malformed parameter at ordinal {ordinal}")
            parameter_names.add(parameter["name"])
            parameter_type = parameter.get("type")
            if not isinstance(parameter_type, dict) or parameter_type.get("kind") != "generic" or not isinstance(parameter_type.get("ordinal"), int) or not 0 <= parameter_type["ordinal"] < generic_count:
                raise ValueError(f"trusted law {name} parameter {ordinal} has an invalid type reference")

        def visit(expression: Any) -> None:
            if not isinstance(expression, dict):
                raise ValueError(f"trusted law {name} contains a non-AST expression")
            kind = expression.get("kind")
            if kind == "parameter":
                ordinal = expression.get("ordinal")
                if not isinstance(ordinal, int) or not 0 <= ordinal < len(parameters):
                    raise ValueError(f"trusted law {name} has an invalid parameter reference")
                return
            if kind == "generic":
                ordinal = expression.get("ordinal")
                if not isinstance(ordinal, int) or not 0 <= ordinal < generic_count:
                    raise ValueError(f"trusted law {name} has an invalid generic reference")
                return
            if kind == "call":
                if not NAME.fullmatch(expression.get("operation", "")) or not isinstance(expression.get("arguments"), list):
                    raise ValueError(f"trusted law {name} has a malformed call")
                for argument in expression["arguments"]:
                    visit(argument)
                return
            if kind == "implies":
                visit(expression.get("antecedent"))
                visit(expression.get("consequent"))
                return
            if kind == "equal":
                visit(expression.get("left"))
                visit(expression.get("right"))
                return
            raise ValueError(f"trusted law {name} uses unknown expression kind {kind!r}")

        visit(law.get("expression"))


def inferred_argument_kind(argument: str, environment: dict[str, str], constructors: dict[str, tuple[list[str], str]]) -> str:
    argument = argument.strip()
    if argument in environment:
        return environment[argument]
    if re.fullmatch(r"-?[0-9]+", argument):
        return "Const<int>"
    if re.fullmatch(r"[A-Za-z_][A-Za-z0-9_?!-]*:\"(?:[^\"\\]|\\.)*\"", argument):
        return "Const<path-selector>"
    if argument in {"non-suspending", "suspends"}:
        return "Const<suspension>"
    match = re.match(r"([A-Za-z_][A-Za-z0-9_?!-]*)\s*<", argument)
    if match:
        opening = argument.index("<", match.start())
        if matching_angle(argument, opening) == len(argument) - 1:
            return check_application(match.group(1), argument[opening + 1 : -1], environment, constructors)
    return "Type"


def kind_compatible(actual: str, expected: str) -> bool:
    if expected == "TypePack":
        return actual in {"Type", "TypePack"}
    return actual == expected


def check_application(name: str, arguments_text: str, environment: dict[str, str], constructors: dict[str, tuple[list[str], str]]) -> str:
    arguments = [] if not arguments_text.strip() else split_top_level(arguments_text, ",")
    if name in {"effects", "state", "state-read", "returns-loan"}:
        if name == "effects":
            if len(arguments) > 1:
                raise ValueError("effects accepts at most one effect-row argument")
            if arguments and inferred_argument_kind(arguments[0], environment, constructors) != "EffectRow":
                raise ValueError(f"effects argument {arguments[0]!r} is not an EffectRow")
        elif len(arguments) != 1:
            raise ValueError(f"{name} requires exactly one target argument")
        return "ContractItem"
    if name == "Const":
        if len(arguments) != 1:
            raise ValueError("Const requires exactly one value argument")
        return "Type"
    if name in {"tuple", "throws"}:
        if name == "throws":
            actual_kinds = [inferred_argument_kind(argument, environment, constructors) for argument in arguments]
            if len(actual_kinds) == 1 and actual_kinds[0] == "ExceptionSet":
                return "ExceptionSet"
            for argument, actual in zip(arguments, actual_kinds):
                if actual != "Type":
                    raise ValueError(f"throws argument {argument!r} has kind {actual}, expected Type")
            return "ExceptionSet"
        actual_kinds = [inferred_argument_kind(argument, environment, constructors) for argument in arguments]
        for argument, actual in zip(arguments, actual_kinds):
            if actual != "Type":
                raise ValueError(f"tuple argument {argument!r} has kind {actual}, expected Type")
        return "Type"
    if name not in constructors:
        raise ValueError(f"unknown generic constructor {name}")
    parameters, result = constructors[name]
    if len(arguments) != len(parameters):
        raise ValueError(f"{name} expects {len(parameters)} generic arguments, found {len(arguments)}")
    for position, (argument, expected) in enumerate(zip(arguments, parameters), 1):
        actual = inferred_argument_kind(argument, environment, constructors)
        if not kind_compatible(actual, expected):
            raise ValueError(f"{name} argument {position} {argument!r} has kind {actual}, expected {expected}")
    return result


def check_applications(text: str, environment: dict[str, str], constructors: dict[str, tuple[list[str], str]]) -> None:
    position = 0
    while True:
        match = APPLICATION.search(text, position)
        if not match:
            return
        opening = text.index("<", match.start())
        closing = matching_angle(text, opening)
        check_application(match.group(1), text[opening + 1 : closing], environment, constructors)
        check_applications(text[opening + 1 : closing], environment, constructors)
        position = closing + 1


def validate_prelude(definitions: dict[str, Any]) -> list[str]:
    errors: list[str] = []
    constructors: dict[str, tuple[list[str], str]] = {}
    for entry in definitions["types"]:
        try:
            constructors[entry["name"]] = parse_kind_signature(entry["kind"])
        except ValueError as error:
            errors.append(f"type {entry['name']}: {error}")
    for concept in definitions["concepts"]:
        signature = concept["signature"]
        try:
            match = re.search(r"\bconcept\s+([A-Za-z_][A-Za-z0-9_?!-]*)\s*<", signature)
            if not match:
                constructors[concept["name"]] = ([], "Evidence")
                continue
            opening = signature.index("<", match.start())
            closing = matching_angle(signature, opening)
            parameters = list(generic_environment(signature[opening + 1 : closing]).values())
            constructors[match.group(1)] = (parameters, "Evidence")
        except ValueError as error:
            errors.append(f"concept {concept['name']}: {error}")
    for operation in definitions["operations"]:
        try:
            environment, body = leading_header(operation["signature"])
            if "->" not in body or " ! " not in body:
                raise ValueError("signature must contain result arrow and callable contract")
            split_top_level(body, ",")  # validates every delimiter and quote, without discarding text
            check_applications(body, environment, constructors)
            parse_callable_signature(operation["signature"], constructors)
        except ValueError as error:
            errors.append(f"operation {operation['name']}: {error}")
    for concept in definitions["concepts"]:
        signature = concept["signature"]
        try:
            match = re.search(r"\bconcept\s+[A-Za-z_][A-Za-z0-9_?!-]*\s*<", signature)
            environment: dict[str, str] = {}
            body = signature
            if match and "<" in match.group(0):
                opening = signature.index("<", match.start())
                closing = matching_angle(signature, opening)
                environment = generic_environment(signature[opening + 1 : closing])
                body = signature[closing + 1 :]
            split_top_level(signature, ",")
            check_applications(body, environment, constructors)
            signature_ast = parse_concept_signature(signature, constructors)
            validate_trusted_laws(concept, signature_ast)
        except ValueError as error:
            errors.append(f"concept {concept['name']}: {error}")
    return errors
