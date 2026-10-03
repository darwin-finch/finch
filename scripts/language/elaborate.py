#!/usr/bin/env python3
"""Semantic construction for the executable core of both Finch frontends.

Each frontend's parse tree (produced by ``grammar_engine`` from the normative grammar) is lowered to
the same normalized AST.  The two functions here share no syntax knowledge: ``elaborate_colisp``
walks nested forms, ``elaborate_coforth`` rebuilds expression trees from postfix words.  A paired
fixture is valid only when both produce an identical AST for their own spelling.

Forms outside the executable core raise ``ElaborationError``; they are covered by the parse corpus,
not by execution vectors.
"""

from __future__ import annotations

import re
from typing import Any

from grammar_engine import parse


BINARY_WORDS = {"+", "-", "*", "/", "==", "!=", "<", "<=", ">", ">="}
GUARD_REASONS = {"on-exit": "exit", "on-success": "success", "on-failure": "failure", "on-cancel": "cancel"}
CONTRACT_SUGAR = {
    "plain": "effects<> | nothrow | non-suspending",
    "inferred": "effects-infer | throws-infer | suspends-infer",
}
CHILD_LISTS = ("items", "arguments", "arms", "catches", "guards", "fields", "parameters", "captures", "cases")


class ElaborationError(Exception):
    """A source the frontend cannot construct.  ``code`` is set when the rejection is a language diagnostic."""

    def __init__(self, message: str, code: str | None = None):
        super().__init__(message)
        self.code = code


def is_rule(node: dict[str, Any], name: str) -> bool:
    return node.get("rule") == name


def rules(node: dict[str, Any], name: str) -> list[dict[str, Any]]:
    return [child for child in node["children"] if child.get("rule") == name]


def tokens(node: dict[str, Any], name: str) -> list[dict[str, Any]]:
    return [child for child in node["children"] if child.get("token") == name]


def has_literal(node: dict[str, Any], text: str) -> bool:
    return any(child.get("token") == "literal" and child["text"] == text for child in node["children"])


CALLABLE_SHAPES: dict[str, tuple[int, int]] = {}


def leaves(node: dict[str, Any]) -> list[str]:
    """Terminal spellings of a type, with any callable contract in its normalized three-axis form."""
    if "rule" not in node:
        return [node["text"]]
    if node["rule"] == "contract":
        return ["!", contract_text(node).replace(" | ", "|")]
    found: list[str] = []
    for child in node["children"]:
        found.extend(leaves(child))
    if node["rule"] == "callable_type" and not rules(node, "contract"):
        found[-1:] = ["!", contract_text(None).replace(" | ", "|"), ">"]
    return found


def canonical_text(node: dict[str, Any]) -> str:
    """Whitespace-independent spelling of a type or contract item."""
    out = ""
    for leaf in leaves(node):
        if out and re.match(r"[\w&]", leaf[0]) and re.match(r"\w", out[-1]):
            out += " "
        out += leaf
    target = node
    while target.get("rule") in {"type", "primary_type"} and len(target["children"]) == 1:
        target = target["children"][0]
    if target.get("rule") == "callable_type":
        result = canonical_text(rules(target, "type")[0])
        CALLABLE_SHAPES[out] = (len(rules(target, "callable_parameter")), 0 if result == "unit" else 1)
    return out


def contract_axis(item: str) -> str | None:
    """Which of the three closed axes a normalized contract item belongs to, if any."""
    if item in {"pure", "effects-infer"} or item.startswith("effects<"):
        return "effect"
    if item in {"nothrow", "throws-infer"} or item.startswith("throws<"):
        return "exception"
    if item in {"non-suspending", "suspends", "suspends-infer"}:
        return "suspension"
    return None


def checked_contract(items: list[str]) -> str:
    """Join contract items after sugar, enforcing the axis rules every frontend shares."""
    text = " | ".join(CONTRACT_SUGAR.get(item, item) for item in items)
    axes = [contract_axis(item) for item in text.split(" | ")]
    for axis in ("effect", "exception", "suspension"):
        if axes.count(axis) > 1:
            raise ElaborationError(f"contract {text!r} states its {axis} axis more than once", "F-DIAG-CONTRACT-AXES")
    if any(axes) and not ("exception" in axes and "suspension" in axes):
        raise ElaborationError(f"contract {text!r} must state both its exception and its suspension axis", "F-DIAG-CONTRACT-AXES")
    return text


def contract_text(node: dict[str, Any] | None) -> str:
    if node is None:
        return CONTRACT_SUGAR["inferred"]
    return checked_contract([canonical_text(item) for item in rules(node, "contract_item")])


def spanned(node: dict[str, Any], tree: dict[str, Any]) -> dict[str, Any]:
    node["$span"] = [tree["start"], tree["end"]]
    return node


def cover(node: dict[str, Any], *parts: dict[str, Any]) -> dict[str, Any]:
    starts = [part["$span"][0] for part in parts if "$span" in part]
    ends = [part["$span"][1] for part in parts if "$span" in part]
    if "$span" in node:
        starts.append(node["$span"][0])
        ends.append(node["$span"][1])
    node["$span"] = [min(starts), max(ends)]
    return node


def strip_spans(value: Any) -> Any:
    if isinstance(value, dict):
        return {key: strip_spans(item) for key, item in value.items() if key != "$span"}
    if isinstance(value, list):
        return [strip_spans(item) for item in value]
    return value


def decode_escapes(body: str) -> str:
    def replace(match: re.Match[str]) -> str:
        text = match.group(0)
        if text[1] == "x":
            return chr(int(text[2:], 16))
        if text[1] == "u":
            return chr(int(text[3:-1], 16))
        return {"\\\\": "\\", '\\"': '"', "\\n": "\n", "\\r": "\r", "\\t": "\t", "\\0": "\0"}[text]

    return re.sub(r"\\(?:[\\\"nrt0]|x[0-9A-Fa-f]{2}|u\{[0-9A-Fa-f]+\})", replace, body)


def dedent_triple(body: str) -> str:
    """Section 3.2: drop one leading newline, then remove the minimum ASCII-space indentation."""
    if body.startswith("\r\n"):
        body = body[2:]
    elif body.startswith("\n"):
        body = body[1:]
    lines = body.split("\n")
    indents = [len(line) - len(line.lstrip(" ")) for line in lines if line.strip(" \r") != ""]
    width = min(indents) if indents else 0
    return "\n".join(line[width:] if len(line) - len(line.lstrip(" ")) >= width else line for line in lines)


def integer_value(text: str) -> tuple[int, str | None]:
    match = re.fullmatch(r"(.*?)((?:i|u)(?:8|16|32|64))?", text)
    assert match is not None
    digits, suffix = match.group(1).replace("_", ""), match.group(2)
    if suffix and digits[:2].lower() == "0x" and not re.fullmatch(r"0[xX][0-9A-Fa-f]+", digits):
        digits, suffix = text.replace("_", ""), None
    return int(digits, 0), suffix


def literal_node(tree: dict[str, Any]) -> dict[str, Any]:
    """Lower a ``literal`` or ``base_literal`` production shared by both frontends."""
    leaf = tree
    while "rule" in leaf:
        leaf = leaf["children"][0]
    kind, text = leaf["token"], leaf["text"]
    if kind == "literal":
        if text in {"true", "false"}:
            return spanned({"form": "literal", "value": text == "true"}, tree)
        raise ElaborationError(f"literal {text} is outside the executable core")
    if kind == "colisp_boolean":
        return spanned({"form": "literal", "value": text == "#t"}, tree)
    if kind == "integer":
        value, suffix = integer_value(text)
        node: dict[str, Any] = {"form": "literal", "value": value}
        if suffix:
            node["type"] = suffix
        return spanned(node, tree)
    if kind == "negative_numeric":
        if re.search(r"[.eE]", text) and not text.lower().startswith("-0x"):
            raise ElaborationError("floating literals are outside the executable core")
        value, suffix = integer_value(text[1:])
        operand: dict[str, Any] = {"form": "literal", "value": value}
        if suffix:
            operand["type"] = suffix
        operand["$span"] = [tree["start"] + 1, tree["end"]]
        return spanned({"form": "call", "callee": "-", "arguments": [operand]}, tree)
    if kind == "escaped_string":
        body = text[text.index('"') + 1 : -1]
        if text.startswith('s"') and body.startswith(" "):
            body = body[1:]
        return spanned({"form": "literal", "value": decode_escapes(body)}, tree)
    if kind == "raw_string":
        return spanned({"form": "literal", "value": text[text.index('"') + 1 : text.rindex('"')]}, tree)
    if kind == "triple_string":
        return spanned({"form": "literal", "value": dedent_triple(decode_escapes(text[text.index('"""') + 3 : -3]))}, tree)
    raise ElaborationError(f"{kind} literals are outside the executable core")


def pattern_literal(tree: dict[str, Any]) -> Any:
    node = literal_node(tree)
    if node["form"] != "literal":
        raise ElaborationError("a negative literal pattern is outside the executable core")
    return node["value"]


def sequence_of(nodes: list[dict[str, Any]]) -> dict[str, Any]:
    if len(nodes) == 1:
        return nodes[0]
    return cover({"form": "sequence", "items": nodes}, *nodes)


# ---------------------------------------------------------------------------------------- CoLisp


def colisp_pattern(tree: dict[str, Any]) -> Any:
    alternative = tree["alternative"]
    children = tree["children"]
    if alternative == 0:
        return "_"
    if alternative == 1:
        return pattern_literal(children[0])
    if alternative == 2:
        return {"bind": children[0]["text"]}
    if alternative == 4:
        return {"or": [colisp_pattern(child) for child in rules(tree, "pattern")]}
    if alternative == 5:
        return {"as": tokens(tree, "identifier")[0]["text"], "pattern": colisp_pattern(rules(tree, "pattern")[0])}
    if alternative == 6:
        name = tokens(tree, "identifier")[0]["text"]
        parts = rules(tree, "pattern")
        if parts and all(part["alternative"] == 3 for part in parts):
            return {
                "record": name,
                "fields": [{"name": part["children"][0]["text"][1:], "pattern": colisp_pattern(part["children"][1])} for part in parts],
            }
        if any(part["alternative"] == 3 for part in parts):
            raise ElaborationError("a constructor pattern cannot mix positional and named fields")
        return {"constructor": name, "arguments": [colisp_pattern(part) for part in parts]}
    if alternative == 7:
        return {"tuple": [colisp_pattern(child) for child in rules(tree, "pattern")]}
    raise ElaborationError("a field pattern is valid only inside a record pattern")


def colisp_parameter(tree: dict[str, Any]) -> dict[str, Any]:
    ownership = "borrow-mut" if has_literal(tree, "borrow-mut") else "steal" if has_literal(tree, "steal") else "borrow"
    return spanned(
        {"form": "parameter", "name": tokens(tree, "identifier")[0]["text"], "type": canonical_text(rules(tree, "type")[0]), "ownership": ownership},
        tree,
    )


def colisp_captures(tree: dict[str, Any] | None) -> tuple[str, list[dict[str, Any]]]:
    if tree is None:
        return "borrow", []
    entries = [
        spanned({"form": "capture", "name": tokens(entry, "identifier")[0]["text"], "ownership": canonical_text(rules(entry, "ownership_mode")[0])}, entry)
        for entry in rules(tree, "capture_entry")
    ]
    if has_literal(tree, ":move"):
        return "move", entries
    return "exact", entries


def colisp_expressions(tree: dict[str, Any]) -> list[dict[str, Any]]:
    return [colisp_expression(child) for child in rules(tree, "expression")]


def colisp_expression(tree: dict[str, Any]) -> dict[str, Any]:
    while tree.get("rule") in {"form", "expression"}:
        tree = tree["children"][0]
    if "token" in tree:
        if tree["token"] in {"identifier", "operator_identifier"}:
            return spanned({"form": "read", "place": tree["text"]}, tree)
        raise ElaborationError(f"{tree['token']} expressions are outside the executable core")
    if is_rule(tree, "literal"):
        return literal_node(tree)
    if not is_rule(tree, "list"):
        raise ElaborationError(f"{tree['rule']} expressions are outside the executable core")
    inner = rules(tree, "list_body")[0]["children"][0]
    return spanned(colisp_list(inner), tree)


def colisp_list(inner: dict[str, Any]) -> dict[str, Any]:
    if is_rule(inner, "empty"):
        return {"form": "literal", "value": None}
    if is_rule(inner, "call"):
        head, *arguments = rules(inner, "form")
        callee_tree = head
        while callee_tree.get("rule") in {"form", "expression"}:
            callee_tree = callee_tree["children"][0]
        callee: Any = callee_tree["text"] if callee_tree.get("token") in {"identifier", "operator_identifier"} else colisp_expression(head)
        return {"form": "call", "callee": callee, "arguments": [colisp_expression(argument) for argument in arguments]}
    if is_rule(inner, "record_constructor"):
        names = tokens(inner, "keyword")
        values = rules(inner, "expression")
        fields = [
            cover({"form": "field-init", "name": name["text"][1:], "value": colisp_expression(value), "$span": [name["start"], name["end"]]})
            for name, value in zip(names, values)
        ]
        for field in fields:
            cover(field, field["value"])
        return {"form": "record-construct", "type": tokens(inner, "identifier")[0]["text"], "fields": fields}
    form = inner["children"][0]
    name = form["rule"]
    body = colisp_expressions(form)
    if name == "define_form":
        return colisp_function(form)
    if name == "let_form":
        node = sequence_of(body)
        for binding in reversed(rules(form, "binding")):
            entry: dict[str, Any] = {"form": "let", "name": tokens(binding, "identifier")[0]["text"], "mutable": has_literal(binding, "mut")}
            if rules(binding, "type"):
                entry["type"] = canonical_text(rules(binding, "type")[0])
            entry["initializer"] = colisp_expression(rules(binding, "expression")[0])
            entry["body"] = node
            node = cover(entry, spanned({}, binding), node)
        return node
    if name == "set_form":
        place = rules(form, "place")[0]
        if len(place["children"]) != 1:
            raise ElaborationError("projection assignment is outside the executable core")
        return {"form": "assign", "place": place["children"][0]["text"], "value": body[0]}
    if name == "begin_form":
        return {"form": "sequence", "items": body}
    if name == "if_form":
        return {"form": "if", "condition": body[0], "then": body[1], "else": body[2]}
    if name == "while_form":
        return {"form": "while", "condition": body[0], "body": sequence_of(body[1:])}
    if name == "break_form":
        return {"form": "break"}
    if name == "continue_form":
        return {"form": "continue-loop"}
    if name == "return_form":
        return {"form": "return", "value": body[0]}
    if name == "throw_form":
        return {"form": "throw", "value": body[0]}
    if name == "rethrow_form":
        return {"form": "rethrow"}
    if name == "yield_form":
        return {"form": "yield", "value": body[0]}
    if name == "match_form":
        marker = rules(form, "ownership_marker")
        arms = []
        for arm in rules(form, "match_arm"):
            arms.append(spanned({"form": "match-arm", "pattern": colisp_pattern(rules(arm, "pattern")[0]), "body": sequence_of(colisp_expressions(arm))}, arm))
        return {"form": "match", "ownership": canonical_text(marker[0])[1:] if marker else "borrow", "scrutinee": body[0], "arms": arms}
    if name == "try_form":
        catches = []
        for clause in rules(form, "catch_clause"):
            catches.append(spanned({"form": "catch", "pattern": colisp_pattern(rules(clause, "pattern")[0]), "body": sequence_of(colisp_expressions(clause))}, clause))
        return {"form": "try", "body": sequence_of(body), "catches": catches}
    if name == "scope_form":
        guards = []
        for guard in rules(form, "scope_guard"):
            reason = GUARD_REASONS[guard["children"][1]["text"]]
            guards.append(spanned({"form": "guard", "reason": reason, "body": sequence_of(colisp_expressions(guard))}, guard))
        return {"form": "scope", "guards": guards, "body": sequence_of(body)}
    if name == "lambda_form":
        default, captures = colisp_captures((rules(form, "capture_spec") or [None])[0])
        return {
            "form": "lambda", "capture_default": default, "contract": contract_text((rules(form, "contract") or [None])[0]),
            "captures": captures, "parameters": [colisp_parameter(parameter) for parameter in rules(form, "parameter")], "body": sequence_of(body),
        }
    if name == "index_form":
        return {"form": "call", "callee": "get", "arguments": body}
    if name == "member_access":
        return {"form": "member", "name": tokens(form, "identifier")[0]["text"], "target": body[0]}
    if name == "variant_form":
        if rules(form, "generic_header"):
            raise ElaborationError("generic variants are outside the executable core")
        cases = []
        for case in rules(form, "variant_case"):
            if rules(case, "field_decl"):
                raise ElaborationError("record-payload variant cases are outside the executable core")
            payload = [canonical_text(item) for item in rules(case, "type")]
            cases.append(spanned({"form": "case", "name": tokens(case, "identifier")[0]["text"], "payload_kind": "tuple" if payload else "unit", "payload": payload}, case))
        return {"form": "variant", "name": tokens(form, "identifier")[0]["text"], "representation": "native", "cases": cases}
    raise ElaborationError(f"{name} is outside the executable core")


def colisp_function(form: dict[str, Any]) -> dict[str, Any]:
    if rules(form, "generic_header"):
        raise ElaborationError("generic functions are outside the executable core")
    return {
        "form": "function", "name": tokens(form, "identifier")[0]["text"], "result": canonical_text(rules(form, "type")[0]),
        "contract": contract_text((rules(form, "contract") or [None])[0]),
        "parameters": [colisp_parameter(parameter) for parameter in rules(form, "parameter")],
        "body": sequence_of(colisp_expressions(form)),
    }


def elaborate_colisp(source: str, root: str = "submission") -> dict[str, Any]:
    tree = parse("colisp", root, source)
    items = [colisp_expression(child) for child in tree["children"] if "rule" in child]
    if not items:
        raise ElaborationError("an executable submission needs at least one form")
    return sequence_of(items)


# --------------------------------------------------------------------------------------- Co-Forth


class ForthEnvironment:
    def __init__(self, words: dict[str, tuple[int, int]], result_count: int = 1, resume_count: int | None = None):
        self.words = words
        self.result_count = result_count
        self.resume_count = resume_count
        self.locals: set[str] = set()
        self.pending: dict[str, dict[str, Any]] = {}
        self.callables: dict[str, tuple[int, int]] = {}
        self.shapes: dict[int, tuple[int, int]] = {}
        self.results: dict[str, str] = {}

    def child(self, **changes: Any) -> "ForthEnvironment":
        other = ForthEnvironment(self.words, self.result_count, self.resume_count)
        other.locals = set(self.locals)
        other.pending = dict(self.pending)
        other.callables = dict(self.callables)
        other.shapes = self.shapes
        other.results = self.results
        for key, value in changes.items():
            setattr(other, key, value)
        return other


class ForthBody:
    """Result of elaborating one word sequence: its node, value count, and whether it diverges."""

    def __init__(self, node: dict[str, Any], values: int, diverges: bool):
        self.node = node
        self.values = values
        self.diverges = diverges


def callable_shape(type_text: str | None) -> tuple[int, int] | None:
    """Argument and result counts of a `callable<(parameters) -> result>` type, or None."""
    return CALLABLE_SHAPES.get(type_text) if type_text else None


def applied_shape(node: dict[str, Any], environment: "ForthEnvironment") -> tuple[int, int]:
    """Stack effect of the callable value `call` is about to apply."""
    if id(node) in environment.shapes:
        return environment.shapes[id(node)]
    if node["form"] == "read":
        if node["place"] in environment.callables:
            return environment.callables[node["place"]]
        if node["place"] in environment.words and node["place"] not in environment.locals:
            return environment.words[node["place"]]
    if node["form"] == "call" and isinstance(node["callee"], str):
        shape = callable_shape(environment.results.get(node["callee"]))
        if shape is not None:
            return shape
    raise ElaborationError("call needs a callable whose signature is known: a quotation, a ticked word, or a value of declared callable type")


def coforth_entry(tree: dict[str, Any]) -> dict[str, Any] | None:
    """One stack-signature entry as a parameter node, or None for a stack row."""
    if rules(tree, "stack_row"):
        return None
    types = rules(tree, "type")
    labels = tokens(tree, "label")
    names = tokens(tree, "identifier")
    name = labels[0]["text"][:-1] if labels else names[0]["text"] if names else None
    mode = rules(tree, "source_mode")
    ownership = canonical_text(mode[0]) if mode else "borrow"
    return spanned({"form": "parameter", "name": name, "type": canonical_text(types[0]), "ownership": ownership}, tree)


def coforth_signature(tree: dict[str, Any]) -> tuple[list[dict[str, Any]], list[str], str]:
    inputs = [entry for entry in (coforth_entry(child) for child in rules(rules(tree, "stack_input")[0], "stack_entry")) if entry]
    outputs = [entry["type"] for entry in (coforth_entry(child) for child in rules(rules(tree, "stack_output")[0], "stack_entry")) if entry]
    return inputs, outputs, contract_text((rules(tree, "contract") or [None])[0])


def result_type(outputs: list[str]) -> tuple[str, int]:
    if not outputs or outputs == ["unit"]:
        return "unit", 0
    if len(outputs) == 1:
        return outputs[0], 1
    raise ElaborationError("multiple stack results are outside the executable core")


def coforth_pattern(tree: dict[str, Any]) -> Any:
    atom = rules(tree, "pattern_atom")[0]
    alternative = atom["alternative"]
    if alternative == 0:
        pattern: Any = "_"
    elif alternative == 1:
        pattern = pattern_literal(atom["children"][0])
    elif alternative == 2:
        pattern = {"or": [coforth_pattern(child) for child in rules(atom, "pattern")]}
    elif alternative == 3:
        pattern = {"constructor": tokens(atom, "identifier")[0]["text"], "arguments": [coforth_pattern(child) for child in rules(atom, "pattern")]}
    elif alternative == 4:
        fields = []
        for named in rules(atom, "named_pattern"):
            label = tokens(named, "label")
            name = label[0]["text"][:-1] if label else tokens(named, "identifier")[0]["text"]
            fields.append({"name": name, "pattern": coforth_pattern(rules(named, "pattern")[0])})
        pattern = {"record": tokens(atom, "identifier")[0]["text"], "fields": fields}
    elif alternative == 5:
        pattern = {"bind": atom["children"][0]["text"]}
    else:
        pattern = {"tuple": [coforth_pattern(child) for child in rules(atom, "pattern")]}
    names = tokens(tree, "identifier")
    return {"as": names[0]["text"], "pattern": pattern} if names else pattern


def pattern_names(pattern: Any) -> set[str]:
    if not isinstance(pattern, dict):
        return set()
    found: set[str] = set()
    if "bind" in pattern:
        found.add(pattern["bind"])
    if "as" in pattern:
        found.add(pattern["as"])
    for key in ("arguments", "or", "tuple"):
        for item in pattern.get(key, []):
            found |= pattern_names(item)
    for field in pattern.get("fields", []):
        found |= pattern_names(field["pattern"])
    if "pattern" in pattern:
        found |= pattern_names(pattern["pattern"])
    return found


def coforth_body(items: list[dict[str, Any]], environment: ForthEnvironment, anchor: dict[str, Any]) -> ForthBody:
    """Rebuild one expression tree from a postfix word sequence.

    ``stack`` holds value nodes not yet consumed.  A word that produces no value is a statement; it
    must not be issued while an older value is still pending, because no shared AST node can express
    "evaluate A, then run a statement, then use A".
    """
    out: list[dict[str, Any]] = []
    stack: list[dict[str, Any]] = []
    prefix: list[dict[str, Any]] = []
    diverges = False

    def push(node: dict[str, Any]) -> None:
        """A value issued after deferred statements evaluates them first, in source order."""
        if prefix:
            node = sequence_of(prefix + [node])
            prefix.clear()
        stack.append(node)

    def take(count: int, word: str) -> list[dict[str, Any]]:
        if len(stack) < count:
            raise ElaborationError(f"{word} needs {count} stack values, found {len(stack)}")
        taken = stack[len(stack) - count :]
        del stack[len(stack) - count :]
        return taken

    def statement(node: dict[str, Any]) -> None:
        (prefix if stack else out).append(node)

    def emit(node: dict[str, Any], values: int) -> None:
        if values:
            push(node)
        else:
            statement(node)

    def finish() -> ForthBody:
        if prefix:
            raise ElaborationError("a statement word cannot be the last word while an earlier value is still on the stack")
        if len(stack) > 1:
            raise ElaborationError(f"body leaves {len(stack)} values")
        nodes = out + stack
        if not nodes:
            return ForthBody({"form": "literal", "value": None, "$span": [anchor["end"], anchor["end"]]}, 0, False)
        return ForthBody(sequence_of(nodes), len(stack), diverges)

    for index, item in enumerate(items):
        tree = item["children"][0]
        rest = items[index + 1 :]
        if "token" in tree:
            word = tree["text"]
            if tree["token"] == "member_projection":
                target = take(1, word)[0]
                push(cover(spanned({"form": "member", "name": word[1:], "target": target}, tree), target))
                continue
            if tree["token"] not in {"identifier", "operator_identifier"}:
                raise ElaborationError(f"{tree['token']} words are outside the executable core")
            if word in environment.pending:
                raise ElaborationError(f"local {word} is read before its first assignment")
            if word in environment.callables:
                arity, values = environment.callables[word]
                arguments = take(arity, word)
                emit(cover(spanned({"form": "call", "callee": word, "arguments": arguments}, tree), *arguments), values)
            elif word in environment.locals:
                push(spanned({"form": "read", "place": word}, tree))
            elif word == "drop":
                statement_node = take(1, word)[0]
                statement(statement_node)
            elif word in environment.words:
                arity, values = environment.words[word]
                arguments = take(arity, word)
                emit(cover(spanned({"form": "call", "callee": word, "arguments": arguments}, tree), *arguments), values)
            else:
                # A postfix reader cannot build a tree around a word whose stack effect it does not know.
                raise ElaborationError(f"word {word!r} is not bound", "F-DIAG-UNBOUND-NAME")
            continue
        name = tree["rule"]
        if name == "literal":
            push(literal_node(tree))
        elif name == "reference":
            push(spanned({"form": "read", "place": tree["children"][1]["text"]}, tree))
        elif name == "call_expression":
            callee = take(1, "call")[0]
            arity, values = applied_shape(callee, environment)
            arguments = take(arity, "call")
            emit(cover(spanned({"form": "call", "callee": callee, "arguments": arguments}, tree), callee, *arguments), values)
        elif name == "assignment":
            target = tokens(tree, "identifier")[0]["text"]
            value = take(1, "to")[0]
            if target in environment.pending:
                declared = environment.pending[target]
                inner = environment.child()
                del inner.pending[target]
                inner.locals.add(target)
                if stack or prefix:
                    raise ElaborationError("a local cannot be initialized while an earlier value is still on the stack")
                body = coforth_body(rest, inner, anchor)
                # The binding's origin runs from its declaration in the locals block through its scope.
                node = cover({**declared, "initializer": value, "body": body.node}, spanned({}, tree), value, body.node)
                return ForthBody(sequence_of(out + [node]), body.values, body.diverges)
            statement(cover(spanned({"form": "assign", "place": target, "value": value}, tree), value))
        elif name == "locals":
            marker = next(index for index, child in enumerate(tree["children"]) if child.get("text") == "--")
            before = [child for child in tree["children"][:marker] if child.get("rule") == "local_entry"]
            after = [child for child in tree["children"][marker:] if child.get("rule") == "local_entry"]
            initial = take(len(before), "locals")
            inner = environment.child()
            for entry in after:
                declared = coforth_local(entry)
                if not declared["mutable"]:
                    raise ElaborationError("a local declared after -- has no initializer and must be mut")
                inner.pending[declared["name"]] = spanned(declared, tree)
            for entry in before:
                declared = coforth_local(entry)
                inner.locals.add(declared["name"])
                shape = callable_shape(declared.get("type"))
                if shape is not None:
                    inner.callables[declared["name"]] = shape
            body = coforth_body(rest, inner, anchor)
            node = body.node
            for entry, value in reversed(list(zip(before, initial))):
                node = cover(spanned({**coforth_local(entry), "initializer": value, "body": node}, tree), value, node)
            if stack or prefix:
                raise ElaborationError("a locals block cannot open while an earlier value is still on the stack")
            tail = node["items"] if not before and node["form"] == "sequence" else [node]
            return ForthBody(sequence_of(out + tail), body.values, body.diverges)
        elif name == "if_expression":
            condition = take(1, "if")[0]
            split = next((i for i, child in enumerate(tree["children"]) if child.get("text") == "else"), None)
            children = tree["children"]
            then_items = [child for child in (children[:split] if split is not None else children) if child.get("rule") == "expression"]
            else_items = [child for child in (children[split:] if split is not None else []) if child.get("rule") == "expression"]
            then_body = coforth_body(then_items, environment.child(), tree)
            else_body = coforth_body(else_items, environment.child(), tree)
            values = else_body.values if then_body.diverges else then_body.values
            if not then_body.diverges and not else_body.diverges and then_body.values != else_body.values:
                raise ElaborationError("if arms leave different stack depths")
            emit(cover(spanned({"form": "if", "condition": condition, "then": then_body.node, "else": else_body.node}, tree), condition), values)
        elif name == "while_expression":
            split = next(i for i, child in enumerate(tree["children"]) if child.get("text") == "while")
            condition = coforth_body([c for c in tree["children"][:split] if c.get("rule") == "expression"], environment.child(), tree)
            body = coforth_body([c for c in tree["children"][split:] if c.get("rule") == "expression"], environment.child(), tree)
            if condition.values != 1 or body.values != 0:
                raise ElaborationError("a loop condition leaves one value and its body leaves none")
            statement(spanned({"form": "while", "condition": condition.node, "body": body.node}, tree))
        elif name in {"break_expression", "continue_expression"}:
            statement(spanned({"form": "break" if name == "break_expression" else "continue-loop"}, tree))
            diverges = True
        elif name == "return_expression":
            if environment.result_count:
                value = take(1, "return")[0]
            else:
                value = {"form": "literal", "value": None, "$span": [tree["start"], tree["start"]]}
            statement(cover(spanned({"form": "return", "value": value}, tree), value))
            diverges = True
        elif name == "throw_expression":
            value = take(1, "throw")[0]
            statement(cover(spanned({"form": "throw", "value": value}, tree), value))
            diverges = True
        elif name == "rethrow_expression":
            statement(spanned({"form": "rethrow"}, tree))
            diverges = True
        elif name == "yield_expression":
            # `yield` leaves the option of a reply.  Whether this body may yield is decided after reading.
            value = take(1, "yield")[0]
            emit(cover(spanned({"form": "yield", "value": value}, tree), value), 1)
        elif name == "match_expression":
            scrutinee = take(1, "match")[0]
            marker = rules(tree, "ownership_marker")
            arms = []
            values = None
            for arm in rules(tree, "match_arm"):
                pattern = coforth_pattern(rules(arm, "pattern")[0])
                inner = environment.child()
                inner.locals |= pattern_names(pattern)
                arm_body = coforth_body(rules(arm, "expression"), inner, arm)
                if not arm_body.diverges:
                    if values is not None and values != arm_body.values:
                        raise ElaborationError("match arms leave different stack depths")
                    values = arm_body.values
                arms.append(spanned({"form": "match-arm", "pattern": pattern, "body": arm_body.node}, arm))
            node = {"form": "match", "ownership": canonical_text(marker[0]) if marker else "borrow", "scrutinee": scrutinee, "arms": arms}
            emit(cover(spanned(node, tree), scrutinee), values or 0)
        elif name == "try_expression":
            body = coforth_body(rules(tree, "expression"), environment.child(), tree)
            catches = []
            values = None if body.diverges else body.values
            for arm in rules(tree, "catch_arm"):
                pattern = coforth_pattern(rules(arm, "pattern")[0])
                inner = environment.child()
                inner.locals |= pattern_names(pattern)
                arm_body = coforth_body(rules(arm, "expression"), inner, arm)
                if not arm_body.diverges:
                    if values is not None and values != arm_body.values:
                        raise ElaborationError("try and catch arms leave different stack depths")
                    values = arm_body.values
                catches.append(spanned({"form": "catch", "pattern": pattern, "body": arm_body.node}, arm))
            emit(spanned({"form": "try", "body": body.node, "catches": catches}, tree), values or 0)
            diverges = values is None
        elif name == "scope_expression":
            guards = []
            for guard in rules(tree, "scope_guard"):
                quotation = rules(guard, "quotation")[0]
                inputs, outputs, _ = coforth_signature(rules(quotation, "stack_signature")[0])
                if inputs or result_type(outputs)[1]:
                    raise ElaborationError("a guard quotation takes and leaves nothing")
                guard_body = coforth_body(rules(quotation, "expression"), environment.child(), quotation)
                guards.append(spanned({"form": "guard", "reason": GUARD_REASONS[guard["children"][0]["text"]], "body": guard_body.node}, guard))
            body = coforth_body(rules(tree, "expression"), environment.child(), tree)
            emit(spanned({"form": "scope", "guards": guards, "body": body.node}, tree), body.values)
            diverges = body.diverges
        elif name == "record_constructor":
            fields = []
            for argument in rules(tree, "named_argument"):
                label = rules(argument, "field_label")[0]
                text = label["children"][0]["text"]
                field_name = text[:-1] if text.endswith(":") else text
                value = coforth_body([rules(value, "expression")[0] for value in rules(argument, "field_value")], environment.child(), argument)
                if value.values != 1:
                    raise ElaborationError(f"field {field_name} must leave exactly one value")
                fields.append(cover({"form": "field-init", "name": field_name, "value": value.node, "$span": [label["start"], label["end"]]}, value.node))
            push(spanned({"form": "record-construct", "type": tokens(tree, "identifier")[0]["text"], "fields": fields}, tree))
        elif name == "quotation":
            push(spanned(coforth_callable(tree, environment, None), tree))
        else:
            raise ElaborationError(f"{name} is outside the executable core")
    return finish()


def coforth_local(entry: dict[str, Any]) -> dict[str, Any]:
    labels = tokens(entry, "label")
    name = labels[0]["text"][:-1] if labels else tokens(entry, "identifier")[0]["text"]
    node: dict[str, Any] = {"form": "let", "name": name, "mutable": has_literal(entry, "mut")}
    if rules(entry, "type"):
        node["type"] = canonical_text(rules(entry, "type")[0])
    return node


def coforth_captures(tree: dict[str, Any] | None) -> tuple[str, list[dict[str, Any]]]:
    if tree is None:
        return "borrow", []
    entries = []
    for group in rules(tree, "capture_entries"):
        for entry in rules(group, "capture_entry"):
            entries.append(spanned({"form": "capture", "name": tokens(entry, "identifier")[0]["text"], "ownership": canonical_text(rules(entry, "ownership_mode")[0])}, entry))
    return ("move" if has_literal(tree, "move") else "exact"), entries


def contains_rule(tree: dict[str, Any], name: str, stop: set[str]) -> bool:
    """Whether a parse tree holds rule ``name`` outside any nested rule in ``stop``."""
    for child in tree.get("children", []):
        if "rule" not in child or child["rule"] in stop:
            continue
        if child["rule"] == name or contains_rule(child, name, stop):
            return True
    return False


def coforth_callable(tree: dict[str, Any], environment: ForthEnvironment, kind: str | None) -> dict[str, Any]:
    inputs, outputs, contract = coforth_signature(rules(tree, "stack_signature")[0])
    default, captures = coforth_captures((rules(tree, "capture_spec") or [None])[0])
    result, count = result_type(outputs)
    inner = environment.child(result_count=count, resume_count=None)
    inner.locals |= {capture["name"] for capture in captures}
    if any(entry["name"] is None for entry in inputs):
        raise ElaborationError("unnamed quotation inputs are outside the executable core")
    inner.locals |= {entry["name"] for entry in inputs}
    for entry in inputs:
        shape = callable_shape(entry["type"])
        if shape is not None:
            inner.callables[entry["name"]] = shape
    body = coforth_body(rules(tree, "expression"), inner, tree)
    if not body.diverges and body.values != count:
        raise ElaborationError("quotation body does not match its stack signature")
    node = {"form": "lambda", "capture_default": default, "contract": contract, "captures": captures, "parameters": inputs, "body": body.node}
    environment.shapes[id(node)] = (len(inputs), count)
    return node


def elaborate_coforth(source: str, root: str = "submission", operations: dict[str, Any] | None = None,
                      session: dict[str, tuple[int, int, str]] | None = None) -> dict[str, Any]:
    """``session`` gives the stack effect and result type of each word earlier turns declared."""
    tree = parse("coforth", root, source)
    words: dict[str, tuple[int, int]] = {word: (2, 1) for word in BINARY_WORDS}
    for name, (arity, values, _) in (session or {}).items():
        words[name] = (arity, values)
    # Range operations on a generator.  `reply` carries one value here; a generator with several
    # parameters needs a tuple, which the executable core does not have yet.
    words.update({"empty?": (1, 1), "front": (1, 1), "pop-front": (1, 0), "start": (1, 1), "reply": (2, 1), "some": (1, 1), "none": (0, 1)})
    words["spawn"] = (1, 1)
    words["join"] = (1, 1)
    words["cancel"] = (1, 0)
    for name, operation in (operations or {}).items():
        words[name] = (operation["parameters"], 1 if operation.get("result", True) else 0)
    declarations = []
    expressions = []
    results: dict[str, str] = {name: result for name, (_, _, result) in (session or {}).items()}
    shapes: dict[int, tuple[int, int]] = {}
    variants: list[dict[str, Any]] = []
    order: list[tuple[int, dict[str, Any]]] = []
    for top in rules(tree, "top_level"):
        child = top["children"][0]
        if is_rule(child, "function_decl"):
            if rules(child, "generic_header"):
                raise ElaborationError("generic functions are outside the executable core")
            inputs, outputs, contract = coforth_signature(rules(child, "stack_signature")[0])
            result, count = result_type(outputs)
            name = tokens(child, "identifier")[0]["text"]
            # A word whose own body yields makes a generator when called: one value, whatever it returns.
            generator = contains_rule(child, "yield_expression", stop={"quotation"})
            words[name] = (len(inputs), 1 if generator else count)
            results[name] = "generator" if generator else result
            declarations.append((child, name, inputs, result, count, contract))
        elif is_rule(child, "variant_decl"):
            if rules(child, "generic_header"):
                raise ElaborationError("generic variants are outside the executable core")
            cases = []
            for case in rules(child, "variant_case"):
                if rules(case, "field_decl") or case["alternative"] == 1:
                    raise ElaborationError("record-payload variant cases are outside the executable core")
                payload = [canonical_text(item) for group in rules(case, "type_list") for item in rules(group, "type")]
                case_name = tokens(case, "identifier")[0]["text"]
                words[case_name] = (len(payload), 1)
                cases.append(spanned({"form": "case", "name": case_name, "payload_kind": "tuple" if payload else "unit", "payload": payload}, case))
            variants.append(spanned({"form": "variant", "name": tokens(child, "identifier")[0]["text"], "representation": "native", "cases": cases}, child))
        elif is_rule(child, "expression"):
            expressions.append(child)
        else:
            raise ElaborationError(f"{child['rule']} is outside the executable core")
    items: list[dict[str, Any]] = []
    for child, name, inputs, result, count, contract in declarations:
        if any(entry["name"] is None for entry in inputs):
            raise ElaborationError("unnamed function inputs are outside the executable core")
        environment = ForthEnvironment(words, count)
        environment.results, environment.shapes = results, shapes
        environment.locals = {entry["name"] for entry in inputs}
        for entry in inputs:
            shape = callable_shape(entry["type"])
            if shape is not None:
                environment.callables[entry["name"]] = shape
        body = coforth_body(rules(child, "expression"), environment, child)
        if not body.diverges and body.values != count:
            raise ElaborationError(f"function {name} body does not match its stack signature")
        order.append((child["start"], spanned({"form": "function", "name": name, "result": result, "contract": contract, "parameters": inputs, "body": body.node}, child)))
    order.extend((variant["$span"][0], variant) for variant in variants)
    items.extend(node for _, node in sorted(order, key=lambda entry: entry[0]))
    if expressions:
        top_environment = ForthEnvironment(words)
        top_environment.results, top_environment.shapes = results, shapes
        body = coforth_body(expressions, top_environment, tree)
        items.extend(body.node["items"] if body.node["form"] == "sequence" else [body.node])
    if not items:
        raise ElaborationError("an executable submission needs at least one form")
    return sequence_of(items)


# ----------------------------------------------------------------------------------------- C-like

CLIKE_ATTRIBUTES = {
    "plain": "plain", "inferred": "inferred", "pure": "pure", "nothrow": "nothrow", "suspends": "suspends",
    "nonSuspending": "non-suspending", "effectsInfer": "effects-infer", "throwsInfer": "throws-infer",
    "suspendsInfer": "suspends-infer",
}
CLIKE_GUARDS = {"onExit": "exit", "onSuccess": "success", "onFailure": "failure", "onCancel": "cancel"}
CLIKE_MODES = {"mut": "borrow-mut", "move": "steal"}


def clike_name(tree: dict[str, Any]) -> str:
    """Canonical name: a raw identifier verbatim, an uppercase-initial word verbatim, else camelCase."""
    leaf = tree["children"][0] if "rule" in tree else tree
    text = leaf["text"]
    if leaf["token"] == "raw_identifier":
        return text[1:-1]
    if "A" <= text[0] <= "Z":
        return text
    return re.sub(r"(?<=[a-z0-9])([A-Z])", lambda match: "-" + match.group(1).lower(), text)


def clike_contract(attributes: list[dict[str, Any]]) -> str:
    if not attributes:
        return CONTRACT_SUGAR["inferred"]
    return checked_contract([CLIKE_ATTRIBUTES[attribute["children"][0]["text"]] for attribute in attributes])


def clike_type(tree: dict[str, Any]) -> str:
    """The shared canonical type spelling for a C-like type expression."""
    base = rules(tree, "ctype_base")[0]
    arguments = rules(base, "ctype")
    text = clike_name(rules(base, "name")[0])
    if arguments:
        text += "<" + ",".join(clike_type(argument) for argument in arguments) + ">"
    for brackets in rules(tree, "array_suffix"):
        # One bracket is one array, and each suffix wraps the type to its left.
        dimensions = []
        for dimension in rules(brackets, "dimension"):
            leaf = dimension["children"][0]
            dimensions.append(clike_name(leaf) if "rule" in leaf else leaf["text"])
        if not dimensions:
            text = f"vector<{text}>"
        elif len(dimensions) == 1:
            text = f"array<{text},{dimensions[0]}>"
        else:
            text = f"static-array<{text},{','.join(dimensions)}>"
    suffix = rules(tree, "function_suffix")
    if not suffix:
        return text
    parameters = []
    for parameter in rules(suffix[0], "type_parameter"):
        mode = CLIKE_MODES.get(parameter["children"][0].get("text", ""))
        inner = clike_type(rules(parameter, "ctype")[0])
        parameters.append(f"{mode} {inner}" if mode else inner)
    contract = clike_contract(rules(suffix[0], "attribute")).replace(" | ", "|")
    return f"callable<({','.join(parameters)})->{text}!{contract}>"


def clike_parameter(tree: dict[str, Any]) -> dict[str, Any]:
    mode = CLIKE_MODES.get(tree["children"][0].get("text", ""), "borrow")
    return spanned({"form": "parameter", "name": clike_name(rules(tree, "name")[0]), "type": clike_type(rules(tree, "ctype")[0]), "ownership": mode}, tree)


def clike_captures(tree: dict[str, Any] | None) -> tuple[str, list[dict[str, Any]]]:
    if tree is None:
        return "borrow", []
    default = "exact"
    entries = []
    for capture in rules(tree, "capture"):
        if capture["alternative"] == 0:
            default = "move"
            continue
        mode = canonical_text(rules(capture, "capture_mode")[0])
        entries.append(spanned({"form": "capture", "name": clike_name(rules(capture, "name")[0]), "ownership": CLIKE_MODES.get(mode, mode)}, capture))
    return default, entries


def clike_pattern(tree: dict[str, Any]) -> Any:
    alternative = tree["alternative"]
    if alternative == 0:
        return "_"
    if alternative == 1:
        return clike_expression(tree["children"][0])["value"]
    name = clike_name(rules(tree, "name")[0])
    if alternative == 2:
        return {"constructor": name, "arguments": [clike_pattern(child) for child in rules(tree, "pattern")]}
    if alternative == 3:
        return {"record": name, "fields": [
            {"name": clike_name(rules(field, "name")[0]), "pattern": clike_pattern(rules(field, "pattern")[0])}
            for field in rules(tree, "field_pattern")
        ]}
    return {"bind": name}


def clike_unit(position: int) -> dict[str, Any]:
    return {"form": "literal", "value": None, "$span": [position, position]}


def clike_items(tree: dict[str, Any], anchor: dict[str, Any], top: bool = False) -> list[dict[str, Any]]:
    """Items of a block or submission; a local declaration scopes every item after it.

    A declaration is an item of the submission wherever it is written among the top-level items, so
    one written after a local declaration is placed before that local's scope, not inside it.
    """
    entries = [child for child in tree["children"] if "rule" in child]
    if top:
        first = next((index for index, entry in enumerate(entries) if is_rule(entry, "local_decl")), len(entries))
        later = [entry for entry in entries[first:] if is_rule(entry, "declaration")]
        entries = entries[:first] + later + [entry for entry in entries[first:] if not is_rule(entry, "declaration")]

    def build(index: int) -> list[dict[str, Any]]:
        out: list[dict[str, Any]] = []
        for position in range(index, len(entries)):
            entry = entries[position]
            if is_rule(entry, "local_decl"):
                rest = build(position + 1)
                body = sequence_of(rest) if rest else clike_unit(entry["end"])
                node: dict[str, Any] = {"form": "let", "name": clike_name(rules(entry, "name")[0]), "mutable": has_literal(entry, "mut")}
                local_type = rules(entry, "local_type")[0]
                if rules(local_type, "ctype"):
                    node["type"] = clike_type(rules(local_type, "ctype")[0])
                node["initializer"] = clike_expression(rules(entry, "expression")[0])
                node["body"] = body
                out.append(cover(spanned(node, entry), body))
                return out
            out.append(clike_declaration(entry) if is_rule(entry, "declaration") else clike_expression(entry))
        return out

    return build(0)


def clike_block(tree: dict[str, Any]) -> dict[str, Any]:
    items = clike_items(rules(tree, "items")[0], tree)
    if len(items) == 1:
        return items[0]
    return spanned({"form": "sequence", "items": items}, tree)


def clike_declaration(tree: dict[str, Any]) -> dict[str, Any]:
    inner = tree["children"][0]
    if is_rule(inner, "function_decl"):
        parameters = rules(inner, "parameters")
        return spanned({
            "form": "function", "name": clike_name(rules(inner, "name")[0]), "result": clike_type(rules(inner, "ctype")[0]),
            "contract": clike_contract(rules(inner, "attribute")),
            "parameters": [clike_parameter(parameter) for parameter in rules(parameters[0], "parameter")] if parameters else [],
            "body": clike_block(rules(inner, "block")[0]),
        }, inner)
    cases = []
    for case in rules(inner, "variant_case"):
        payload = [clike_type(item) for item in rules(case, "ctype")]
        cases.append(spanned({"form": "case", "name": clike_name(rules(case, "name")[0]), "payload_kind": "tuple" if payload else "unit", "payload": payload}, case))
    return spanned({"form": "variant", "name": clike_name(rules(inner, "name")[0]), "representation": "native", "cases": cases}, inner)


def clike_binary(tree: dict[str, Any], operand: str, operator: str) -> dict[str, Any]:
    operands = [clike_expression(child) for child in rules(tree, operand)]
    operators = [canonical_text(child) for child in rules(tree, operator)]
    node = operands[0]
    for symbol, right in zip(operators, operands[1:]):
        node = cover({"form": "call", "callee": symbol, "arguments": [node, right]}, node, right)
    return node


def clike_expression(tree: dict[str, Any]) -> dict[str, Any]:
    name = tree["rule"]
    children = tree["children"]
    if name == "expression":
        if children[0].get("token") == "literal":
            form = children[0]["text"]
            return spanned({"form": form, "value": clike_expression(children[1])}, tree)
        return clike_expression(children[0])
    if name == "assignment":
        return spanned({"form": "assign", "place": clike_name(children[0]), "value": clike_expression(children[2])}, tree)
    if name == "comparison":
        return clike_binary(tree, "additive", "comparison_operator")
    if name == "additive":
        return clike_binary(tree, "multiplicative", "additive_operator")
    if name == "multiplicative":
        return clike_binary(tree, "unary", "multiplicative_operator")
    if name == "unary":
        if tree["alternative"] == 0:
            return spanned({"form": "call", "callee": "-", "arguments": [clike_expression(children[1])]}, tree)
        return clike_expression(children[0])
    if name == "postfix":
        primary = children[0]
        node = clike_expression(primary)
        named = is_rule(primary["children"][0], "name")
        for operator in rules(tree, "postfix_operator"):
            inner = operator["children"][0]
            if is_rule(inner, "call_arguments"):
                arguments = [clike_expression(argument) for argument in rules(inner, "expression")]
                callee: Any = node["place"] if named else node
                node = cover(spanned({"form": "call", "callee": callee, "arguments": arguments}, operator), node)
            else:
                node = cover(spanned({"form": "member", "name": clike_name(operator["children"][1]), "target": node}, operator), node)
            named = False
        return node
    if name == "primary":
        first = children[0]
        if first.get("token") == "literal":
            if first["text"] == "(":
                return spanned(dict(clike_expression(children[1])), tree)
            return spanned({"form": {"break": "break", "continue": "continue-loop", "rethrow": "rethrow"}[first["text"]]}, tree)
        if is_rule(first, "name"):
            return spanned({"form": "read", "place": clike_name(first)}, tree)
        return clike_expression(first)
    if name == "literal":
        leaf = children[0]
        if leaf["token"] == "integer":
            value, suffix = integer_value(leaf["text"])
            node = {"form": "literal", "value": value}
            if suffix:
                node["type"] = suffix
            return spanned(node, tree)
        if leaf["token"] == "escaped_string":
            return spanned({"form": "literal", "value": decode_escapes(leaf["text"][1:-1])}, tree)
        return spanned({"form": "literal", "value": leaf["text"] == "true"}, tree)
    if name == "unit":
        return spanned({"form": "literal", "value": None}, tree)
    if name == "block":
        return clike_block(tree)
    if name == "if_expression":
        parts = [clike_expression(child) for child in rules(tree, "expression")]
        otherwise = parts[2] if len(parts) == 3 else clike_unit(tree["end"])
        return spanned({"form": "if", "condition": parts[0], "then": parts[1], "else": otherwise}, tree)
    if name == "while_expression":
        condition, body = [clike_expression(child) for child in rules(tree, "expression")]
        return spanned({"form": "while", "condition": condition, "body": body}, tree)
    if name == "match_expression":
        arms = [
            spanned({"form": "match-arm", "pattern": clike_pattern(rules(arm, "pattern")[0]), "body": clike_expression(rules(arm, "expression")[0])}, arm)
            for arm in rules(tree, "match_arm")
        ]
        return spanned({
            "form": "match", "ownership": "steal" if has_literal(tree, "move") else "borrow",
            "scrutinee": clike_expression(rules(tree, "expression")[0]), "arms": arms,
        }, tree)
    if name == "try_expression":
        catches = [
            spanned({"form": "catch", "pattern": clike_pattern(rules(clause, "pattern")[0]), "body": clike_expression(rules(clause, "expression")[0])}, clause)
            for clause in rules(tree, "catch_clause")
        ]
        return spanned({"form": "try", "body": clike_expression(rules(tree, "expression")[0]), "catches": catches}, tree)
    if name == "scope_expression":
        guards = [
            spanned({"form": "guard", "reason": CLIKE_GUARDS[canonical_text(rules(guard, "guard_word")[0])], "body": clike_expression(rules(guard, "expression")[0])}, guard)
            for guard in rules(tree, "guard")
        ]
        items = clike_items(rules(tree, "items")[0], tree)
        return spanned({"form": "scope", "guards": guards, "body": sequence_of(items) if items else clike_unit(tree["end"])}, tree)
    if name == "lambda":
        default, captures = clike_captures((rules(tree, "capture_list") or [None])[0])
        parameters = rules(tree, "parameters")
        return spanned({
            "form": "lambda", "capture_default": default, "contract": clike_contract(rules(tree, "attribute")), "captures": captures,
            "parameters": [clike_parameter(parameter) for parameter in rules(parameters[0], "parameter")] if parameters else [],
            "body": clike_expression(rules(tree, "expression")[0]),
        }, tree)
    if name == "record_construct":
        fields = [
            spanned({"form": "field-init", "name": clike_name(rules(field, "name")[0]), "value": clike_expression(rules(field, "expression")[0])}, field)
            for field in rules(tree, "field_init")
        ]
        return spanned({"form": "record-construct", "type": clike_name(rules(tree, "name")[0]), "fields": fields}, tree)
    raise ElaborationError(f"{name} is outside the executable core")


def elaborate_clike(source: str, root: str = "submission") -> dict[str, Any]:
    tree = parse("clike", root, source)
    items = clike_items(rules(tree, "items")[0], tree, top=True)
    if not items:
        raise ElaborationError("an executable submission needs at least one form")
    return sequence_of(items)


def elaborate(syntax: str, source: str, operations: dict[str, Any] | None = None,
              session: dict[str, tuple[int, int, str]] | None = None) -> dict[str, Any]:
    if syntax == "colisp":
        return elaborate_colisp(source)
    if syntax == "coforth":
        return elaborate_coforth(source, operations=operations, session=session)
    if syntax == "clike":
        return elaborate_clike(source)
    raise ElaborationError(f"unknown syntax {syntax!r}")
