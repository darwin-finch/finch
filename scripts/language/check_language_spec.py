#!/usr/bin/env python3
"""Validate the executable Finch language specification artifacts."""

from __future__ import annotations

import argparse
import copy
import hashlib
import json
import re
import subprocess
import sys
import unicodedata
from pathlib import Path
from typing import Any

from jsonschema import Draft202012Validator

from elaborate import CHILD_LISTS, ElaborationError, elaborate, strip_spans
from grammar_engine import GrammarError, ReaderError, load_grammar, parse, rules_used
from reference_machine import Machine, MachineError, execute, prepare
from admission import manifest_of_ir, manifest_of_program
from compile_scheduler import run_case, schedules
from ir_lower import LoweringGap, lower
from ir_machine import direct_comparable, direct_projection, execute_ir
from ir_verify import verify_ir
from session import Session
from static_check import StaticError, static_check
from signature_parser import validate_prelude


ROOT = Path(__file__).resolve().parents[2]
LANG = ROOT / "docs/language"
VECTORS = LANG / "fixtures/execution-vectors.json"
SYNTAXES = (("colisp", "lisp"), ("coforth", "forth"), ("clike", "c"))
SPECIAL_TOKENS = {"delimiter", "word_extra"}  # used by the word-boundary rule, not by a production
SCHEMA_FIXTURES = LANG / "fixtures/schema-instances.json"


def strict_json_loads(text: str, source: str) -> Any:
    def strict_object(pairs: list[tuple[str, Any]]) -> dict[str, Any]:
        result: dict[str, Any] = {}
        normalized_keys: dict[str, str] = {}
        for key, value in pairs:
            if key in result:
                raise ValueError(f"duplicate JSON object key {key!r} in {source}")
            canonical = unicodedata.normalize("NFC", key)
            previous = normalized_keys.get(canonical)
            if previous is not None:
                raise ValueError(f"JSON keys {previous!r} and {key!r} collide after NFC in {source}")
            if canonical != key:
                raise ValueError(f"non-NFC JSON object key {key!r} in {source}")
            normalized_keys[canonical] = key
            result[key] = value
        return result

    return json.loads(text, object_pairs_hook=strict_object)


def load(relative: str) -> Any:
    return strict_json_loads((LANG / relative).read_text(), relative)


def normalized(value: Any) -> Any:
    if isinstance(value, str):
        if any(0xD800 <= ord(character) <= 0xDFFF for character in value):
            raise ValueError("canonical JSON contains a surrogate code point")
        canonical = unicodedata.normalize("NFC", value)
        if canonical != value:
            raise ValueError(f"canonical JSON string is not NFC: {value!r}")
        return value
    if isinstance(value, list):
        return [normalized(item) for item in value]
    if isinstance(value, dict):
        return {normalized(key): normalized(item) for key, item in value.items()}
    return value


def canonical_bytes(value: Any) -> bytes:
    return json.dumps(normalized(value), ensure_ascii=False, separators=(",", ":"), sort_keys=True).encode()


def event_children(node: dict[str, Any]) -> list[tuple[str, int | None, dict[str, Any]]]:
    """Child edges of a normalized AST node, in canonical field order."""
    children: list[tuple[str, int | None, dict[str, Any]]] = []
    for key, value in node.items():
        if isinstance(value, dict) and "form" in value:
            children.append((key, None, value))
        elif key in CHILD_LISTS and isinstance(value, list):
            children.extend((key, index, item) for index, item in enumerate(value))
    return children


def semantic_events(identity: str, language: str, source: str, ast: dict[str, Any]) -> dict[str, Any]:
    """Preorder semantic-construction events for one reader's span-carrying AST."""
    events: list[dict[str, Any]] = []
    source_bytes = source.encode("utf-8")

    def visit(node: dict[str, Any], parent: str | None, role: str | None, index: int | None) -> None:
        node_id = f"n{len(events)}"
        child_keys = {key for key, _, _ in event_children(node)}
        attributes = {key: value for key, value in node.items() if key not in {"form", "$span"} and key not in child_keys and key not in CHILD_LISTS}
        start, end = node["$span"]
        events.append({
            "sequence": len(events),
            "kind": {"let": "binding", "assign": "write"}.get(node["form"], node["form"]),
            "origin": {"span": {"source": identity, "start": start, "end": end}, "expansion": []},
            "payload": {"node_id": node_id, "parent_id": parent, "role": role, "index": index, "attributes": strip_spans(attributes)},
        })
        for key, child_index, child in event_children(node):
            visit(child, node_id, key, child_index)

    visit(ast, None, None, None)
    return {
        "schema_version": 1,
        "language_version": "0.1-draft",
        "source": {
            "identity": identity,
            "language": language,
            "sha256": hashlib.sha256(source_bytes).hexdigest(),
            "byte_length": len(source_bytes),
        },
        "events": events,
    }


def semantic_digest(stream: dict[str, Any]) -> str:
    """Digest of an event stream with every frontend-specific fact removed."""
    stream = copy.deepcopy(stream)
    stream["source"].pop("language")
    stream["source"].pop("sha256")
    stream["source"].pop("byte_length")
    for event in stream["events"]:
        event.pop("origin")
    domain = load("semantics/canonical-digests.json")["domains"]["semantic"]
    tag = domain["tag"].encode() + bytes.fromhex(domain["tag_terminator_hex"])
    return hashlib.sha256(tag + canonical_bytes(stream)).hexdigest()


def validate_grammar_terminals(grammar: dict[str, Any], inherited_reserved: set[str] | None = None) -> list[str]:
    del inherited_reserved  # Contextual terminals imported from common are intentionally not reserved.
    quoted_terminals: set[str] = set()
    for production in grammar.get("productions", {}).values():
        for alternative in production.get("alternatives", []):
            quoted_terminals.update(re.findall(r"'([^']+)'", alternative))
    return [
        f"grammar/{grammar.get('syntax', '<unknown>')}.json reserves nonterminal spelling {word!r}"
        for word in sorted(set(grammar.get("reserved_words", [])) - quoted_terminals)
    ]


def validate_root_entrypoints(grammar: dict[str, Any]) -> list[str]:
    if grammar.get("syntax") == "common":
        return []
    expected = {"library": "module", "executable": "module", "submission": "submission", "script": "submission"}
    if grammar.get("root_entrypoints") != expected:
        return [f"grammar/{grammar.get('syntax', '<unknown>')}.json does not select canonical root entrypoints"]
    return [
        f"grammar/{grammar['syntax']}.json root {root!r} references missing production {production!r}"
        for root, production in grammar["root_entrypoints"].items()
        if production not in grammar.get("productions", {})
    ]


def check_grammars(errors: list[str]) -> None:
    common = load("grammar/common.json")
    grammars = {name: load(f"grammar/{name}.json") for name in ("colisp", "coforth", "clike")}
    for name, grammar in {"common": common, **grammars}.items():
        for required in ("schema_version", "language_version", "syntax", "entrypoint", "rule_id_template", "tokens", "productions", "reserved_words"):
            if required not in grammar:
                errors.append(f"grammar/{name}.json missing {required}")
        if grammar.get("entrypoint") not in grammar.get("productions", {}):
            errors.append(f"grammar/{name}.json entrypoint is not a production")
        rule_ids = {
            grammar.get("rule_id_template", "").format(production=production.upper().replace("_", "-"))
            for production in grammar.get("productions", {})
        }
        if len(rule_ids) != len(grammar.get("productions", {})):
            errors.append(f"grammar/{name}.json does not derive unique production rule IDs")
        if len(grammar.get("reserved_words", [])) != len(set(grammar.get("reserved_words", []))):
            errors.append(f"grammar/{name}.json has duplicate reserved words")
        visible = set(common["tokens"]) | set(common["productions"]) | set(grammar.get("tokens", {})) | set(grammar.get("productions", {}))
        for production, definition in grammar.get("productions", {}).items():
            for alternative in definition["alternatives"]:
                unquoted = re.sub(r"'[^']*'|\"[^\"]*\"", "", alternative)
                references = set(re.findall(r"\b[a-z][a-z0-9_]*\b", unquoted))
                for reference in sorted(references - visible):
                    errors.append(f"grammar/{name}.json production {production} references unknown symbol {reference}")
        inherited = set() if name == "common" else set(common["reserved_words"])
        errors.extend(validate_grammar_terminals(grammar, inherited))
        errors.extend(validate_root_entrypoints(grammar))
    prelude = load("spec-prelude.json")
    for error in validate_prelude(load("prelude-definitions.json")):
        errors.append(f"generated prelude signature: {error}")
    operation_names: set[str] = set()
    for operation in prelude["operations"]:
        name = operation["name"]
        if name in operation_names:
            errors.append(f"generated prelude has duplicate operation {name}")
        operation_names.add(name)
        signature = operation["signature"]
        if " ! " not in signature:
            errors.append(f"generated prelude operation {name} has no callable contract")
        contract = signature.split(" ! ", 1)[1] if " ! " in signature else ""
        if not any(item in contract for item in ("pure", "effects<", "effects-infer", "state<", "state-read<", "comptime", "{")):
            errors.append(f"generated prelude operation {name} has no effect-row axis")
        if not any(item in signature for item in ("nothrow", "throws<", "throws-infer")):
            errors.append(f"generated prelude operation {name} has no exception axis")
        if not any(item in signature for item in ("non-suspending", "suspends", "suspends-infer")):
            errors.append(f"generated prelude operation {name} has no suspension axis")
    for form in prelude["core_forms"]:
        for syntax in ("colisp", "coforth"):
            production = form[syntax]
            if production not in grammars[syntax]["productions"]:
                errors.append(f"prelude form {form['name']} references missing {syntax} production {production}")


def validate_transition_programs(rules: dict[str, Any]) -> list[str]:
    errors: list[str] = []
    instruction_vocabulary = set(rules.get("machine", {}).get("instruction_vocabulary", []))
    if not instruction_vocabulary:
        errors.append("semantics/transitions.json has no instruction vocabulary")
    for rule in rules.get("rules", []):
        for position, instruction in enumerate(rule.get("program", [])):
            operation = instruction.get("op")
            if operation not in instruction_vocabulary:
                errors.append(f"transition rule {rule['rule']} instruction {position} has unknown operation {operation!r}")
            elif not hasattr(Machine, f"op_{operation}"):
                errors.append(f"transition rule {rule['rule']} instruction {position} operation {operation!r} has no reference definition")
        labels = [instruction["label"] for instruction in rule.get("program", []) if "label" in instruction]
        if len(labels) != len(set(labels)):
            errors.append(f"transition rule {rule['rule']} repeats a label")
        for instruction in rule.get("program", []):
            for key in ("target", "false", "miss", "intrinsic"):
                if instruction["op"] in {"jump", "branch_loop", "branch_catch", "branch_callable"} and key in instruction and instruction[key] not in labels:
                    errors.append(f"transition rule {rule['rule']} jumps to undefined label {instruction[key]!r}")
    used = {instruction.get("op") for rule in rules.get("rules", []) for instruction in rule.get("program", [])}
    for unused in sorted(instruction_vocabulary - used):
        errors.append(f"semantics/transitions.json declares instruction {unused!r} that no rule program uses")
    return errors


def execute_replay_automaton(automaton: dict[str, Any], initial: dict[str, Any], event: dict[str, Any]) -> dict[str, Any]:
    state = copy.deepcopy(initial)
    existing = state["requests"].get(event["request_id"])
    event_record = {
        "generation": event["generation"], "sequence": event["sequence"],
        "kind": event["kind"], "fingerprint": event["fingerprint"],
    }
    predicates = {
        "terminal": lambda: state["terminal"],
        "generation-mismatch": lambda: event["generation"] != state["generation"],
        "request-exact-match": lambda: existing == event_record,
        "request-id-conflict": lambda: existing is not None and existing != event_record,
        "sequence-mismatch": lambda: event["sequence"] != state["next_sequence"],
        "active": lambda: not state["terminal"],
        "generation-match": lambda: event["generation"] == state["generation"],
        "sequence-match": lambda: event["sequence"] == state["next_sequence"],
        "request-new": lambda: existing is None,
        "resume-unsolicited": lambda: event["kind"] == "resume" and state.get("outstanding") != event["request_id"],
    }
    outputs = {
        "reject-post-terminal": "rejected-post-terminal",
        "reject-stale-generation": "rejected-stale-generation",
        "replay-recorded-ack-without-dispatch": "replayed-no-dispatch",
        "reject-conflicting-request": "rejected-conflicting-request",
        "reject-out-of-order": "rejected-out-of-order",
        "reject-unsolicited-resume": "rejected-unsolicited-resume",
        "ack-then-dispatch-once": "accepted-dispatch-once",
    }
    known_actions = set(outputs) | {"durably-record-request", "increment-next-sequence"}
    for rule in automaton["rules"]:
        unknown_predicates = set(rule["when"]) - predicates.keys()
        unknown_actions = set(rule["actions"]) - known_actions
        if unknown_predicates or unknown_actions:
            raise ValueError(f"unknown replay program words: predicates={sorted(unknown_predicates)} actions={sorted(unknown_actions)}")
        if not all(predicates[predicate]() for predicate in rule["when"]):
            continue
        output = None
        for action in rule["actions"]:
            if action == "durably-record-request":
                state["requests"][event["request_id"]] = event_record
            elif action == "increment-next-sequence":
                state["next_sequence"] = f"{int(state['next_sequence'], 16) + 1:016x}"
            else:
                output = outputs[action]
        if output is None:
            raise ValueError(f"replay rule {rule['rule']} produced no outcome")
        return {"rule": rule["rule"], "output": output, "state": state}
    raise ValueError("replay automaton has no matching rule")


def check_replay_automaton(errors: list[str]) -> None:
    automaton = load("semantics/replay-automaton.json")
    seen_rules: set[str] = set()
    for rule in automaton["rules"]:
        if rule["rule"] in seen_rules:
            errors.append(f"duplicate replay rule {rule['rule']}")
        seen_rules.add(rule["rule"])
    for case in load("fixtures/replay-transitions.json")["cases"]:
        try:
            actual = execute_replay_automaton(automaton, case["initial"], case["event"])
        except (KeyError, TypeError, ValueError) as error:
            errors.append(f"replay fixture {case['id']} failed: {error}")
            continue
        if (actual["rule"], actual["output"], actual["state"]["next_sequence"]) != (
            case["expected_rule"], case["expected_output"], case["expected_next_sequence"]
        ):
            errors.append(f"replay fixture {case['id']} mismatch: {actual!r}")


def validate_performance_budgets(budgets: dict[str, Any]) -> list[str]:
    errors: list[str] = []
    required_manifest = {
        "cpu_model", "logical_cores", "memory_bytes", "os", "filesystem", "power_mode",
        "compiler_revision", "optimization_profile", "corpus_digest", "sample_count", "warmup_count",
    }
    if set(budgets.get("measurement_manifest_required", [])) != required_manifest:
        errors.append("performance budget measurement manifest fields are incomplete")
    for name, value in budgets.get("compile", {}).items():
        if not isinstance(value, (int, float)) or value <= 0:
            errors.append(f"performance compile budget {name} is not positive")
    for name, value in budgets.get("runtime_semantic_taxes", {}).items():
        if value != 0:
            errors.append(f"semantic zero-tax budget {name} must remain exactly zero")
    if budgets.get("regression_policy", {}).get("minimum_samples", 0) < 30:
        errors.append("performance regression policy requires fewer than 30 samples")
    return errors


def vector_trees(vector: dict[str, Any]) -> dict[str, dict[str, Any]]:
    """Read each spelling of a vector with its own frontend; the result keeps byte spans."""
    operations = vector.get("context", {}).get("operations")
    return {
        syntax: elaborate(syntax, vector[syntax], operations, vector.get("$session"))
        for syntax, _ in SYNTAXES if vector.get(syntax) is not None
    }


def vector_streams(vector: dict[str, Any], trees: dict[str, dict[str, Any]]) -> dict[str, dict[str, Any]]:
    identity = f"fixture:{vector['id']}"
    return {syntax: semantic_events(identity, language, vector[syntax], trees[syntax]) for syntax, language in SYNTAXES if syntax in trees}


def replay_acceptor() -> Any:
    automaton = load("semantics/replay-automaton.json")
    return lambda state, event: execute_replay_automaton(automaton, state, event)


def check_vector_frontends(vector: dict[str, Any], errors: list[str], write: bool, rejection: str | None = None) -> dict[str, Any] | None:
    """Every reader must build the stored AST and one digest from its own bytes and spans.

    ``rejection`` is the diagnostic a rejected program is expected to carry.  A postfix reader may
    report it during construction, where the other readers build a tree that is rejected later.
    """
    name = vector["id"]
    if vector.get("coforth") is None and not vector.get("unpaired_reason"):
        errors.append(f"vector {name} has no Co-Forth spelling and no unpaired_reason")
    for required in ("colisp", "clike"):
        if vector.get(required) is None:
            errors.append(f"vector {name} has no {required} spelling")
    operations = vector.get("context", {}).get("operations")
    trees: dict[str, dict[str, Any]] = {}
    for syntax, _ in SYNTAXES:
        if vector.get(syntax) is None:
            continue
        try:
            trees[syntax] = elaborate(syntax, vector[syntax], operations, vector.get("$session"))
        except ReaderError as error:
            errors.append(f"vector {name} {syntax} was not read: {error}")
            return None
        except ElaborationError as error:
            if error.code is None or error.code != rejection:
                errors.append(f"vector {name} {syntax} was not constructed: {error}")
                return None
    if "colisp" not in trees:
        return None
    asts = {syntax: strip_spans(tree) for syntax, tree in trees.items()}
    if write:
        vector["ast"] = asts["colisp"]
    for syntax, ast in asts.items():
        if ast != vector.get("ast"):
            errors.append(f"vector {name} {syntax} AST mismatch: expected={vector.get('ast')!r} actual={ast!r}")
    digests = {syntax: semantic_digest(stream) for syntax, stream in vector_streams(vector, trees).items()}
    if len(set(digests.values())) != 1:
        errors.append(f"vector {name} readers disagree on the semantic digest: {digests!r}")
    if write:
        vector["semantic_digest"] = digests["colisp"]
    if vector.get("semantic_digest") != digests["colisp"]:
        errors.append(f"vector {name} semantic digest is stale")
    return asts["colisp"]


def check_rules_and_fixtures(errors: list[str], write: bool) -> None:
    rules = load("semantics/transitions.json")
    rule_ids = [rule["rule"] for rule in rules["rules"]]
    forms = [rule["form"] for rule in rules["rules"]]
    if len(rule_ids) != len(set(rule_ids)) or len(forms) != len(set(forms)):
        errors.append("semantics/transitions.json has duplicate rule IDs or forms")
    errors.extend(validate_transition_programs(rules))
    replay = replay_acceptor()
    document = load("fixtures/execution-vectors.json")
    seen: set[str] = set()
    hits: set[tuple[str, str]] = set()
    instructions: set[tuple[str, int]] = set()
    differential: list[str] = []
    direct_vectors: list[str] = []
    ir_specification = load("semantics/ir.json")
    ir_operations: set[str] = set()
    for vector in document["vectors"]:
        name = vector["id"]
        if name in seen:
            errors.append(f"duplicate execution vector ID {name}")
        seen.add(name)
        ast = check_vector_frontends(vector, errors, write)
        if ast is None:
            continue
        try:
            program = prepare(ast, vector.get("context"))
            ownership = static_check(program)
            outcome, machine = execute(rules, ast, vector.get("context"), replay, program)
        except StaticError as error:
            errors.append(f"vector {name} is rejected by the static ownership pass: {error.code}: {error}")
            continue
        except MachineError as error:
            errors.append(f"vector {name} is not a valid program: {error}")
            continue
        for read in ownership.reads:
            observed = machine.read_modes.get(id(read))
            if observed is not None and observed != {read["static_mode"]}:
                errors.append(
                    f"vector {name}: read of {read['place']!r} was decided {read['static_mode']} statically "
                    f"but executed as {sorted(observed)}"
                )
        hits |= machine.hits
        instructions |= machine.instructions_run
        actual = {
            "manifest": manifest_of_program(program),
            "spec_rules": sorted({rule for rule, _ in machine.hits}),
            "transitions": outcome["trace"],
            "terminal": outcome["terminal"],
            "state": outcome["state"],
        }
        for key, value in actual.items():
            if write:
                vector[key] = value
            if vector.get(key) != value:
                errors.append(f"vector {name} {key} mismatch: expected={vector.get(key)!r} actual={value!r}")
        vector.pop("ir", None)
        try:
            module = lower(program, ownership)
        except LoweringGap as error:
            errors.append(f"vector {name} has no IR: {error}")
            continue
        for error in verify_ir(module, ir_specification):
            errors.append(f"vector {name} IR verification failed: {error}")
        if manifest_of_ir(module, {}) != actual["manifest"]:
            errors.append(
                f"vector {name}: the manifest read from IR differs from the program's: "
                f"program={actual['manifest']!r} ir={manifest_of_ir(module, {})!r}"
            )
        for function in module["functions"].values():
            for block in function["blocks"]:
                ir_operations.update(instruction["op"] for instruction in block["instructions"])
        try:
            ir_outcome = execute_ir(module, program, vector.get("context"), replay)
        except (MachineError, KeyError, IndexError, TypeError) as error:
            errors.append(f"vector {name} IR execution failed: {type(error).__name__}: {error}")
            continue
        for key in ("observable", "terminal", "state"):
            if ir_outcome[key] != outcome[key]:
                errors.append(
                    f"vector {name}: IR {key} differs from the rule machine: "
                    f"machine={outcome[key]!r} ir={ir_outcome[key]!r}"
                )
        providers = direct_comparable(vector.get("context"), outcome)
        if providers is not None:
            direct = execute_ir(module, program, vector.get("context"), replay, "direct", providers)
            expected_direct = {
                "observable": direct_projection(outcome["observable"]), "terminal": outcome["terminal"],
                "drops": outcome["state"]["drops"], "reaper": outcome["state"]["reaper"], "max_frames": outcome["state"]["max_frames"],
            }
            actual_direct = {
                "observable": direct["observable"], "terminal": direct["terminal"],
                "drops": direct["state"]["drops"], "reaper": direct["state"]["reaper"], "max_frames": direct["state"]["max_frames"],
            }
            if actual_direct != expected_direct:
                errors.append(f"vector {name}: the direct binding differs from the mediated run: mediated={expected_direct!r} direct={actual_direct!r}")
            if direct["state"]["journal"] or direct["state"]["host_log"]:
                errors.append(f"vector {name}: the direct binding kept a journal or host log")
            direct_vectors.append(name)
        digest = hashlib.sha256(canonical_bytes(module)).hexdigest()
        if write:
            vector["ir_digest"] = digest
        if vector.get("ir_digest") != digest:
            errors.append(f"vector {name} IR digest is stale; the lowering of its program changed")
        differential.append(name)
    if write:
        VECTORS.write_text(json.dumps(document, ensure_ascii=False, indent=2) + "\n")
    for instruction in ir_specification["instructions"]:
        if instruction["op"] not in ir_operations:
            errors.append(f"semantics/ir.json instruction {instruction['op']} is produced by no execution vector")
    check_static_rejections(rules, replay, errors, write)
    check_transition_coverage(rules, hits, instructions, differential, errors, write, direct_vectors)


def check_static_rejections(rules: dict[str, Any], replay: Any, errors: list[str], write: bool) -> None:
    """Programs the reference machine can only reach if a compiler failed to reject them."""
    document = load("fixtures/static-rejections.json")
    seen: set[str] = set()
    for vector in document["vectors"]:
        name = vector["id"]
        if name in seen:
            errors.append(f"duplicate static rejection ID {name}")
        seen.add(name)
        ast = check_vector_frontends(vector, errors, write, vector["code"])
        if ast is None:
            continue
        try:
            static_check(prepare(ast, vector.get("context")))
        except MachineError as error:
            if error.code != vector["code"]:
                errors.append(f"static rejection {name}: the static pass reports {error.code}, expected {vector['code']}: {error}")
        else:
            errors.append(f"static rejection {name} is accepted by the static pass; it must be rejected before execution")
        try:
            execute(rules, ast, vector.get("context"), replay)
        except MachineError as error:
            if error.code != vector["code"]:
                errors.append(f"static rejection {name} reports {error.code}, expected {vector['code']}: {error}")
        else:
            errors.append(f"static rejection {name} executed to a terminal instead of being rejected")
    if write:
        (LANG / "fixtures/static-rejections.json").write_text(json.dumps(document, ensure_ascii=False, indent=2) + "\n")


def check_transition_coverage(
    rules: dict[str, Any], hits: set[tuple[str, str]], instructions: set[tuple[str, int]], differential: list[str], errors: list[str], write: bool,
    direct_vectors: list[str] | None = None,
) -> None:
    """A rule is executable only when vectors ran every instruction and every declared branch of it."""
    path = LANG / "semantics/transition-coverage.json"
    coverage = load("semantics/transition-coverage.json")
    executable: list[str] = []
    pending: dict[str, str] = {}
    for rule in rules["rules"]:
        name = rule["rule"]
        declared = set(rule["branches"])
        observed = {label for owner, label in hits if owner == name}
        if observed - declared:
            errors.append(f"transition rule {name} took undeclared branches {sorted(observed - declared)}")
        missing_instructions = [index for index in range(len(rule["program"])) if (name, index) not in instructions]
        if declared - observed:
            pending[name] = f"no vector takes branches {sorted(declared - observed)}"
        elif missing_instructions:
            pending[name] = f"no vector runs program instructions {missing_instructions}"
        else:
            executable.append(name)
    computed = {"executable_rules": sorted(executable), "pending_rules": pending, "ir_differential_vectors": differential}
    if direct_vectors is not None:
        computed["direct_binding_vectors"] = direct_vectors
    for key, value in computed.items():
        if write:
            coverage[key] = value
        if coverage.get(key) != value:
            errors.append(f"semantics/transition-coverage.json {key} is stale: expected={value!r}")
    if write:
        path.write_text(json.dumps(coverage, ensure_ascii=False, indent=2) + "\n")


def check_sessions(errors: list[str], write: bool) -> None:
    """Run each session's turns in order; a turn commits its declarations only when it completes."""
    rules = load("semantics/transitions.json")
    ir_specification = load("semantics/ir.json")
    replay = replay_acceptor()
    path = LANG / "fixtures/session-vectors.json"
    document = load("fixtures/session-vectors.json")
    seen: set[str] = set()
    for case in document["sessions"]:
        if case["id"] in seen:
            errors.append(f"duplicate session ID {case['id']}")
        seen.add(case["id"])
        session = Session()
        for number, turn in enumerate(case["turns"], start=1):
            turn["id"] = f"{case['id']}#{number}"
            context = {**case.get("context", {}), **turn.get("context", {})}
            turn_view = {**turn, "context": context, "$session": session.words()}
            expected_terminal = turn.get("expected", {}).get("terminal", {})
            ast = check_vector_frontends(turn_view, errors, write, expected_terminal.get("code") or "F-DIAG-UNBOUND-NAME")
            for key in ("ast", "semantic_digest"):
                if key in turn_view:
                    turn[key] = turn_view[key]
            if ast is None:
                break
            outcome: dict[str, Any]
            try:
                linked, declarations, declared = session.prepare(ast)
                program = prepare(linked, context)
                ownership = static_check(program)
                result, _ = execute(rules, linked, context, replay, program)
                module = lower(program, ownership)
                for error in verify_ir(module, ir_specification):
                    errors.append(f"session turn {turn['id']} IR verification failed: {error}")
                ir_result = execute_ir(module, program, context, replay)
                for key in ("observable", "terminal", "state"):
                    if ir_result[key] != result[key]:
                        errors.append(f"session turn {turn['id']}: IR {key} differs from the rule machine")
                committed = result["terminal"]["kind"] == "complete"
                if committed:
                    session.commit(declarations, declared)
                outcome = {"terminal": result["terminal"], "observable": result["observable"], "committed": committed}
            except MachineError as error:
                outcome = {"terminal": {"kind": "rejected", "code": error.code}, "observable": [], "committed": False}
            outcome["visible"] = dict(sorted(session.visible.items()))
            if write:
                turn["expected"] = outcome
            if turn.get("expected") != outcome:
                errors.append(f"session turn {turn['id']} mismatch: expected={turn.get('expected')!r} actual={outcome!r}")
    if write:
        path.write_text(json.dumps(document, ensure_ascii=False, indent=2) + "\n")


def check_compile_scheduler(errors: list[str]) -> None:
    """Every scheduler case has one outcome under every scheduling order, and it is the stated one."""
    specification = load("semantics/compile-scheduler.json")
    kinds = [entry["kind"] for entry in specification["requirements"]]
    if len(kinds) != len(set(kinds)):
        errors.append("semantics/compile-scheduler.json repeats a requirement kind")
    for entry in specification["requirements"]:
        for key in ("issued_while_reaching", "requires"):
            if entry[key] not in specification["states"]:
                errors.append(f"compile-scheduler requirement {entry['kind']} names unknown state {entry[key]!r}")
        if entry["records"] not in specification["dependency_classes"]:
            errors.append(f"compile-scheduler requirement {entry['kind']} records unknown dependency class {entry['records']!r}")
    used: set[str] = set()
    seen: set[str] = set()
    for case in load("fixtures/compile-scheduler.json")["cases"]:
        name = case["id"]
        if name in seen:
            errors.append(f"duplicate compile-scheduler case {name}")
        seen.add(name)
        for symbol in case["symbols"].values():
            used.update(requirement["kind"] for requirement in symbol.get("requires", []))
        outcomes = []
        for schedule in schedules():
            try:
                outcomes.append((schedule, run_case(specification, case, **schedule)))
            except (KeyError, ValueError) as error:
                errors.append(f"compile-scheduler case {name} failed under {schedule}: {error}")
        for schedule, outcome in outcomes:
            if outcome != case["expected"]:
                errors.append(f"compile-scheduler case {name} under {schedule} gives {outcome!r}, expected {case['expected']!r}")
                break
    for kind in kinds:
        if kind not in used:
            errors.append(f"compile-scheduler requirement kind {kind} is exercised by no case")


def check_grammar_corpus(errors: list[str]) -> None:
    """Execute both normative grammars over the accept/reject corpus and require full production use."""
    used: dict[str, set[str]] = {"colisp": set(), "coforth": set(), "clike": set()}
    seen: set[str] = set()
    for case in load("fixtures/grammar-corpus.json")["cases"]:
        name = case["id"]
        if name in seen:
            errors.append(f"duplicate grammar corpus ID {name}")
        seen.add(name)
        source: str | bytes = bytes.fromhex(case["source_hex"]) if "source_hex" in case else case["source"]
        try:
            tree = parse(case["syntax"], case["root"], source, case.get("production"))
        except ReaderError as error:
            if case["accept"]:
                errors.append(f"grammar corpus {name} was rejected: {error}")
            elif (error.code, error.offset) != (case["code"], case["offset"]):
                errors.append(f"grammar corpus {name} failed with {error.code}@{error.offset}, expected {case['code']}@{case['offset']}")
            continue
        if not case["accept"]:
            errors.append(f"grammar corpus {name} was accepted but must be rejected")
        used[case["syntax"]] |= rules_used(tree)
    for document, key in (("fixtures/execution-vectors.json", "vectors"), ("fixtures/static-rejections.json", "vectors")):
        for vector in load(document)[key]:
            for syntax in used:
                if vector.get(syntax) is not None:
                    try:
                        used[syntax] |= rules_used(parse(syntax, "submission", vector[syntax]))
                    except ReaderError:
                        pass
    lookahead_only = {"const_operator"}
    placeholders = {"expression_placeholder"}  # common's where-clause body, replaced by every frontend's expression
    for syntax, productions in used.items():
        grammar = load_grammar(syntax)
        own = set(grammar.raw["productions"])
        unreachable = set(grammar.unreachable_productions(sorted(set(grammar.root_entrypoints.values()))))
        for production in sorted(unreachable & own - {"source"}):
            errors.append(f"grammar/{syntax}.json production {production} is unreachable from every envelope root")
        for production in sorted(set(grammar.productions) - unreachable - productions - lookahead_only):
            errors.append(f"grammar/{syntax}.json production {production} is exercised by no corpus case")
    all_referenced: set[str] = set()
    for syntax in used:
        grammar = load_grammar(syntax)
        referenced: set[str] = set()

        def symbols(node: tuple) -> None:
            if node[0] == "sym":
                referenced.add(node[1])
            elif node[0] in {"seq", "alt"}:
                for child in node[1]:
                    symbols(child)
            elif node[0] != "lit":
                symbols(node[1])

        for alternatives in grammar.productions.values():
            for alternative in alternatives:
                symbols(alternative)
        all_referenced |= referenced
        for token, definition in grammar.raw["tokens"].items():
            if token not in referenced and not definition.get("skip") and token not in SPECIAL_TOKENS:
                errors.append(f"grammar/{syntax}.json token {token} is referenced by no production")
        for token, definition in grammar.tokens.items():
            if "precedence" in definition:
                errors.append(f"grammar/{syntax}.json token {token} declares a precedence, which Finch-PEG-1 does not use")
    for token, definition in load("grammar/common.json")["tokens"].items():
        if token not in all_referenced and not definition.get("skip") and token not in SPECIAL_TOKENS:
            errors.append(f"grammar/common.json token {token} is referenced by no frontend")
    common = set(load("grammar/common.json")["productions"])
    reachable = set()
    for syntax in used:
        grammar = load_grammar(syntax)
        reachable |= set(grammar.productions) - set(grammar.unreachable_productions(sorted(set(grammar.root_entrypoints.values()))))
    for production in sorted(common - reachable - placeholders):
        errors.append(f"grammar/common.json production {production} is reachable from no frontend")


def check_event_kinds(errors: list[str]) -> None:
    schema = load("schemas/semantic-events.schema.json")
    declared = set(schema["$defs"]["event"]["properties"]["kind"]["enum"])
    registry = load("schemas/semantic-event-kinds.json")
    registered = set(registry["kinds"])
    attribute_schemas = registry["attribute_schemas"]
    for missing in sorted(declared - registered):
        errors.append(f"semantic event kind {missing} has no attribute contract")
    for extra in sorted(registered - declared):
        errors.append(f"semantic event attribute contract {extra} has no schema kind")
    for kind, contract in registry["kinds"].items():
        keys = contract["required"] + contract["optional"]
        if len(keys) != len(set(keys)):
            errors.append(f"semantic event kind {kind} repeats an attribute")
        for key in keys:
            if key not in attribute_schemas:
                errors.append(f"semantic event kind {kind} uses untyped attribute {key}")
    validator = Draft202012Validator(schema)
    vectors = load("fixtures/execution-vectors.json")["vectors"] + load("fixtures/static-rejections.json")["vectors"]
    for vector in vectors:
        try:
            streams = vector_streams(vector, vector_trees(vector))
        except (ReaderError, ElaborationError):
            continue
        for syntax, stream in streams.items():
            label = f"vector {vector['id']} {syntax}"
            for error in validator.iter_errors(stream):
                errors.append(f"{label} semantic event schema violation: {error.message}")
            source_bytes = vector[syntax].encode("utf-8")
            if stream["source"]["sha256"] != hashlib.sha256(source_bytes).hexdigest() or stream["source"]["byte_length"] != len(source_bytes):
                errors.append(f"{label} semantic-event source identity is not bound to its bytes")
            spans: dict[str, tuple[int, int]] = {}
            child_positions: dict[tuple[str, str], list[int | None]] = {}
            for position, event in enumerate(stream["events"]):
                if event["sequence"] != position:
                    errors.append(f"{label} event sequence is not contiguous at {position}")
                span = event["origin"]["span"]
                payload = event["payload"]
                if span["source"] != stream["source"]["identity"] or not 0 <= span["start"] <= span["end"] <= stream["source"]["byte_length"]:
                    errors.append(f"{label} event {position} has an invalid source span")
                synthetic_unit = event["kind"] == "literal" and payload["attributes"].get("value", 0) is None
                if span["start"] == span["end"] and not synthetic_unit:
                    errors.append(f"{label} event {position} ({event['kind']}) has an empty source span")
                parent_id = payload["parent_id"]
                if position == 0:
                    if (parent_id, payload["role"], payload["index"]) != (None, None, None):
                        errors.append(f"{label} root event has a parent relation")
                elif parent_id not in spans:
                    errors.append(f"{label} event {position} does not name an earlier parent")
                else:
                    outer = spans[parent_id]
                    if not outer[0] <= span["start"] <= span["end"] <= outer[1]:
                        errors.append(f"{label} event {position} span {span['start']}..{span['end']} escapes its parent span {outer[0]}..{outer[1]}")
                    child_positions.setdefault((parent_id, payload["role"]), []).append(payload["index"])
                spans[payload["node_id"]] = (span["start"], span["end"])
                contract = registry["kinds"].get(event["kind"])
                if contract is None:
                    errors.append(f"{label} event {position} has unregistered kind {event['kind']}")
                    continue
                actual = set(payload["attributes"])
                required = set(contract["required"])
                allowed = required | set(contract["optional"])
                if not required <= actual:
                    errors.append(f"{label} event {position} ({event['kind']}) lacks attributes {sorted(required - actual)}")
                if not actual <= allowed:
                    errors.append(f"{label} event {position} ({event['kind']}) has forbidden attributes {sorted(actual - allowed)}")
                for key, value in payload["attributes"].items():
                    if key in attribute_schemas:
                        for error in Draft202012Validator(attribute_schemas[key]).iter_errors(value):
                            errors.append(f"{label} event {position} attribute {key}: {error.message}")
            for relation, positions in child_positions.items():
                if positions == [None]:
                    continue
                if any(position is None for position in positions) or positions != list(range(len(positions))):
                    errors.append(f"{label} child relation {relation} is not singular or contiguous")


def check_schema_artifacts(errors: list[str], write: bool) -> None:
    schemas = {
        name: load(f"schemas/{name}.schema.json")
        for name in ("semantic-events", "module-interface", "target-abi", "portable-message-abi", "native-call-abi", "abi-operation-table")
    }
    schemas["grammar"] = load("grammar/grammar.schema.json")
    for name, schema in schemas.items():
        try:
            Draft202012Validator.check_schema(schema)
        except Exception as error:
            errors.append(f"{name} schema is invalid: {error}")
    for name in ("common", "colisp", "coforth"):
        for error in Draft202012Validator(schemas["grammar"]).iter_errors(load(f"grammar/{name}.json")):
            errors.append(f"grammar/{name}.json schema violation: {error.message}")
    for target in load("schemas/target-registry.json")["admitted_tuples"]:
        instance = {"schema_version": 1, **target}
        for error in Draft202012Validator(schemas["target-abi"]).iter_errors(instance):
            errors.append(f"target tuple schema violation: {error.message}")
    fixtures = load("fixtures/schema-instances.json")
    for key, schema_name in (
        ("module_interface", "module-interface"),
        ("portable_message", "portable-message-abi"), ("native_call", "native-call-abi"),
        ("abi_operation_table", "abi-operation-table"),
    ):
        for error in Draft202012Validator(schemas[schema_name]).iter_errors(fixtures[key]):
            errors.append(f"{key} fixture schema violation: {error.message}")
    interface = fixtures["module_interface"]
    ordering = (
        ("exports", lambda item: (item["namespace"], item["identity"], item["kind"])),
        ("dependencies", lambda item: (item["module"], item["phase"], item["interface_digest"])),
    )
    for field, key in ordering:
        values = interface[field]
        keys = [key(item) for item in values]
        if keys != sorted(keys) or len(keys) != len(set(keys)):
            errors.append(f"module_interface fixture {field} is not uniquely sorted canonically")
    if interface["unsafe_summary"] != sorted(set(interface["unsafe_summary"])):
        errors.append("module_interface fixture unsafe_summary is not uniquely sorted canonically")

    def check_wire_value(value: Any, location: str) -> None:
        if isinstance(value, list):
            for index, item in enumerate(value):
                check_wire_value(item, f"{location}/{index}")
            return
        if not isinstance(value, dict):
            return
        encoding = value.get("encoding")
        fixed_widths = {
            "i8-le": 2, "u8-le": 2, "i16-le": 4, "u16-le": 4,
            "i32-le": 8, "u32-le": 8, "f32-ieee-le": 8, "unicode-scalar-le": 8,
            "i64-le": 16, "u64-le": 16, "f64-ieee-le": 16,
        }
        if encoding in fixed_widths and not re.fullmatch(f"[0-9a-f]{{{fixed_widths[encoding]}}}", value.get("data", "")):
            errors.append(f"{location} has the wrong byte width for {encoding}")
        if encoding == "utf8-hex":
            try:
                bytes.fromhex(value["data"]).decode("utf-8", errors="strict")
            except (KeyError, UnicodeDecodeError, ValueError) as error:
                errors.append(f"{location} is not strict UTF-8: {error}")
        elif encoding == "unicode-scalar-le":
            try:
                scalar = int.from_bytes(bytes.fromhex(value["data"]), "little")
            except (KeyError, ValueError) as error:
                errors.append(f"{location} has an invalid Unicode-scalar encoding: {error}")
            else:
                if scalar > 0x10FFFF or 0xD800 <= scalar <= 0xDFFF:
                    errors.append(f"{location} encodes non-scalar U+{scalar:04X}")
        for key, item in value.items():
            if key not in {"encoding", "data"}:
                check_wire_value(item, f"{location}/{key}")

    message = fixtures["portable_message"]
    if "arguments" in message:
        for index, argument in enumerate(message["arguments"]):
            check_wire_value(argument["value"], f"portable_message/arguments/{index}/value")
    if "result" in message and "value" in message["result"]:
        check_wire_value(message["result"]["value"], "portable_message/result/value")
    if message.get("diagnostic") is not None and "details" in message["diagnostic"]:
        check_wire_value(message["diagnostic"]["details"], "portable_message/diagnostic/details")
    operation_table = fixtures["abi_operation_table"]
    operations = {operation["operation_key"]: operation for operation in operation_table["operations"]}
    if len(operations) != len(operation_table["operations"]):
        errors.append("abi_operation_table repeats an operation key")
    operation = operations.get(message["operation_key"])
    if operation is None or operation_table["interface_digest"] != message["interface_digest"]:
        errors.append("portable_message does not resolve in its sealed ABI operation table")
    elif message["message_kind"] == "result":
        expected = operation["result"]
        actual = message["result"]
        if (actual["ownership"], actual.get("type_key"), actual.get("release_operation_key")) != (
            expected["ownership"], expected["type_key"], expected["release_operation_key"]
        ):
            errors.append("portable_message result ownership/type/release contract does not match operation table")
    native = fixtures["native_call"]
    native_operation = operations.get(native["operation_key"])
    if native_operation is None or operation_table["interface_digest"] != native["interface_digest"]:
        errors.append("native_call does not resolve in its sealed ABI operation table")
    else:
        actual_parameters = [(item["ownership"], item["type_key"]) for item in native["parameters"]]
        expected_parameters = [(item["ownership"], item["type_key"]) for item in native_operation["parameters"]]
        if actual_parameters != expected_parameters:
            errors.append("native_call parameter arity/ownership/type does not match operation table")
        if native["callback_policy"] != native_operation["callback_policy"]:
            errors.append("native_call callback policy does not match operation table")
    changed = False
    for key, domain_name, digest_key in (
        ("module_interface", "interface", "module_interface_digest"),
        ("portable_message", "portable_abi", "portable_message_digest"),
    ):
        domain = load("semantics/canonical-digests.json")["domains"][domain_name]
        tag = domain["tag"].encode() + bytes.fromhex(domain["tag_terminator_hex"])
        digest = hashlib.sha256(tag + canonical_bytes(fixtures[key])).hexdigest()
        if fixtures[digest_key] != digest:
            if write:
                fixtures[digest_key] = digest
                changed = True
            else:
                errors.append(f"fixtures/schema-instances.json has stale {digest_key}")
    if changed:
        SCHEMA_FIXTURES.write_text(json.dumps(fixtures, ensure_ascii=False, indent=2) + "\n")


def check_target_registry(errors: list[str], write: bool) -> None:
    registry = load("schemas/target-registry.json")
    namespaces = registry["namespaces"]
    fields = {
        "architecture": "architecture", "operating_system": "operating_system", "environment": "environment",
        "object_format": "object_format", "c_abi_revision": "c_abi_revision", "scalar_layout_table": "scalar_layout_table",
        "aggregate_layout_algorithm": "aggregate_layout_algorithm", "calling_convention_set": "calling_convention_set"
    }
    tuples: set[bytes] = set()
    changed = False
    digest_domain = load("semantics/canonical-digests.json")["domains"]["target_abi"]
    digest_tag = digest_domain["tag"].encode() + bytes.fromhex(digest_domain["tag_terminator_hex"])
    for target in registry["admitted_tuples"]:
        digest_input = {"schema_version": 1, **{key: value for key, value in target.items() if key != "digest"}}
        encoded = canonical_bytes(digest_input)
        if encoded in tuples:
            errors.append("schemas/target-registry.json has a duplicate admitted tuple")
        tuples.add(encoded)
        for field, namespace in fields.items():
            if target[field] not in namespaces[namespace]:
                errors.append(f"target tuple uses unknown {field} key {target[field]}")
        digest = hashlib.sha256(digest_tag + encoded).hexdigest()
        if target.get("digest") != digest:
            if write:
                target["digest"] = digest
                changed = True
            else:
                errors.append(f"target tuple {target['architecture']}/{target['operating_system']} has a stale digest")
    if changed:
        (LANG / "schemas/target-registry.json").write_text(json.dumps(registry, ensure_ascii=False, indent=2) + "\n")


def check_json_files(errors: list[str]) -> None:
    for path in sorted(LANG.rglob("*.json")):
        try:
            strict_json_loads(path.read_text(), str(path.relative_to(ROOT)))
        except (UnicodeDecodeError, json.JSONDecodeError, ValueError) as error:
            errors.append(f"{path.relative_to(ROOT)}: invalid JSON: {error}")


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--write", action="store_true", help="regenerate fixture digests before checking")
    args = parser.parse_args()
    errors: list[str] = []
    check_json_files(errors)
    if errors:
        print("\n".join(errors), file=sys.stderr)
        return 1
    check_grammars(errors)
    try:
        check_grammar_corpus(errors)
    except GrammarError as error:
        errors.append(f"grammar artifact is not executable: {error}")
    check_rules_and_fixtures(errors, args.write)
    check_replay_automaton(errors)
    check_compile_scheduler(errors)
    check_sessions(errors, args.write)
    errors.extend(validate_performance_budgets(load("semantics/performance-budgets.json")))
    check_event_kinds(errors)
    check_target_registry(errors, args.write)
    check_schema_artifacts(errors, args.write)
    prelude = subprocess.run(
        [sys.executable, str(Path(__file__).with_name("generate_spec_prelude.py"))],
        cwd=ROOT,
        capture_output=True,
        text=True,
        check=False,
    )
    if prelude.returncode:
        errors.append(prelude.stdout.strip() or prelude.stderr.strip())
    if errors:
        print("\n".join(errors), file=sys.stderr)
        return 1
    print("language specification artifacts are consistent")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
