#!/usr/bin/env python3
"""Reference execution machine for the Finch dynamic semantics.

The machine has no per-form evaluation logic of its own.  Every node is executed by interpreting
the canonical rule program for its form from ``docs/language/semantics/transitions.json``; this
module only defines what each instruction of that vocabulary does to machine state, plus the
unwinding discipline the rule file states under ``machine.unwinding``.

It is an executable definition of observable behaviour (the transition trace, terminal outcome,
journal, drop sequence, and host acceptance log).  It is not an implementation strategy: section 2
of the specification forbids reading it as a requirement to allocate one object per transition.
"""

from __future__ import annotations

import hashlib
import inspect
import json
from typing import Any, Callable, Generator


INT_MIN = -(2**63)
INT_MAX = 2**63 - 1
ARITHMETIC = {
    "+": lambda a, b: a + b,
    "-": lambda a, b: a - b,
    "*": lambda a, b: a * b,
}
COMPARISON = {
    "==": lambda a, b: a == b,
    "!=": lambda a, b: a != b,
    "<": lambda a, b: a < b,
    "<=": lambda a, b: a <= b,
    ">": lambda a, b: a > b,
    ">=": lambda a, b: a >= b,
}
INTRINSICS = frozenset({*ARITHMETIC, *COMPARISON, "/", "drop"})


class MachineError(Exception):
    """The program is one a conforming compiler rejects before execution.

    ``code`` is the stable diagnostic a static-rejection vector names; errors without a specific
    code are defects in the vector or the artifacts rather than modelled user errors.
    """

    def __init__(self, message: str, code: str = "F-DIAG-ILL-FORMED"):
        super().__init__(message)
        self.code = code


class _Abrupt(Exception):
    exit_class = "failure"


class _Raise(_Abrupt):
    def __init__(self, envelope: dict[str, Any]):
        super().__init__("raise")
        self.envelope = envelope

    def note(self, text: str) -> None:
        self.envelope["suppressed"].append(text)

    def describe(self) -> str:
        return f"exception:{shown(self.envelope['value'])}"


class _Protected(_Abrupt):
    def __init__(self, edge: str, detail: str | None = None):
        super().__init__(edge)
        self.edge = edge
        self.detail = detail
        self.suppressed: list[str] = []
        self.exit_class = "cancel" if edge == "Cancel" else "failure"

    def note(self, text: str) -> None:
        self.suppressed.append(text)

    def spelling(self) -> str:
        return self.edge if self.detail is None else f"{self.edge}({self.detail})"

    def describe(self) -> str:
        return f"protected:{self.spelling()}"


class _LoopTransfer(_Abrupt):
    exit_class = "success"

    def __init__(self, edge: str):
        super().__init__(edge)
        self.edge = edge


class _FrameTransfer(_Abrupt):
    exit_class = "success"

    def __init__(self, edge: str, value: Any = None, callee: Any = None, arguments: list[Any] | None = None, carried: list[tuple[Cell, list[int]]] | None = None, adopt_callee: bool = False):
        super().__init__(edge)
        self.edge = edge
        self.value = value
        self.callee = callee
        self.arguments = arguments or []
        self.carried = carried or []  # adopted values that follow their loans into the next frame
        self.adopt_callee = adopt_callee  # the closure being called moves into the next frame


def shown(value: Any) -> str:
    """Canonical spelling of a value in a transition trace."""
    if value is None:
        return "unit"
    if value is True:
        return "true"
    if value is False:
        return "false"
    if isinstance(value, int):
        return str(value)
    if isinstance(value, str):
        return json.dumps(value, ensure_ascii=False)
    if isinstance(value, Cell):
        return f"&{shown(value.value)}"
    if "$record" in value:
        fields = ",".join(f"{name}={shown(item)}" for name, item in value["fields"].items())
        return f"{value['$record']}{{{fields}}}"
    if "$variant" in value:
        arguments = ",".join(shown(item) for item in value["arguments"])
        return f"{value['$variant']}({arguments})"
    if "$closure" in value:
        return f"closure#{value['$closure']}"
    if "$fiber" in value:
        return f"fiber#{value['$fiber']}"
    if "$task" in value:
        return f"task#{value['$task']}"
    raise MachineError(f"value has no canonical spelling: {value!r}")


def exported(value: Any) -> Any:
    """JSON form of a value in a terminal outcome."""
    if isinstance(value, Cell):
        return {"$loan": exported(value.value)}
    if isinstance(value, dict):
        return {key: exported(item) for key, item in value.items()}
    if isinstance(value, list):
        return [exported(item) for item in value]
    return value


class Cell:
    """One place: a value plus its initialization state."""

    __slots__ = ("value", "state", "mutable", "name", "depth")

    def __init__(self, name: str, value: Any, mutable: bool = False, depth: int = -1):
        self.name = name
        self.value = value
        self.state = "live"
        self.mutable = mutable
        self.depth = depth  # number of frames live when a frame-owned place was created; -1 if not frame-owned


class Context:
    """One resumable execution: frames, operand stack, and the lexical cleanup stack."""

    def __init__(self) -> None:
        self.frames: list[dict[str, Any]] = []
        self.stack: list[Any] = []
        self.cleanup: list[dict[str, Any]] = []


class Activation:
    def __init__(self, rule: dict[str, Any], node: dict[str, Any], context: Context):
        self.rule = rule
        self.node = node
        self.stack_height = len(context.stack)
        self.cleanup_height = len(context.cleanup)
        self.scope_height = len(context.frames[-1]["scopes"]) if context.frames else 0
        self.handler: list[dict[str, Any]] | None = None
        self.loop = False
        self.local: dict[str, Any] = {}


def resolve(ast: dict[str, Any], operations: dict[str, Any] | None = None) -> dict[str, Any]:
    """Resolve a frontend-normalized AST into the program the machine executes.

    Resolution is the shared, syntax-neutral phase after semantic construction: top-level function
    declarations are hoisted, each call is classified by what its callee names, and calls in tail
    position are marked.  Nothing here depends on which frontend produced the AST.
    """
    operations = operations or {}
    items = ast["items"] if ast["form"] == "sequence" else [ast]
    definitions = {item["name"]: item for item in items if item["form"] == "function"}
    constructors = {case["name"]: (item["name"], len(case["payload"])) for item in items if item["form"] == "variant" for case in item["cases"]}
    # `yield` evaluates to an option of the reply, so the two cases are always constructible.
    constructors.setdefault("some", ("Option", 1))
    constructors.setdefault("none", ("Option", 0))
    # A function is a generator exactly when `yield` appears in its own body.
    generators = {name for name, definition in definitions.items() if yields_itself(definition["body"])}
    body_items = [item for item in items if item["form"] not in {"function", "variant"}]
    if ast["form"] == "sequence" and len(body_items) == len(items):
        body: dict[str, Any] = ast
    elif len(body_items) == 1:
        body = body_items[0]
    else:
        body = {"form": "sequence", "items": body_items}

    # A `return` hands its operand's call the frame only when nothing in the callable still has to
    # observe that call: not inside a try body, and not in a fiber body.
    held = [0]

    def lent(parameters: list[dict[str, Any]], captures: list[dict[str, Any]] = ()) -> frozenset[Any]:
        """Markers for names that hold a loan: a call through one cannot take the closure with it."""
        modes = {"borrow", "borrow-mut"}
        return frozenset(("&", item["name"]) for item in [*parameters, *captures] if item["ownership"] in modes)

    def shadow(scope: frozenset[Any], names: Any) -> frozenset[Any]:
        names = frozenset(names)
        return (scope - {("&", name) for name in names}) | names

    def walk(node: dict[str, Any], scope: frozenset[str], tail: bool) -> dict[str, Any]:
        form = node["form"]
        if form in {"literal", "break", "continue-loop", "rethrow"}:
            return dict(node)
        if form == "read":
            if node["place"] not in scope and node["place"] in definitions:
                # A function named in value position is a capture-free callable over its definition.
                return {"form": "lambda", "function": node["place"], "capture_default": "exact", "captures": [],
                        "parameters": definitions[node["place"]]["parameters"], "body": {"form": "literal", "value": None}}
            return dict(node)
        if form == "let":
            return {**node, "initializer": walk(node["initializer"], scope, False), "body": walk(node["body"], shadow(scope, {node["name"]}), tail)}
        if form == "assign":
            return {**node, "value": walk(node["value"], scope, False)}
        if form == "sequence":
            count = len(node["items"])
            return {**node, "items": [walk(item, scope, tail and index + 1 == count) for index, item in enumerate(node["items"])]}
        if form == "if":
            return {**node, "condition": walk(node["condition"], scope, False), "then": walk(node["then"], scope, tail), "else": walk(node["else"], scope, tail)}
        if form == "while":
            return {**node, "condition": walk(node["condition"], scope, False), "body": walk(node["body"], scope, False)}
        if form in {"throw", "yield"}:
            return {**node, "value": walk(node["value"], scope, False)}
        if form == "return":
            return {**node, "value": walk(node["value"], scope, held[0] == 0)}
        if form == "match":
            arms = [
                {**arm, "body": walk(arm["body"], shadow(scope, pattern_binders(arm["pattern"])), tail)}
                for arm in node["arms"]
            ]
            scrutinee = walk(node["scrutinee"], scope, False)
            if node["ownership"] == "borrow" and scrutinee["form"] == "read":
                scrutinee["mode"] = "borrow"
            return {**node, "scrutinee": scrutinee, "arms": arms}
        if form == "try":
            # A handler arm still owes the release of the exception it caught, so neither the try
            # body nor an arm is tail position.
            held[0] += 1
            catches = [
                {**clause, "body": walk(clause["body"], shadow(scope, pattern_binders(clause["pattern"])), False)}
                for clause in node["catches"]
            ]
            body = walk(node["body"], scope, False)
            held[0] -= 1
            return {**node, "body": body, "catches": catches}
        if form == "scope":
            guards = [{**guard, "body": walk(guard["body"], scope, False)} for guard in node["guards"]]
            return {**node, "guards": guards, "body": walk(node["body"], scope, tail)}
        if form == "member":
            target = walk(node["target"], scope, False)
            if target["form"] == "read":
                target["mode"] = "borrow"
            return {**node, "target": target}
        if form == "record-construct":
            return {**node, "fields": [{**field, "value": walk(field["value"], scope, False)} for field in node["fields"]]}
        if form == "lambda" and "function" in node:
            return dict(node)
        if form in {"lambda", "fiber"}:
            own = frozenset(p["name"] for p in node["parameters"]) if form == "lambda" else frozenset({node["resume"]})
            captures = [dict(capture) for capture in node["captures"]]
            listed = {capture["name"] for capture in captures}
            for name in free_names(node["body"], own):
                if name in scope and name not in listed:
                    if node["capture_default"] == "exact":
                        raise MachineError(f"closure uses {name!r} without listing it in its exact capture list", "F-DIAG-CAPTURE-UNLISTED")
                    captures.append({"form": "capture", "name": name, "ownership": "steal" if node["capture_default"] == "move" else "borrow"})
                    listed.add(name)
            for capture in captures:
                if capture["name"] not in scope:
                    raise MachineError(f"capture of unbound local {capture['name']!r}", "F-DIAG-UNBOUND-NAME")
            saved, held[0] = held[0], 0 if form == "lambda" else 1
            inner = frozenset(listed) | own | lent(node["parameters"] if form == "lambda" else [], captures)
            body = walk(node["body"], inner, form == "lambda")
            held[0] = saved
            return {**node, "captures": captures, "body": body}
        if form == "call":
            callee = node["callee"]
            arguments = [walk(argument, scope, False) for argument in node["arguments"]]
            # A tail call through a closure the frame owns takes the closure with it: the new frame
            # adopts it.  A closure the frame was only lent stays where it is.
            if isinstance(callee, dict):
                return {"form": "call", "callee": walk(callee, scope, False), "arguments": arguments, **({"tail": True, "adopt_callee": True} if tail else {})}
            if callee in scope:
                if tail and ("&", callee) not in scope:
                    return {"form": "call", "callee": {"form": "read", "place": callee}, "arguments": arguments, "tail": True, "adopt_callee": True}
                target = {"form": "read", "place": callee, "mode": "borrow"}
                return {"form": "call", "callee": target, "arguments": arguments, **({"tail": True} if tail else {})}
            if callee in constructors:
                variant, arity = constructors[callee]
                if arity != len(arguments):
                    raise MachineError(f"constructor {callee} takes {arity} values, found {len(arguments)}", "F-DIAG-CONSTRUCTOR-ARITY")
                return {"form": "variant-construct", "type": variant, "case": callee, "arguments": arguments}
            if callee in definitions:
                modes = [parameter["ownership"] for parameter in definitions[callee]["parameters"]]
                if len(modes) != len(arguments):
                    raise MachineError(f"call to {callee} passes {len(arguments)} arguments for {len(modes)} parameters")
                for mode, argument in zip(modes, arguments):
                    if mode in {"borrow", "borrow-mut"} and argument["form"] == "read":
                        argument["mode"] = mode
                if callee in generators:
                    # Calling a generator runs none of it: the call makes a dormant fiber.
                    return {"form": "generator-create", "callee": callee, "arguments": arguments}
                return {"form": "call", "callee": callee, "arguments": arguments, **({"tail": True} if tail else {})}
            if callee in operations:
                kind = operations[callee]["kind"]
                if kind not in {"effect", "await", "emit"}:
                    raise MachineError(f"host operation {callee} has unknown kind {kind!r}")
                if kind == "emit":
                    if len(arguments) != 1:
                        raise MachineError("an emit operation takes exactly one event")
                    return {"form": "emit", "operation": callee, "event": arguments[0]}
                return {"form": kind, "operation": callee, "arguments": arguments}
            if callee in {"spawn", "join", "cancel"}:
                if len(arguments) != 1:
                    raise MachineError(f"{callee} takes exactly one argument")
                return {"form": callee, "operand": arguments[0]}
            if callee in GENERATOR_OPERATIONS:
                if not arguments or (callee != "reply" and len(arguments) != 1):
                    raise MachineError(f"{callee} takes a generator" + (" and its reply arguments" if callee == "reply" else ""))
                target = arguments[0]
                if target["form"] == "read" and callee != "start":
                    target["mode"] = "borrow-mut"
                return {"form": "generator-op", "operation": callee, "target": target, "arguments": arguments[1:]}
            if callee in INTRINSICS:
                return {"form": "call", "callee": callee, "arguments": arguments}
            raise MachineError(f"unresolved callee {callee!r}", "F-DIAG-UNBOUND-NAME")
        if form in {"function", "variant"}:
            raise MachineError(f"{form} {node['name']} is declared inside an expression; declarations are items of a submission or module", "F-DIAG-NESTED-DECLARATION")
        raise MachineError(f"resolution has no rule for form {form!r}")

    if yields_itself(body):
        raise MachineError("yield outside a function or lambda body", "F-DIAG-YIELD-TARGET")
    resolved_definitions = {}
    for name, definition in definitions.items():
        parameters = frozenset(parameter["name"] for parameter in definition["parameters"]) | lent(definition["parameters"])
        # A generator's frame belongs to its handle, so nothing in its body is a tail call.
        held[0] = 1 if name in generators else 0
        resolved_definitions[name] = {**definition, "body": walk(definition["body"], parameters, name not in generators)}
        if name in generators:
            resolved_definitions[name]["generator"] = True
    held[0] = 0
    variants = {item["name"]: {case["name"]: case["payload"] for case in item["cases"]} for item in items if item["form"] == "variant"}
    variants.setdefault("Option", {"some": ["unknown"], "none": []})
    return {"definitions": resolved_definitions, "variants": variants, "body": walk(body, frozenset(), True)}


GENERATOR_OPERATIONS = frozenset({"empty?", "front", "pop-front", "reply", "start"})


def yields_itself(node: Any) -> bool:
    """Whether `yield` appears in this body itself, not inside a callable nested in it."""
    if isinstance(node, list):
        return any(yields_itself(item) for item in node)
    if not isinstance(node, dict):
        return False
    if node.get("form") == "yield":
        return True
    if node.get("form") in {"lambda", "function"}:
        return False
    return any(yields_itself(value) for value in node.values())


def free_names(node: Any, bound: frozenset[str]) -> list[str]:
    """Names a body reads, assigns, or calls that it does not bind itself, in first-use order."""
    found: list[str] = []

    def visit(value: Any, inner: frozenset[str]) -> None:
        if isinstance(value, list):
            for item in value:
                visit(item, inner)
            return
        if not isinstance(value, dict) or "form" not in value:
            return
        form = value["form"]
        names: list[str] = []
        if form in {"read", "assign"}:
            names.append(value["place"])
        if form == "call" and isinstance(value["callee"], str):
            names.append(value["callee"])
        if form in {"lambda", "fiber"}:
            names.extend(capture["name"] for capture in value["captures"])
            own = frozenset(p["name"] for p in value["parameters"]) if form == "lambda" else frozenset({value["resume"]})
            names.extend(name for name in free_names(value["body"], own) if name not in names)
            for name in names:
                if name not in inner and name not in found:
                    found.append(name)
            return
        for name in names:
            if name not in inner and name not in found:
                found.append(name)
        if form == "let":
            visit(value["initializer"], inner)
            visit(value["body"], inner | {value["name"]})
            return
        if form in {"match-arm", "catch"}:
            visit(value["body"], inner | pattern_binders(value["pattern"]))
            return
        for key, item in value.items():
            if key != "form":
                visit(item, inner)

    visit(node, bound)
    return found


def pattern_binders(pattern: Any) -> frozenset[str]:
    if isinstance(pattern, dict):
        if "bind" in pattern:
            return frozenset({pattern["bind"]})
        if "as" in pattern:
            return frozenset({pattern["as"]}) | pattern_binders(pattern["pattern"])
        if "constructor" in pattern:
            found: frozenset[str] = frozenset()
            for argument in pattern["arguments"]:
                found |= pattern_binders(argument)
            return found
        if "record" in pattern:
            found = frozenset()
            for field in pattern["fields"]:
                found |= pattern_binders(field["pattern"])
            return found
    return frozenset()


class Machine:
    def __init__(
        self,
        rules: dict[str, Any],
        program: dict[str, Any],
        host: dict[str, Any] | None = None,
        replay_automaton: Callable[[dict[str, Any], dict[str, Any]], dict[str, Any]] | None = None,
    ):
        self.rules = {rule["form"]: rule for rule in rules["rules"]}
        self.vocabulary = set(rules["machine"]["instruction_vocabulary"])
        self.program = program
        self.host = host or {}
        self.replay = replay_automaton
        self.context = Context()
        self.trace: list[str] = []
        self.journal: list[str] = []
        self.drops: list[str] = []
        self.host_log: list[str] = []
        self.reaper: list[str] = []
        self.transaction = "open"
        self.hits: set[tuple[str, str]] = set()
        self.read_modes: dict[int, set[str]] = {}
        self.lifecycle_marks: set[int] = set()
        self.instructions_run: set[tuple[str, int]] = set()
        self.max_frames = 0
        self.safepoints = 0
        self.raises = 0
        self.entry = {"$entry": True}
        self.entry_node = {"form": "lambda", "captures": [], "parameters": [], "body": program["body"]}
        self.closures: list[dict[str, Any]] = []
        self.fibers: list[dict[str, Any]] = []
        self.tasks: list[dict[str, Any]] = []
        self.grants: dict[str, bool] = dict(self.host.get("grants", {}))
        self.root_grants = self.grants
        self.terminal: dict[str, Any] | None = None
        generation = f"{self.host.get('generation', 0):016x}"
        self.effects = {"generation": generation, "next_sequence": f"{0:016x}", "terminal": False, "requests": {}}
        self.resumes = {"generation": generation, "next_sequence": f"{0:016x}", "terminal": False, "requests": {}, "outstanding": None}
        self.messages = list(self.host.get("resumes", []))
        self.pending: dict[str, Any] | None = None

    # ------------------------------------------------------------------ state helpers

    def hit(self, activation: Activation, label: str) -> None:
        self.hits.add((activation.rule["rule"], label))

    @property
    def frame(self) -> dict[str, Any]:
        return self.context.frames[-1]

    def push(self, value: Any) -> None:
        self.context.stack.append(value)

    def pop(self, activation: Activation) -> Any:
        if len(self.context.stack) <= activation.stack_height:
            raise MachineError(f"{activation.rule['rule']} pops below its own operand base")
        return self.context.stack.pop()

    def lookup(self, name: str) -> Cell:
        for scope in reversed(self.frame["scopes"]):
            if name in scope:
                return scope[name]
        raise MachineError(f"unbound local {name!r}", "F-DIAG-UNBOUND-NAME")

    def is_copy(self, value: Any) -> bool:
        if isinstance(value, Cell):
            return True
        if isinstance(value, dict):
            if "$record" in value:
                return value["$record"] in self.program.get("copy_types", [])
            return False
        return True

    def drop_value(self, value: Any) -> None:
        """Run the lifecycle of an owned value whose obligation ends here."""
        if isinstance(value, Cell) or not isinstance(value, dict):
            return
        if "$record" in value:
            if value["$record"] in self.program.get("lifecycle", []):
                self.lifecycle_marks.add(len(self.trace))
                self.trace.append(f"Continue(drop:{shown(value)})")
                self.drops.append(shown(value))
            for item in reversed(list(value["fields"].values())):
                self.drop_value(item)
        elif "$variant" in value:
            for item in reversed(value["arguments"]):
                self.drop_value(item)
        elif "$fiber" in value:
            self.cancel_fiber(value)
        elif "$task" in value:
            task = self.tasks[value["$task"]]
            if task["state"] == "runnable":
                task["state"] = "cancel-requested"
                self.lifecycle_marks.add(len(self.trace))
                self.trace.append(f"Continue(drop:{shown(value)})")
                self.drops.append(shown(value))
                self.reaper.append(shown(value))
        elif "$closure" in value:
            for cell in reversed(self.closures[value["$closure"]]["owned"]):
                if cell.state == "live":
                    cell.state = "dropped"
                    self.drop_value(cell.value)

    def bind(self, name: str, value: Any, mutable: bool = False, owned: bool = True) -> Cell:
        """Introduce a place.  An owned place carries a drop obligation; a loan place does not."""
        cell = Cell(name, value, mutable, len(self.context.frames))
        self.frame["scopes"][-1][name] = cell
        if owned:
            self.context.cleanup.append({"kind": "drop", "cell": cell})
        return cell

    def target(self, name: str) -> Cell:
        """The place an assignment writes: the local itself, or the owner an exclusive loan names."""
        cell = self.lookup(name)
        return cell.value if isinstance(cell.value, Cell) else cell

    # ------------------------------------------------------------------ unwinding

    def run_entry(self, entry: dict[str, Any], exit_class: str) -> Generator[Any, Any, None]:
        if entry["kind"] == "drop":
            cell = entry["cell"]
            if cell.state == "live":
                cell.state = "dropped"
                self.drop_value(cell.value)
            return
        if entry["kind"] == "field":
            self.drop_value(entry["value"])
            return
        activation: Activation = entry["activation"]
        reason = entry["reason"]
        applies = reason == "exit" or reason == exit_class or (reason == "failure" and exit_class == "cancel")
        if not applies:
            self.hit(activation, "skipped")
            return
        self.hit(activation, f"run-{entry['reason']}")
        self.trace.append(f"Continue(cleanup:{entry['reason']})")
        frame = self.frame
        frame["barriers"] += 1
        saved_loops, frame["loops"] = frame["loops"], 0
        try:
            yield from self.eval(entry["body"])
            self.drop_value(self.context.stack.pop())
        except (_LoopTransfer, _FrameTransfer) as transfer:
            raise MachineError("a guard body cannot transfer control out of itself", "F-DIAG-GUARD-ESCAPE") from transfer
        finally:
            frame["barriers"] -= 1
            frame["loops"] = saved_loops

    def unwind(self, cleanup_height: int, stack_height: int, transfer: _Abrupt, owner: Activation | None) -> Generator[Any, Any, _Abrupt]:
        """Leave an activation or frame abruptly: temporaries first, then registered cleanup LIFO."""
        context = self.context
        while len(context.stack) > stack_height:
            self.drop_value(context.stack.pop())
        while len(context.cleanup) > cleanup_height:
            entry = context.cleanup.pop()
            try:
                yield from self.run_entry(entry, transfer.exit_class)
            except _Abrupt as secondary:
                while len(context.stack) > stack_height:
                    self.drop_value(context.stack.pop())
                if transfer.exit_class == "success":
                    if owner is not None:
                        self.hit(owner, "guard-failure-primary")
                    transfer = secondary
                else:
                    if owner is not None:
                        self.hit(owner, "guard-failure-suppressed")
                    text = secondary.describe()  # type: ignore[attr-defined]
                    self.trace.append(f"Continue(cleanup-suppressed:{text})")
                    transfer.note(text)  # type: ignore[attr-defined]
        return transfer

    # ------------------------------------------------------------------ rule interpretation

    def eval(self, node: dict[str, Any]) -> Generator[Any, Any, None]:
        form = node["form"]
        if form == "call" and node.get("tail"):
            form = "tail-call"
        if form not in self.rules:
            raise MachineError(f"no transition rule for form {form!r}")
        yield from self.activate(self.rules[form], node)

    def activate(self, rule: dict[str, Any], node: dict[str, Any], **local: Any) -> Generator[Any, Any, Any]:
        activation = Activation(rule, node, self.context)
        activation.local.update(local)
        program = rule["program"]
        labels = {instruction["label"]: index for index, instruction in enumerate(program) if "label" in instruction}
        pc = 0
        frame = self.frame
        while True:
            try:
                while pc < len(program):
                    instruction = program[pc]
                    operation = instruction["op"]
                    if operation not in self.vocabulary:
                        raise MachineError(f"{rule['rule']} uses undeclared instruction {operation!r}")
                    self.instructions_run.add((rule["rule"], pc))
                    outcome = getattr(self, f"op_{operation}")(activation, instruction)
                    if inspect.isgenerator(outcome):
                        outcome = yield from outcome
                    if isinstance(outcome, tuple) and outcome[0] == "finish":
                        return outcome[1]
                    pc = labels[outcome] if isinstance(outcome, str) else pc + 1
                raise MachineError(f"{rule['rule']} ran off the end of its program")
            except _Abrupt as transfer:
                guarded = activation.handler is not None
                if guarded:
                    frame["handlers"] -= 1
                transfer = yield from self.unwind(activation.cleanup_height, activation.stack_height, transfer, activation)
                del frame["scopes"][activation.scope_height :]
                if guarded:
                    # Control left a try body before it finished: the bindings that body moves are
                    # dropped here if it had not moved them yet, so a handler never sees them.
                    self.join_drops(activation.node.get("unwind_drops", []))
                if "active" in activation.local:
                    frame["active"].pop()
                    del activation.local["active"]
                if activation.loop and isinstance(transfer, _LoopTransfer):
                    self.hit(activation, "break" if transfer.edge == "exit" else "continue")
                    pc = labels[transfer.edge]
                    continue
                if activation.loop:
                    frame["loops"] -= 1
                    activation.loop = False
                if activation.handler is not None and isinstance(transfer, _Raise):
                    clauses, activation.handler = activation.handler, None
                    handled = False
                    for clause in clauses:
                        verdict = yield from self.activate(self.rules["catch"], clause, envelope=transfer.envelope)
                        if verdict == "handled":
                            handled = True
                            break
                    if handled:
                        self.hit(activation, "caught")
                        return None
                    self.hit(activation, "missed")
                    self.trace.append("Continue(catch:miss)")
                activation.handler = None
                raise transfer

    # ------------------------------------------------------------------ instructions: values and places

    def op_continue(self, activation: Activation, instruction: dict[str, Any]) -> tuple[str, Any]:
        if len(self.context.stack) != activation.stack_height + 1:
            raise MachineError(
                f"{activation.rule['rule']} must complete with exactly one value; operand delta is "
                f"{len(self.context.stack) - activation.stack_height}"
            )
        return ("finish", None)

    def op_complete_without_value(self, activation: Activation, instruction: dict[str, Any]) -> tuple[str, Any]:
        if len(self.context.stack) != activation.stack_height:
            raise MachineError(f"{activation.rule['rule']} must not leave an operand")
        return ("finish", None)

    def op_push_literal(self, activation: Activation, instruction: dict[str, Any]) -> None:
        value = activation.node[instruction["field"]]
        self.push(value)
        self.hit(activation, "value")
        self.trace.append(f"Continue(push:{shown(value)})")

    def op_push_unit(self, activation: Activation, instruction: dict[str, Any]) -> None:
        self.push(None)
        self.trace.append("Continue(push:unit)")

    def op_read_place(self, activation: Activation, instruction: dict[str, Any]) -> None:
        name = activation.node[instruction["field"]]
        cell = self.lookup(name)
        if cell.state != "live":
            raise MachineError(f"read of {cell.state} place {name!r}", "F-DIAG-USE-AFTER-MOVE")
        loan = cell.value if isinstance(cell.value, Cell) else None
        value = loan.value if loan is not None else cell.value
        mode = activation.node.get("mode")
        decision = "borrow" if mode == "borrow-mut" or (not self.is_copy(value) and (mode == "borrow" or loan is not None)) else "copy" if self.is_copy(value) else "move"
        self.read_modes.setdefault(id(activation.node), set()).add(decision)
        if decision == "borrow":
            self.hit(activation, "borrow")
            self.push(loan if loan is not None else cell)
            self.trace.append(f"Continue(borrow:{name})")
            return
        if self.is_copy(value):
            self.hit(activation, "copy")
            self.trace.append(f"Continue(read:{name}={shown(value)})")
        else:
            self.hit(activation, "move")
            cell.state = "moved"
            self.trace.append(f"Continue(move:{name}={shown(value)})")
        self.push(value)

    def op_eval(self, activation: Activation, instruction: dict[str, Any]) -> Generator[Any, Any, None]:
        yield from self.eval(activation.node[instruction["operand"]])

    def op_eval_each_left_to_right(self, activation: Activation, instruction: dict[str, Any]) -> Generator[Any, Any, None]:
        items = activation.node[instruction["operand"]]
        if instruction["keep"] == "all":
            for item in items:
                yield from self.eval(item)
            return
        if not items:
            self.hit(activation, "empty")
            self.op_push_unit(activation, instruction)
            return
        self.hit(activation, "nonempty")
        for index, item in enumerate(items):
            yield from self.eval(item)
            if index + 1 != len(items):
                self.drop_value(self.pop(activation))

    def op_bind_fresh(self, activation: Activation, instruction: dict[str, Any]) -> None:
        value = self.pop(activation)
        name = activation.node[instruction["field"]]
        self.frame["scopes"].append({})
        self.bind(name, value, bool(activation.node.get("mutable")))
        self.hit(activation, "bound")
        self.trace.append(f"Continue(bind:{name}={shown(value)})")

    def op_drop_scope(self, activation: Activation, instruction: dict[str, Any]) -> Generator[Any, Any, None]:
        """Normal exit of the bindings this activation introduced; the result operand survives."""
        context = self.context
        while len(context.cleanup) > activation.cleanup_height:
            entry = context.cleanup.pop()
            yield from self.run_entry(entry, "success")
        del self.frame["scopes"][activation.scope_height :]
        self.join_drops(activation.local.pop("join", []))

    def op_eval_to_temporary(self, activation: Activation, instruction: dict[str, Any]) -> Generator[Any, Any, None]:
        yield from self.eval(activation.node[instruction["operand"]])
        activation.local["temporary"] = self.pop(activation)
        self.trace.append(f"Continue(temp:{shown(activation.local['temporary'])})")

    def op_validate_no_borrow_from_destination(self, activation: Activation, instruction: dict[str, Any]) -> None:
        if activation.local["temporary"] is self.target(activation.node["place"]):
            raise MachineError("assigned value still borrows from its destination")

    def op_end_conflicting_loans(self, activation: Activation, instruction: dict[str, Any]) -> None:
        """A static obligation (see ``machine.static_obligations``); it has no dynamic effect."""
        return None

    def op_drop_destination(self, activation: Activation, instruction: dict[str, Any]) -> None:
        if not self.lookup(activation.node["place"]).mutable:
            raise MachineError(f"assignment to immutable local {activation.node['place']!r}", "F-DIAG-ASSIGN-IMMUTABLE")
        cell = self.target(activation.node["place"])
        if cell.state != "live":
            raise MachineError(f"assignment to {cell.state} local {cell.name!r}")
        self.trace.append(f"Continue(drop:{activation.node['place']}={shown(cell.value)})")
        if not self.is_copy(cell.value):
            self.drop_value(cell.value)

    def op_move_temporary(self, activation: Activation, instruction: dict[str, Any]) -> None:
        cell = self.target(activation.node[instruction["field"]])
        cell.value = activation.local.pop("temporary")
        self.hit(activation, "assigned")
        self.trace.append(f"Continue(write:{activation.node[instruction['field']]}={shown(cell.value)})")

    # ------------------------------------------------------------------ instructions: control

    def op_require_bool(self, activation: Activation, instruction: dict[str, Any]) -> None:
        if not isinstance(self.context.stack[-1], bool):
            raise MachineError(f"{activation.rule['rule']} condition is not bool")

    def join_drops(self, names: list[str]) -> None:
        """Drop the bindings the compiler determined another path moved and this one still holds."""
        for name in names:
            cell = self.lookup(name)
            if cell.state == "live":
                cell.state = "dropped"
                self.drop_value(cell.value)

    def op_branch(self, activation: Activation, instruction: dict[str, Any]) -> Generator[Any, Any, None]:
        arm = instruction["true"] if self.pop(activation) else instruction["false"]
        self.hit(activation, arm)
        self.trace.append(f"Continue(branch:{arm})")
        yield from self.eval(activation.node[arm])
        self.join_drops(activation.node.get("join_drops", {}).get(arm, []))

    def op_enter_loop(self, activation: Activation, instruction: dict[str, Any]) -> None:
        activation.loop = True
        self.frame["loops"] += 1

    def op_branch_loop(self, activation: Activation, instruction: dict[str, Any]) -> Generator[Any, Any, str | None]:
        if not self.pop(activation):
            self.hit(activation, "exit")
            self.trace.append("Continue(loop:exit)")
            return instruction["false"]
        self.hit(activation, "body")
        self.trace.append("Continue(loop:body)")
        yield from self.eval(activation.node[instruction["true"]])
        self.drop_value(self.pop(activation))
        return None

    def op_cancellation_safepoint(self, activation: Activation, instruction: dict[str, Any]) -> None:
        index = self.safepoints
        self.safepoints += 1
        if self.host.get("cancel_at_safepoint") == index:
            self.hit(activation, "cancel")
            raise _Protected("Cancel")

    def op_jump(self, activation: Activation, instruction: dict[str, Any]) -> str:
        return instruction["target"]

    def op_leave_loop(self, activation: Activation, instruction: dict[str, Any]) -> None:
        activation.loop = False
        self.frame["loops"] -= 1

    def op_transfer_to_loop(self, activation: Activation, instruction: dict[str, Any]) -> None:
        if self.frame["loops"] == 0:
            raise MachineError(f"{activation.node['form']} has no lexically enclosing loop", "F-DIAG-LOOP-TARGET")
        self.hit(activation, instruction["edge"])
        self.trace.append(f"Continue(loop:{'break' if instruction['edge'] == 'exit' else 'continue'})")
        raise _LoopTransfer(instruction["edge"])

    def op_transfer_to_frame(self, activation: Activation, instruction: dict[str, Any]) -> None:
        if self.frame["barriers"]:
            raise MachineError("a guard body cannot return from or tail-call out of its enclosing callable", "F-DIAG-GUARD-ESCAPE")
        if instruction["edge"] == "return":
            if self.frame["callable"] == "<entry>":
                raise MachineError("return has no enclosing function, lambda, or fiber body", "F-DIAG-RETURN-TARGET")
            self.hit(activation, "frame")
            raise _FrameTransfer("return", value=self.pop(activation))
        count = len(activation.node["arguments"])
        arguments = [self.pop(activation) for _ in range(count)][::-1]
        raise _FrameTransfer("tail-call", callee=activation.local.pop("callee"), arguments=arguments, carried=activation.local.pop("carried", []), adopt_callee=bool(activation.node.get("adopt_callee")))

    # ------------------------------------------------------------------ instructions: calls

    def op_resolve_callee(self, activation: Activation, instruction: dict[str, Any]) -> Generator[Any, Any, None]:
        callee = activation.node["callee"]
        if isinstance(callee, dict):
            yield from self.eval(callee)
            value = self.pop(activation)
            value = value.value if isinstance(value, Cell) else value
            if not isinstance(value, dict) or "$closure" not in value:
                raise MachineError("indirect call target is not a callable")
            activation.local["callee"] = value
            self.hit(activation, "closure")
        elif callee in self.program["definitions"]:
            activation.local["callee"] = callee
            self.hit(activation, "function")
        elif callee in INTRINSICS:
            activation.local["callee"] = callee
            self.hit(activation, "intrinsic")
        else:
            raise MachineError(f"unresolved callee {callee!r}", "F-DIAG-UNBOUND-NAME")

    def op_apply_ownership_modes(self, activation: Activation, instruction: dict[str, Any]) -> None:
        callee = activation.local["callee"]
        if isinstance(callee, str) and callee in INTRINSICS:
            return
        parameters = self.callable_of(callee)["parameters"]
        count = len(activation.node["arguments"])
        if len(parameters) != count:
            raise MachineError(f"call passes {count} arguments for {len(parameters)} parameters")
        stack = self.context.stack
        base = len(stack) - count
        depth = len(self.context.frames)
        tail = activation.rule["form"] == "tail-call"
        for offset, parameter in enumerate(parameters):
            value = stack[base + offset]
            mode = parameter["ownership"]
            if mode not in {"borrow", "borrow-mut"}:
                if isinstance(value, Cell):
                    raise MachineError(f"parameter {parameter['name']} takes ownership of a borrowed argument", "F-DIAG-MOVE-FROM-BORROW")
            elif not tail and not isinstance(value, Cell) and (mode == "borrow-mut" or not self.is_copy(value)):
                # A borrowed temporary stays owned by the calling activation and is dropped after the call.
                # In a tail call no caller remains, so the callee frame adopts the temporary instead.
                temporary = Cell("<temporary>", value, mode == "borrow-mut", depth)
                self.context.cleanup.append({"kind": "drop", "cell": temporary})
                stack[base + offset] = temporary
        if tail:
            # An adopted value whose loan this call passes on moves to the callee's frame.
            adopted = self.frame["adopted"]
            carried: dict[int, tuple[Cell, list[int]]] = {}
            for source, position in activation.node.get("forward", []):
                for cell, sources in adopted:
                    if source in sources:
                        carried.setdefault(id(cell), (cell, []))[1].append(position)
            activation.local["carried"] = list(carried.values())
            for cell, _ in activation.local["carried"]:
                self.context.cleanup[:] = [entry for entry in self.context.cleanup if entry.get("cell") is not cell]
            for value in [activation.local["callee"], *stack[base:]]:
                if isinstance(value, Cell) and value.depth >= depth and id(value) not in carried:
                    raise MachineError(f"a tail call cannot pass a loan of a place owned by the frame it discards ({value.name})", "F-DIAG-LOAN-ESCAPES-FRAME")

    def op_branch_callable(self, activation: Activation, instruction: dict[str, Any]) -> str | None:
        callee = activation.local["callee"]
        return instruction["intrinsic"] if isinstance(callee, str) and callee in INTRINSICS else None

    def op_apply_intrinsic(self, activation: Activation, instruction: dict[str, Any]) -> None:
        name = activation.local.pop("callee")
        count = len(activation.node["arguments"])
        arguments = [self.pop(activation) for _ in range(count)][::-1]
        arguments = [argument.value if isinstance(argument, Cell) else argument for argument in arguments]
        self.trace.append(f"Continue(call:core.{name})")
        if name == "drop":
            if count != 1:
                raise MachineError("drop takes one value")
            self.drop_value(arguments[0])
            self.push(None)
            return
        if name == "-" and count == 1 and isinstance(arguments[0], int) and not isinstance(arguments[0], bool):
            if arguments[0] == INT_MIN:
                self.hit(activation, "trap")
                raise _Protected("Trap", "integer-overflow")
            self.push(-arguments[0])
            return
        if count != 2 or not all(isinstance(argument, int) and not isinstance(argument, bool) for argument in arguments):
            if name in {"==", "!="} and count == 2:
                self.push(COMPARISON[name](arguments[0], arguments[1]))
                return
            raise MachineError(f"intrinsic {name} is applied to unsupported operands")
        left, right = arguments
        if name in COMPARISON:
            self.push(COMPARISON[name](left, right))
            return
        if name == "/":
            if right == 0:
                self.hit(activation, "trap")
                raise _Protected("Trap", "divide-by-zero")
            magnitude = abs(left) // abs(right)
            result = magnitude if (left < 0) == (right < 0) else -magnitude
        else:
            result = ARITHMETIC[name](left, right)
        if not INT_MIN <= result <= INT_MAX:
            self.hit(activation, "trap")
            raise _Protected("Trap", "integer-overflow")
        self.push(result)

    def callable_of(self, callee: Any) -> dict[str, Any]:
        if isinstance(callee, str):
            return self.program["definitions"][callee]
        if callee is self.entry:
            return self.entry_node
        return self.closures[callee["$closure"]]["node"]

    def callable_name(self, callee: Any) -> str:
        if callee is self.entry:
            return "<entry>"
        return callee if isinstance(callee, str) else shown(callee)

    def op_enter_callable(self, activation: Activation, instruction: dict[str, Any]) -> Generator[Any, Any, None]:
        """Move the arguments into a new frame, run the callee to its result, and push that result."""
        count = len(activation.node["arguments"])
        arguments = [self.pop(activation) for _ in range(count)][::-1]
        value = yield from self.invoke(activation.local.pop("callee"), arguments, announce=True)
        self.push(value)

    def new_frame(self, name: str) -> dict[str, Any]:
        frame = {
            "callable": name, "scopes": [{}], "loops": 0, "barriers": 0, "handlers": 0, "active": [], "adopted": [],
            "cleanup_base": len(self.context.cleanup), "stack_base": len(self.context.stack),
        }
        self.context.frames.append(frame)
        self.max_frames = max(self.max_frames, len(self.context.frames))
        return frame

    def enter_frame(self, callee: Any, arguments: list[Any], carried: list[tuple[Cell, list[int]]] = (), adopt_callee: bool = False) -> dict[str, Any]:
        definition = self.callable_of(callee)
        frame = self.new_frame(self.callable_name(callee))
        for cell, positions in carried:
            cell.depth = len(self.context.frames)
            self.context.cleanup.append({"kind": "drop", "cell": cell})
            frame["adopted"].append((cell, set(positions)))
        if adopt_callee and not isinstance(callee, Cell) and not self.is_copy(callee):
            owner = Cell("<adopted>", callee, True, len(self.context.frames))
            self.context.cleanup.append({"kind": "drop", "cell": owner})
            frame["adopted"].append((owner, {-1}))
        if isinstance(callee, dict) and "$closure" in callee:
            for name, cell in self.closures[callee["$closure"]]["environment"].items():
                self.frame["scopes"][-1][name] = cell
        for index, (parameter, argument) in enumerate(zip(definition["parameters"], arguments)):
            borrowed = parameter["ownership"] in {"borrow", "borrow-mut"}
            if borrowed and not isinstance(argument, Cell) and (parameter["ownership"] == "borrow-mut" or not self.is_copy(argument)):
                # A tail call handed over a temporary: this frame owns it, and the parameter is still a
                # loan, so the callee's body means the same thing at every call site.
                adopted = Cell("<adopted>", argument, True, len(self.context.frames))
                self.context.cleanup.append({"kind": "drop", "cell": adopted})
                frame["adopted"].append((adopted, {index}))
                argument = adopted
            self.bind(parameter["name"], argument, parameter["ownership"] == "borrow-mut", owned=not isinstance(argument, Cell))
        return definition

    def invoke(self, callee: Any, arguments: list[Any], announce: bool) -> Generator[Any, Any, Any]:
        """Run a callable to its result, replacing the frame for every tail call."""
        carried: list[tuple[Cell, list[int]]] = []
        adopt_callee = False
        while True:
            definition = self.enter_frame(callee, arguments, carried, adopt_callee)
            frame = self.frame
            if announce:
                self.trace.append(f"Continue(enter:{frame['callable']})")
            transfer = yield from self.run_frame(frame, definition["body"])
            if transfer.edge == "return":
                if frame["callable"] != "<entry>":
                    self.trace.append(f"Continue(return:{frame['callable']}={shown(transfer.value)})")
                return transfer.value
            callee, arguments, announce, carried, adopt_callee = transfer.callee, transfer.arguments, False, transfer.carried, transfer.adopt_callee
            self.trace.append(f"Continue(tail-call:{self.callable_name(callee)})")

    def run_frame(self, frame: dict[str, Any], body: dict[str, Any]) -> Generator[Any, Any, _FrameTransfer]:
        """Evaluate a frame body, run the frame's cleanup, discard the frame, and report how it left."""
        transfer: _Abrupt
        try:
            yield from self.eval(body)
            transfer = _FrameTransfer("return", value=self.context.stack.pop())
        except _LoopTransfer as loop:
            raise MachineError("a loop transfer cannot cross a callable boundary") from loop
        except _Abrupt as caught:
            transfer = caught
        outcome = yield from self.unwind(frame["cleanup_base"], frame["stack_base"], transfer, None)
        self.context.frames.pop()
        if outcome is not transfer and isinstance(transfer, _FrameTransfer):
            for abandoned in [transfer.value, *transfer.arguments, *(cell for cell, _ in transfer.carried), *([transfer.callee] if transfer.adopt_callee else [])]:
                self.drop_value(abandoned)
        if not isinstance(outcome, _FrameTransfer):
            raise outcome
        return outcome

    def hit_rule(self, rule: str, label: str) -> None:
        self.hits.add((rule, label))

    # ------------------------------------------------------------------ instructions: matching

    def matches(self, pattern: Any, value: Any) -> bool:
        value = value.value if isinstance(value, Cell) else value
        if pattern == "_":
            return True
        if isinstance(pattern, dict):
            if "bind" in pattern:
                return True
            if "constructor" in pattern:
                return (
                    isinstance(value, dict) and value.get("$variant") == pattern["constructor"]
                    and len(value["arguments"]) == len(pattern["arguments"])
                    and all(self.matches(p, v) for p, v in zip(pattern["arguments"], value["arguments"]))
                )
            if "record" in pattern:
                return (
                    isinstance(value, dict) and value.get("$record") == pattern["record"]
                    and all(field["name"] in value["fields"] and self.matches(field["pattern"], value["fields"][field["name"]]) for field in pattern["fields"])
                )
            raise MachineError(f"unsupported pattern {pattern!r}")
        return type(pattern) is type(value) and pattern == value

    def bind_borrowed(self, pattern: Any, value: Any) -> None:
        value = value.value if isinstance(value, Cell) else value
        if isinstance(pattern, dict):
            if "bind" in pattern:
                held = value if self.is_copy(value) else Cell("<loan>", value)
                self.frame["scopes"][-1][pattern["bind"]] = Cell(pattern["bind"], held)
            elif "constructor" in pattern:
                for sub, item in zip(pattern["arguments"], value["arguments"]):
                    self.bind_borrowed(sub, item)
            elif "record" in pattern:
                for field in pattern["fields"]:
                    self.bind_borrowed(field["pattern"], value["fields"][field["name"]])

    def op_select_first_matching_arm(self, activation: Activation, instruction: dict[str, Any]) -> None:
        scrutinee = self.pop(activation)
        arms = activation.node[instruction["operand"]]
        for index, arm in enumerate(arms):
            if self.matches(arm["pattern"], scrutinee):
                activation.local.update(arm=arm, scrutinee=scrutinee)
                self.hit(activation, "first-arm" if index == 0 else "later-arm")
                self.trace.append(f"Continue(match:arm-{index})")
                return
        raise MachineError("match is not exhaustive", "F-DIAG-MATCH-NOT-EXHAUSTIVE")

    def op_bind_pattern(self, activation: Activation, instruction: dict[str, Any]) -> Generator[Any, Any, None]:
        pattern = activation.local["arm"]["pattern"]
        scrutinee = activation.local.pop("scrutinee")
        self.frame["scopes"].append({})
        if activation.node["ownership"] == "steal":
            if isinstance(scrutinee, Cell):
                raise MachineError("an ownership match cannot consume a borrowed scrutinee")
            self.hit(activation, "steal")
            yield from self.activate(self.rules[instruction["steal"]], activation.node, pattern=pattern, value=scrutinee)
            return
        self.hit(activation, "borrow")
        if not isinstance(scrutinee, Cell):
            self.context.cleanup.append({"kind": "field", "value": scrutinee})
        self.bind_borrowed(pattern, scrutinee)

    def op_eval_selected_body(self, activation: Activation, instruction: dict[str, Any]) -> Generator[Any, Any, None]:
        arm = activation.local.pop("arm")
        yield from self.eval(arm["body"])
        activation.local["join"] = arm.get("join_drops", [])

    def op_prepare_atomic_consumption(self, activation: Activation, instruction: dict[str, Any]) -> None:
        value = activation.local["value"]
        if isinstance(value, Cell):
            raise MachineError("ownership destructure requires an owned aggregate")

    def op_transfer_or_drop_all_fields(self, activation: Activation, instruction: dict[str, Any]) -> None:
        """Bind transferred parts in declaration order, then drop discarded parts in reverse order."""
        discarded: list[Any] = []

        def consume(pattern: Any, value: Any) -> None:
            if isinstance(pattern, dict) and "bind" in pattern:
                self.hit(activation, "transfer")
                self.bind(pattern["bind"], value)
                self.trace.append(f"Continue(bind:{pattern['bind']}={shown(value)})")
            elif isinstance(pattern, dict) and "constructor" in pattern:
                for sub, item in zip(pattern["arguments"], value["arguments"]):
                    consume(sub, item)
            elif isinstance(pattern, dict) and "record" in pattern:
                named = {field["name"]: field["pattern"] for field in pattern["fields"]}
                for name, item in value["fields"].items():
                    consume(named.get(name, "_"), item)
            else:
                self.hit(activation, "drop")
                discarded.append(value)

        consume(activation.local["pattern"], activation.local["value"])
        for value in reversed(discarded):
            self.drop_value(value)

    def op_invalidate_aggregate(self, activation: Activation, instruction: dict[str, Any]) -> None:
        activation.local.pop("value")

    # ------------------------------------------------------------------ instructions: exceptions

    def op_raise_with_provenance(self, activation: Activation, instruction: dict[str, Any]) -> None:
        value = self.pop(activation)
        envelope = {"value": value, "provenance": self.raises, "suppressed": []}
        self.raises += 1
        self.hit(activation, "raise")
        self.trace.append(f"Raise(value:{shown(value)})")
        raise _Raise(envelope)

    def op_require_active_exception(self, activation: Activation, instruction: dict[str, Any]) -> None:
        if not self.frame["active"]:
            raise MachineError("rethrow outside a catch arm", "F-DIAG-RETHROW-TARGET")

    def op_raise_original_envelope(self, activation: Activation, instruction: dict[str, Any]) -> None:
        active = self.frame["active"][-1]
        active["cell"].state = "moved"
        self.hit(activation, "raise")
        self.trace.append(f"Raise(rethrow:{shown(active['envelope']['value'])})")
        raise _Raise(active["envelope"])

    def op_push_handler(self, activation: Activation, instruction: dict[str, Any]) -> None:
        activation.handler = activation.node[instruction["operand"]]
        self.frame["handlers"] += 1
        self.trace.append("Continue(handler:push)")

    def op_pop_handler(self, activation: Activation, instruction: dict[str, Any]) -> None:
        activation.handler = None
        self.frame["handlers"] -= 1
        self.hit(activation, "normal")
        self.trace.append("Continue(handler:pop)")
        self.join_drops(activation.node.get("join_drops", []))

    def op_match_exception_without_consuming(self, activation: Activation, instruction: dict[str, Any]) -> None:
        activation.local["matched"] = self.matches(activation.node[instruction["operand"]], activation.local["envelope"]["value"])

    def op_branch_catch(self, activation: Activation, instruction: dict[str, Any]) -> str | None:
        if not activation.local.pop("matched"):
            self.hit(activation, "decline")
            return instruction["miss"]
        envelope = activation.local["envelope"]
        self.hit(activation, "match")
        self.trace.append("Continue(catch:match)")
        self.frame["scopes"].append({})
        cell = Cell("<exception>", envelope["value"])
        self.context.cleanup.append({"kind": "drop", "cell": cell})
        self.bind_borrowed(activation.node["pattern"], envelope["value"])
        self.frame["active"].append({"envelope": envelope, "cell": cell})
        activation.local["active"] = True
        return None

    def op_end_catch(self, activation: Activation, instruction: dict[str, Any]) -> Generator[Any, Any, tuple[str, Any]]:
        self.frame["active"].pop()
        del activation.local["active"]
        activation.local["join"] = activation.node.get("join_drops", [])
        yield from self.op_drop_scope(activation, instruction)
        self.op_continue(activation, instruction)
        return ("finish", "handled")

    def op_decline_catch(self, activation: Activation, instruction: dict[str, Any]) -> tuple[str, Any]:
        return ("finish", "declined")

    # ------------------------------------------------------------------ instructions: cleanup

    def op_register_guards(self, activation: Activation, instruction: dict[str, Any]) -> Generator[Any, Any, None]:
        for guard in activation.node[instruction["operand"]]:
            yield from self.activate(self.rules["guard"], guard)

    def op_register_cleanup(self, activation: Activation, instruction: dict[str, Any]) -> None:
        reason = activation.node[instruction["field"]]
        self.context.cleanup.append({"kind": "guard", "reason": reason, "body": activation.node[instruction["operand"]], "activation": activation})
        self.hit(activation, f"register-{reason}")
        self.trace.append(f"Continue(guard:register:{reason})")

    def op_run_cleanup_lifo(self, activation: Activation, instruction: dict[str, Any]) -> Generator[Any, Any, None]:
        self.hit(activation, "success")
        context = self.context
        while len(context.cleanup) > activation.cleanup_height:
            entry = context.cleanup.pop()
            try:
                yield from self.run_entry(entry, "success")
            except _Abrupt:
                self.hit(activation, "guard-failure-primary")
                raise

    # ------------------------------------------------------------------ instructions: aggregates

    def op_eval_fields_left_to_right(self, activation: Activation, instruction: dict[str, Any]) -> Generator[Any, Any, None]:
        activation.local["fields"] = {}
        for field in activation.node[instruction["operand"]]:
            if field["name"] in activation.local["fields"]:
                raise MachineError(f"record field {field['name']} is initialized twice")
            try:
                yield from self.eval(field["value"])
            except _Abrupt:
                self.hit(activation, "partial-failure")
                raise
            value = self.pop(activation)
            activation.local["fields"][field["name"]] = value
            self.context.cleanup.append({"kind": "field", "value": value})

    def op_assemble_record(self, activation: Activation, instruction: dict[str, Any]) -> None:
        del self.context.cleanup[activation.cleanup_height :]
        record = {"$record": activation.node["type"], "fields": activation.local.pop("fields")}
        self.push(record)
        self.hit(activation, "complete")
        self.trace.append(f"Continue(record:{shown(record)})")

    def op_assemble_variant(self, activation: Activation, instruction: dict[str, Any]) -> None:
        count = len(activation.node["arguments"])
        arguments = [self.pop(activation) for _ in range(count)][::-1]
        value = {"$variant": activation.node["case"], "arguments": arguments}
        self.push(value)
        self.hit(activation, "unit" if not arguments else "payload")
        self.trace.append(f"Continue(variant:{shown(value)})")

    def op_project_field(self, activation: Activation, instruction: dict[str, Any]) -> None:
        target = self.pop(activation)
        name = activation.node["name"]
        aggregate = target.value if isinstance(target, Cell) else target
        if isinstance(aggregate, Cell):
            aggregate = aggregate.value
        if not isinstance(aggregate, dict) or "$record" not in aggregate or name not in aggregate["fields"]:
            raise MachineError(f"value has no field {name!r}", "F-DIAG-UNKNOWN-MEMBER")
        value = aggregate["fields"][name]
        if self.is_copy(value):
            self.hit(activation, "copy")
            self.trace.append(f"Continue(member:{name}={shown(value)})")
            self.push(value)
            if not isinstance(target, Cell):
                self.drop_value(target)
            return
        if not isinstance(target, Cell):
            raise MachineError(f"field {name!r} cannot be moved out of its aggregate", "F-DIAG-PARTIAL-MOVE")
        self.hit(activation, "loan")
        self.trace.append(f"Continue(member:{name}=&{shown(value)})")
        self.push(Cell("<field>", value))

    def op_capture_environment(self, activation: Activation, instruction: dict[str, Any]) -> None:
        environment: dict[str, Cell] = {}
        owned: list[Cell] = []
        for capture in activation.node["captures"]:
            source = self.lookup(capture["name"])
            if source.state != "live":
                raise MachineError(f"capture of {source.state} place {capture['name']!r}")
            mode = capture["ownership"]
            if mode in {"borrow", "borrow-mut"}:
                environment[capture["name"]] = Cell(capture["name"], source.value if isinstance(source.value, Cell) else source, mode == "borrow-mut")
            elif mode == "copy":
                if not self.is_copy(source.value):
                    raise MachineError(f"copy capture of non-Copy binding {capture['name']!r}", "F-DIAG-COPY-EVIDENCE")
                environment[capture["name"]] = Cell(capture["name"], source.value)
            elif mode == "steal":
                if not self.is_copy(source.value):
                    source.state = "moved"
                environment[capture["name"]] = Cell(capture["name"], source.value)
                owned.append(environment[capture["name"]])
            else:
                raise MachineError(f"capture mode {mode} is outside the reference machine")
        activation.local["environment"] = environment
        activation.local["owned"] = owned

    def op_make_closure(self, activation: Activation, instruction: dict[str, Any]) -> None:
        node = self.program["definitions"][activation.node["function"]] if "function" in activation.node else activation.node
        self.closures.append({"node": node, "environment": activation.local.pop("environment"), "owned": activation.local.pop("owned")})
        value = {"$closure": len(self.closures) - 1}
        self.push(value)
        self.hit(activation, "closure")
        self.trace.append(f"Continue(closure:{shown(value)})")

    # ------------------------------------------------------------------ instructions: host boundary

    def fingerprint(self, payload: Any) -> str:
        text = json.dumps(exported(payload), ensure_ascii=False, separators=(",", ":"), sort_keys=True)
        return hashlib.sha256(text.encode()).hexdigest()

    def accept(self, state: dict[str, Any], event: dict[str, Any]) -> str:
        if self.replay is None:
            raise MachineError("the host boundary requires the replay automaton")
        outcome = self.replay(state, event)
        state.update(outcome["state"])
        return outcome["output"]

    def op_check_live_grant_and_policy(self, activation: Activation, instruction: dict[str, Any]) -> None:
        from admission import grant_allows

        operation = activation.node["operation"]
        count = len(activation.node["arguments"])
        arguments = self.context.stack[len(self.context.stack) - count :]
        if not grant_allows(self.grants, operation, arguments):
            self.hit(activation, "denied")
            raise _Protected("Denied", operation)

    def op_journal_request(self, activation: Activation, instruction: dict[str, Any]) -> None:
        operation = activation.node["operation"]
        count = len(activation.node["arguments"])
        arguments = [self.pop(activation) for _ in range(count)][::-1]
        sequence = self.effects["next_sequence"]
        request = f"{operation}#{int(sequence, 16)}"
        event = {
            "kind": "effect", "generation": self.effects["generation"], "sequence": sequence,
            "request_id": request, "fingerprint": self.fingerprint([operation, arguments]),
        }
        if self.accept(self.effects, event) != "accepted-dispatch-once":
            raise MachineError("a fresh effect request was not accepted by the replay automaton")
        self.journal.append(f"request:{request}")
        activation.local["request"] = {"id": request, "sequence": sequence}
        for revocation in self.host.get("revocations", []):
            if revocation["after_sequence"] == int(sequence, 16):
                # The host withdraws a grant after this request was accepted; every later dispatch rechecks.
                self.host_log.append(f"revoke:{revocation['operation']}")
                self.grants[revocation["operation"]] = False
                self.root_grants[revocation["operation"]] = False
                for task in self.tasks:
                    task["grants"][revocation["operation"]] = False

    def op_await_correlated_resume(self, activation: Activation, instruction: dict[str, Any]) -> Generator[Any, Any, None]:
        request = activation.local.pop("request")
        if self.journal[-1] != f"request:{request['id']}":
            raise MachineError("a request must be journaled before it is exposed")
        self.trace.append(f"Await({request['id']})")
        outcome = yield ("await", request)
        if "value" in outcome:
            self.hit(activation, "resumed-value")
            self.trace.append(f"Continue(resume:{request['id']}={shown(outcome['value'])})")
            self.push(outcome["value"])
        elif "raise" in outcome:
            self.hit(activation, "resumed-raise")
            envelope = {"value": outcome["raise"], "provenance": self.raises, "suppressed": []}
            self.raises += 1
            self.trace.append(f"Raise(value:{shown(outcome['raise'])})")
            raise _Raise(envelope)
        else:
            self.hit(activation, "resumed-protected")
            raise _Protected(outcome["protected"], outcome.get("detail"))

    def op_journal_event(self, activation: Activation, instruction: dict[str, Any]) -> None:
        activation.local["event"] = f"{activation.node['operation']}:{shown(self.pop(activation))}"
        self.journal.append(f"event:{activation.local['event']}")

    def op_emit(self, activation: Activation, instruction: dict[str, Any]) -> None:
        event = activation.local.pop("event")
        if self.journal[-1] != f"event:{event}":
            raise MachineError("an event must be journaled before it is exposed")
        self.hit(activation, "emitted")
        self.trace.append(f"Emit({event})")

    def deliver(self, request: dict[str, Any]) -> dict[str, Any] | None:
        """Feed scripted host messages through the replay automaton until one resumes ``request``."""
        self.resumes["next_sequence"] = request["sequence"]
        self.resumes["outstanding"] = request["id"]
        while self.messages:
            message = self.messages.pop(0)
            outcome = message["outcome"]
            event = {
                "kind": "resume",
                "generation": f"{message['generation']:016x}",
                "sequence": f"{message['sequence']:016x}",
                "request_id": message["request"],
                "fingerprint": self.fingerprint(outcome),
            }
            verdict = self.accept(self.resumes, event)
            self.host_log.append(f"resume:{message['request']}:{verdict}")
            if verdict == "accepted-dispatch-once":
                if message["request"] != request["id"]:
                    raise MachineError("the replay automaton accepted a resume for a request that is not outstanding")
                self.journal.append(f"resume:{request['id']}")
                self.resumes["outstanding"] = None
                return outcome
        return None

    # ------------------------------------------------------------------ instructions: fibers

    def op_make_generator(self, activation: Activation, instruction: dict[str, Any]) -> None:
        count = len(activation.node["arguments"])
        arguments = [self.pop(activation) for _ in range(count)][::-1]
        if any(isinstance(argument, Cell) for argument in arguments):
            # A dormant or suspended fiber outlives the call that made it, so it cannot hold a loan.
            raise MachineError("a generator cannot be given a borrowed argument", "F-DIAG-TRANSFER-REQUIRES-OWNED")
        self.fibers.append({
            "callee": activation.node["callee"], "arguments": arguments, "state": "dormant",
            "context": Context(), "generator": None, "buffer": None, "failure": None,
        })
        value = {"$fiber": len(self.fibers) - 1}
        self.push(value)
        self.hit(activation, "created")
        self.trace.append(f"Continue(fiber:new:{shown(value)})")

    def op_suspend_to_stepper(self, activation: Activation, instruction: dict[str, Any]) -> Generator[Any, Any, None]:
        if not self.frame.get("fiber"):
            raise MachineError("yield outside a generator body", "F-DIAG-YIELD-TARGET")
        if self.frame["barriers"]:
            raise MachineError("a guard body cannot yield")
        value = self.pop(activation)
        self.trace.append(f"Continue(fiber:yield:{shown(value)})")
        # The yielded value now belongs to the handle.  What comes back is the option of a reply.
        reply = yield ("yield", value)
        self.hit(activation, "replied" if reply["$variant"] == "some" else "advanced")
        self.push(reply)

    def fiber_main(self, fiber: dict[str, Any]) -> Generator[Any, Any, Any]:
        definition = self.enter_frame(fiber["callee"], fiber["arguments"])
        frame = self.frame
        frame["fiber"] = True
        transfer = yield from self.run_frame(frame, definition["body"])
        if transfer.edge != "return":
            raise MachineError("a generator body holds no tail position; resolution must not mark a tail call there")
        if definition["result"] == "unit":
            # A generator declared to return unit has no last item, whatever its body's final value.
            self.drop_value(transfer.value)
            return None
        return transfer.value

    def advance(self, handle: dict[str, Any], reply: Any) -> Generator[Any, Any, None]:
        """Run a fiber to its next `yield` or to its end, in its own context."""
        fiber = self.fibers[handle["$fiber"]]
        self.trace.append(f"Continue(fiber:advance:{shown(handle)})")
        outer = self.context
        self.context = fiber["context"]
        try:
            if fiber["state"] == "dormant":
                fiber["generator"] = self.fiber_main(fiber)
                send: Any = None
            else:
                send = reply
            fiber["state"] = "running"
            while True:
                try:
                    event = fiber["generator"].send(send)
                except StopIteration as stop:
                    # A returned value of the item type is the last item; a unit return adds none.
                    fiber["state"], fiber["buffer"] = ("done", None) if stop.value is None else ("last", stop.value)
                    self.trace.append(f"Continue(fiber:return:{shown(stop.value)})")
                    return
                except _Abrupt:
                    fiber["state"] = "done"
                    raise
                if event[0] == "yield":
                    fiber["state"], fiber["buffer"] = "suspended", event[1]
                    return
                self.context = outer
                send = yield event
                self.context = fiber["context"]
        finally:
            self.context = outer

    def cancel_fiber(self, handle: dict[str, Any]) -> None:
        """End a fiber nobody can advance again: its cleanup runs here, in the dropping execution."""
        fiber = self.fibers[handle["$fiber"]]
        state = fiber["state"]
        if state == "done":
            return
        self.lifecycle_marks.add(len(self.trace))
        self.trace.append(f"Continue(drop:{shown(handle)})")
        self.drops.append(shown(handle))
        fiber["state"] = "done"
        if state == "dormant":
            for argument in reversed(fiber["arguments"]):
                self.drop_value(argument)
            return
        if state in {"suspended", "last"}:
            self.drop_value(fiber["buffer"])
        if state != "suspended":
            return
        outer = self.context
        self.context = fiber["context"]
        try:
            fiber["generator"].throw(_Protected("Cancel", "handle-dropped"))
        except (StopIteration, _Abrupt):
            pass
        else:
            raise MachineError("cleanup that suspends while its generator is being dropped is outside the reference machine")
        finally:
            self.context = outer

    def op_generator_operation(self, activation: Activation, instruction: dict[str, Any]) -> Generator[Any, Any, None]:
        node = activation.node
        operation = node["operation"]
        arguments = [self.pop(activation) for _ in node["arguments"]][::-1]
        target = self.pop(activation)
        handle = target
        while isinstance(handle, Cell):
            handle = handle.value
        if not isinstance(handle, dict) or "$fiber" not in handle:
            raise MachineError(f"{operation} requires a generator")
        fiber = self.fibers[handle["$fiber"]]

        def finished() -> _Raise:
            # Reading past the end is an ordinary exception: the value RangeEmpty, catchable by the reader.
            self.hit(activation, "finished")
            envelope = {"value": {"$record": "RangeEmpty", "fields": {}}, "provenance": self.raises, "suppressed": []}
            self.raises += 1
            self.trace.append(f"Raise(value:{shown(envelope['value'])})")
            return _Raise(envelope)

        def prime() -> Generator[Any, Any, None]:
            if fiber["state"] == "dormant":
                self.hit(activation, "primed")
                yield from self.advance(handle, None)

        def current() -> Any:
            value = fiber["buffer"]
            return value if self.is_copy(value) else Cell("<front>", value)

        try:
            if operation == "start":
                # Runs to the first yield now.  A failure is kept and raised at the first read.
                try:
                    yield from prime()
                    self.hit(activation, "started")
                except _Raise as failure:
                    fiber["state"], fiber["failure"] = "failed", failure
                    self.hit(activation, "start-failed")
                self.push(handle)
                return
            if fiber["state"] == "failed":
                failure, fiber["failure"], fiber["state"] = fiber["failure"], None, "done"
                self.hit(activation, "raised")
                raise failure
            yield from prime()
            if operation == "empty?":
                self.hit(activation, "empty")
                self.push(fiber["state"] == "done")
            elif operation == "front":
                if fiber["state"] == "done":
                    raise finished()
                self.hit(activation, "front")
                self.push(current())
            else:
                if fiber["state"] == "done" or (operation == "reply" and fiber["state"] == "last"):
                    for argument in arguments:
                        self.drop_value(argument)
                    raise finished()
                self.drop_value(fiber["buffer"])
                if fiber["state"] == "last":
                    fiber["state"], fiber["buffer"] = "done", None
                elif operation == "pop-front":
                    yield from self.advance(handle, {"$variant": "none", "arguments": []})
                else:
                    payload = arguments[0] if len(arguments) == 1 else {"$tuple": arguments}
                    yield from self.advance(handle, {"$variant": "some", "arguments": [payload]})
                if operation == "pop-front":
                    self.hit(activation, "advanced")
                    self.push(None)
                else:
                    if fiber["state"] == "done":
                        raise finished()
                    self.hit(activation, "replied")
                    self.push(current())
        finally:
            if not isinstance(target, Cell) and operation != "start":
                self.cancel_fiber(handle)

    # ------------------------------------------------------------------ instructions: tasks

    def op_reserve_scheduler_slot(self, activation: Activation, instruction: dict[str, Any]) -> None:
        capacity = self.host.get("task_capacity")
        live = sum(1 for task in self.tasks if task["state"] in {"runnable", "running"})
        if capacity is not None and live >= capacity:
            self.hit(activation, "capacity-exhausted")
            raise _Protected("ResourceExhausted", "scheduler-capacity")

    def op_linearize_spawn(self, activation: Activation, instruction: dict[str, Any]) -> None:
        closure = self.pop(activation)
        if not isinstance(closure, dict) or "$closure" not in closure:
            raise MachineError("spawn requires an owned callable")
        record = self.closures[closure["$closure"]]
        if record["node"]["parameters"]:
            raise MachineError("spawn requires a zero-argument callable")
        if any(isinstance(cell.value, Cell) for cell in record["environment"].values()):
            raise MachineError("a spawned callable cannot capture a loan", "F-DIAG-TRANSFER-REQUIRES-OWNED")
        allowed = self.host.get("child_grants")
        grants = dict(self.grants) if allowed is None else {name: (live if allowed.get(name, False) else False) for name, live in self.grants.items()}
        self.tasks.append({"closure": closure, "state": "runnable", "context": Context(), "grants": grants})
        value = {"$task": len(self.tasks) - 1}
        self.push(value)
        self.hit(activation, "published")
        self.trace.append(f"Continue(spawn:{shown(value)})")

    def op_consume_task(self, activation: Activation, instruction: dict[str, Any]) -> None:
        handle = self.pop(activation)
        if not isinstance(handle, dict) or "$task" not in handle:
            raise MachineError(f"{activation.node['form']} requires an owned task handle")
        task = self.tasks[handle["$task"]]
        if task["state"] != "runnable":
            raise MachineError(f"{activation.node['form']} of a {task['state']} task handle", "F-DIAG-USE-AFTER-MOVE")
        activation.local["task"] = handle

    def op_await_task(self, activation: Activation, instruction: dict[str, Any]) -> Generator[Any, Any, None]:
        handle = activation.local.pop("task")
        task = self.tasks[handle["$task"]]
        task["state"] = "running"
        self.trace.append(f"Continue(join:{shown(handle)})")
        outer, outer_grants = self.context, self.grants
        self.context, self.grants = task["context"], task["grants"]
        generator = self.invoke(task["closure"], [], announce=False)
        send: Any = None
        try:
            while True:
                try:
                    event = generator.send(send)
                except StopIteration as stop:
                    value = stop.value
                    break
                except _Raise:
                    self.hit(activation, "raised")
                    raise
                except _Protected:
                    self.hit(activation, "protected")
                    raise
                if event[0] == "yield":
                    raise MachineError("yield outside a fiber body", "F-DIAG-YIELD-TARGET")
                self.context, self.grants = outer, outer_grants
                send = yield event
                self.context, self.grants = task["context"], task["grants"]
        finally:
            task["state"] = "done"
            self.context, self.grants = outer, outer_grants
        self.hit(activation, "returned")
        self.push(value)
        self.trace.append(f"Continue(joined:{shown(handle)}={shown(value)})")

    def op_cancel_task(self, activation: Activation, instruction: dict[str, Any]) -> None:
        handle = activation.local.pop("task")
        task = self.tasks[handle["$task"]]
        task["state"] = "cancelled"
        self.hit(activation, "cancelled")
        self.trace.append(f"Continue(cancel:{shown(handle)})")
        self.drop_value(task["closure"])

    # ------------------------------------------------------------------ terminal rules

    def op_require_cleanup_complete(self, activation: Activation, instruction: dict[str, Any]) -> None:
        if self.context.cleanup or self.context.frames:
            raise MachineError("a terminal transition was reached with live cleanup obligations")

    def op_commit_transaction(self, activation: Activation, instruction: dict[str, Any]) -> None:
        self.transaction = "committed"

    def op_discard_vm_mutation(self, activation: Activation, instruction: dict[str, Any]) -> None:
        self.transaction = "discarded"

    def op_complete(self, activation: Activation, instruction: dict[str, Any]) -> tuple[str, Any]:
        value = activation.local["value"]
        self.hit(activation, "committed")
        self.trace.append(f"Complete([{shown(value)}])")
        self.terminal = {"kind": "complete", "values": [exported(value)]}
        return ("finish", None)

    def op_fail(self, activation: Activation, instruction: dict[str, Any]) -> tuple[str, Any]:
        envelope = activation.local["envelope"]
        self.hit(activation, "uncaught-exception")
        self.trace.append("Fail(uncaught-exception)")
        self.terminal = {
            "kind": "fail", "diagnostic": "uncaught-exception",
            "exception": {"value": exported(envelope["value"]), "provenance": envelope["provenance"], "suppressed": envelope["suppressed"]},
        }
        return ("finish", None)

    def op_protected_terminal(self, activation: Activation, instruction: dict[str, Any]) -> tuple[str, Any]:
        edge: _Protected = activation.local["edge"]
        self.hit(activation, edge.edge)
        self.trace.append(edge.spelling())
        self.terminal = {"kind": "protected", "edge": edge.edge, "detail": edge.detail, "suppressed": edge.suppressed}
        return ("finish", None)

    # ------------------------------------------------------------------ driver

    def main(self) -> Generator[Any, Any, None]:
        blank = {"form": "terminal"}
        try:
            value = yield from self.invoke(self.entry, [], announce=False)
        except _Raise as raised:
            self.terminate("fail", blank, envelope=raised.envelope)
        except _Protected as edge:
            self.terminate("protected-edge", blank, edge=edge)
        else:
            self.terminate("complete", blank, value=value)

    def terminate(self, form: str, node: dict[str, Any], **local: Any) -> None:
        rule = self.rules[form]
        activation = Activation(rule, node, self.context)
        activation.local.update(local)
        for index, instruction in enumerate(rule["program"]):
            self.instructions_run.add((rule["rule"], index))
            if isinstance(getattr(self, f"op_{instruction['op']}")(activation, instruction), tuple):
                return
        raise MachineError(f"{rule['rule']} produced no terminal")

    def admit(self) -> None:
        """Preflight admission: decide from the manifest whether this program may start at all."""
        from admission import admit, manifest_of_program

        if self.host.get("admission", {}).get("mode") != "preflight":
            return
        decision = admit(manifest_of_program(self.program), self.host)
        self.host_log.extend(decision["log"])
        self.grants.clear()
        self.grants.update(decision["grants"])
        if decision["refused"]:
            self.transaction = "not-started"
            self.trace.append(f"NotAdmitted({'; '.join(decision['refused'])})")
            self.terminal = {"kind": "not-admitted", "refused": decision["refused"]}

    def run(self) -> dict[str, Any]:
        generator = self.main()
        send: Any = None
        self.admit()
        while self.terminal is None:
            try:
                event = generator.send(send)
            except StopIteration:
                break
            if event[0] == "yield":
                raise MachineError("yield outside a fiber body", "F-DIAG-YIELD-TARGET")
            request = event[1]
            outcome = self.deliver(request)
            if outcome is None:
                self.hit_rule(self.pending_rule(request), "parked")
                self.terminal = {"kind": "await", "request": request["id"]}
                break
            send = outcome
        if self.terminal is None:
            raise MachineError("execution ended without a terminal transition")
        if self.terminal["kind"] not in {"await", "not-admitted"}:
            self.resumes["terminal"] = True
            self.effects["terminal"] = True
            self.deliver({"id": "<none>", "sequence": self.resumes["next_sequence"]})
        return {
            "trace": self.trace,
            "observable": self.observable(),
            "terminal": self.terminal,
            "state": {
                "transaction": self.transaction,
                "journal": self.journal,
                "drops": self.drops,
                "host_log": self.host_log,
                "reaper": self.reaper,
                "max_frames": self.max_frames,
            },
        }

    def observable(self) -> list[str]:
        """What an implementation must reproduce in order: external transitions and lifecycle drops."""
        return [
            entry for index, entry in enumerate(self.trace)
            if not entry.startswith("Continue(") or index in self.lifecycle_marks
        ]

    def pending_rule(self, request: dict[str, Any]) -> str:
        operation = request["id"].rsplit("#", 1)[0]
        kind = self.program.get("operations", {}).get(operation, {}).get("kind", "effect")
        return self.rules[kind]["rule"]


def prepare(ast: dict[str, Any], context: dict[str, Any] | None) -> dict[str, Any]:
    """Resolve one program and attach the declarations its context supplies."""
    context = context or {}
    program = resolve(ast, context.get("operations"))
    program["operations"] = context.get("operations", {})
    program["lifecycle"] = context.get("lifecycle", [])
    program["copy_types"] = context.get("copy_types", [])
    return program


def execute(rules: dict[str, Any], ast: dict[str, Any], context: dict[str, Any] | None, replay: Any = None, program: dict[str, Any] | None = None) -> tuple[dict[str, Any], Machine]:
    """Resolve and run one program; ``context`` carries host operations, grants, and scripted resumes."""
    context = context or {}
    if program is None:
        program = prepare(ast, context)
        # The machine drops at joins where the static pass says to, so an unannotated program
        # would run with a different drop order.  A program the pass rejects runs unannotated and
        # is stopped by the machine's own checks.
        from static_check import StaticError, static_check

        try:
            static_check(program)
        except StaticError:
            pass
    machine = Machine(rules, program, context.get("host"), replay)
    return machine.run(), machine
