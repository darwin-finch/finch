#!/usr/bin/env python3
"""Lower a resolved, statically checked program to Finch typed stack IR version 6.

Input is the program ``reference_machine.prepare`` resolves and ``static_check`` annotates: every
place read carries its compile-time ownership decision and every node its static type.  Lowering
uses only those annotations.  It places every drop, splits cleanup regions where a binding is
moved, and emits inline cleanup on each normal exit path, so the executor needs no ownership state.

``docs/language/semantics/ir.json`` states the instruction set and the lowering rules this module
implements.
"""

from __future__ import annotations

from typing import Any, Callable

from reference_machine import INTRINSICS, MachineError
from static_check import Checker, loan_of


class LoweringGap(MachineError):
    """A program the reference lowering does not handle yet; never a statement about the language."""


STACK_EFFECT = {
    "constant": 1, "drop": -1, "drop_value": -1, "dup": 1, "local_get": 1, "local_set": -1, "local_ref": 1,
    "ref_get": 0, "ref_set": -2, "capture_get": 1, "record_get": 0, "emit": -1, "end_catch": 0,
    "unwind_is_cancel": 1, "safepoint": 0, "yield": 0, "spawn": 0, "join": 0, "cancel": 0,
}
TERMINATORS = {"jump", "branch", "branch_variant", "return", "raise", "rethrow", "resume_unwind", "trap", "tail_call", "tail_call_closure"}


class Construct:
    """One open cleanup obligation: a region for abrupt exits and inline code for normal ones."""

    _next = 0

    def __init__(self, kind: str, target: int, depth: int, on_exit: Callable[[], None] | None):
        Construct._next += 1
        self.id = Construct._next
        self.kind = kind
        self.target = target
        self.depth = depth
        self.on_exit = on_exit


class Var:
    def __init__(self, slot: int, reference: bool = False, capture: bool = False, construct: Construct | None = None):
        self.slot = slot
        self.reference = reference  # the slot holds a slot reference, not the value
        self.capture = capture
        self.construct = construct


class FunctionBuilder:
    def __init__(self, lowering: "Lowering", name: str, parameters: int):
        self.lowering = lowering
        self.name = name
        self.parameters = parameters
        self.locals = parameters
        self.blocks: list[dict[str, Any]] = []
        self.regions: list[dict[str, Any]] = []
        self.region_ids: dict[tuple[int, ...], int] = {}
        self.open: list[Construct] = []
        self.loops: list[dict[str, Any]] = []
        self.height = 0
        self.dead = False
        self.resume_slot: int | None = None
        self.current = self.new_block()

    # ------------------------------------------------------------------ blocks and regions

    def region(self, stack: list[Construct]) -> int | None:
        if not stack:
            return None
        key = tuple(construct.id for construct in stack)
        if key not in self.region_ids:
            parent = self.region(stack[:-1])
            top = stack[-1]
            self.regions.append({"kind": top.kind, "target": top.target, "parent": parent, "depth": top.depth})
            self.region_ids[key] = len(self.regions) - 1
        return self.region_ids[key]

    def new_block(self) -> int:
        self.blocks.append({"id": len(self.blocks), "region": self.region(self.open), "instructions": []})
        return len(self.blocks) - 1

    def switch(self, block: int, height: int) -> None:
        self.current = block
        self.height = height
        self.dead = False

    def emit(self, op: str, delta: int | None = None, **fields: Any) -> None:
        if self.dead:
            return
        self.blocks[self.current]["instructions"].append({"op": op, **fields})
        if op in TERMINATORS:
            self.dead = True
            return
        self.height += STACK_EFFECT[op] if delta is None else delta

    def continue_in_new_block(self) -> None:
        """Start a block for the current construct stack and fall into it."""
        if self.dead:
            return
        height = self.height
        block = self.new_block()
        self.emit("jump", target=block)
        self.switch(block, height)

    def local(self) -> int:
        self.locals += 1
        return self.locals - 1

    # ------------------------------------------------------------------ constructs

    def open_construct(self, construct: Construct) -> None:
        self.open.append(construct)
        self.continue_in_new_block()

    def disarm(self, construct: Construct) -> None:
        """The obligation moved elsewhere: later code is no longer covered by this construct."""
        if construct in self.open:
            self.open.remove(construct)
            self.continue_in_new_block()

    def close(self, construct: Construct) -> None:
        """Normal exit of a construct: leave its region, then run its inline cleanup."""
        if construct not in self.open:
            return
        self.open.remove(construct)
        self.continue_in_new_block()
        if construct.on_exit is not None and not self.dead:
            construct.on_exit()

    def handler(self, depth: int, body: Callable[[], None], extra: int = 0) -> int:
        """Emit a handler block under the current construct stack and return its id."""
        saved = (self.current, self.height, self.dead, list(self.open))
        block = self.new_block()
        self.switch(block, depth + extra)
        body()
        self.current, self.height, self.dead = saved[0], saved[1], saved[2]
        self.open = saved[3]
        return block

    def drop_slot_construct(self, slot: int) -> Construct:
        def drop() -> None:
            self.emit("local_get", index=slot)
            self.emit("drop_value")

        depth = self.height

        def handler() -> None:
            drop()
            self.emit("resume_unwind")

        return Construct("cleanup", self.handler(depth, handler), depth, drop)

    def exit_to(self, count: int) -> list[Construct]:
        """Run the normal-exit cleanup of every construct above ``count``, innermost first."""
        saved = list(self.open)
        for construct in reversed(saved[count:]):
            self.close(construct)
        return saved

    def branches(self, arms: list[tuple[int, int, Callable[[], None]]], height_after: int) -> None:
        """Lower alternative arms and reconcile which outer obligations survive the join."""
        snapshot = list(self.open)
        ends = []
        for block, height, body in arms:
            self.open = list(snapshot)
            self.switch(block, height)
            body()
            ends.append((self.current, list(self.open), self.dead, self.height))
        self.reconcile(snapshot, ends, height_after)

    def reconcile(self, snapshot: list[Construct], ends: list[tuple[int, list[Construct], bool, int]], height_after: int) -> None:
        """Join paths: an obligation survives only if every live path still holds it; the others drop it first."""
        live = [end for end in ends if not end[2]]
        survivors = [construct for construct in snapshot if all(construct in end[1] for end in live)]
        self.open = list(survivors)
        merge = self.new_block()
        for block, arm_open, _, height in live:
            self.open = arm_open
            self.switch(block, height)
            for construct in reversed([c for c in arm_open if c not in survivors]):
                self.close(construct)
            self.emit("jump", target=merge)
        self.open = list(survivors)
        self.switch(merge, height_after)
        self.dead = not live

    def finish(self) -> dict[str, Any]:
        for block in self.blocks:
            instructions = block["instructions"]
            if not instructions or instructions[-1]["op"] not in TERMINATORS:
                instructions.append({"op": "trap", "code": "unreachable"})
        return {
            "name": self.name, "parameters": self.parameters, "locals": self.locals, "entry": 0,
            "blocks": self.blocks, "regions": self.regions,
        }


class Lowering:
    def __init__(self, program: dict[str, Any], checker: Checker):
        self.program = program
        self.checker = checker
        self.functions: dict[str, dict[str, Any]] = {}
        self.counter = 0

    # ------------------------------------------------------------------ helpers

    def droppable(self, node: dict[str, Any]) -> bool:
        return self.droppable_type(node.get("static_type", "unknown"))

    def droppable_type(self, type_: Any) -> bool:
        return not loan_of(type_) and not self.checker.is_copy(type_)

    def module(self) -> dict[str, Any]:
        for name, definition in self.program["definitions"].items():
            self.function(name, definition["parameters"], [], definition["body"], kind="function")
        self.function("<entry>", [], [], self.program["body"], kind="entry")
        return {"version": 6, "entry": "<entry>", "functions": self.functions}

    def function(self, name: str, parameters: list[dict[str, Any]], captures: list[dict[str, Any]], body: dict[str, Any], kind: str, resume: dict[str, Any] | None = None) -> None:
        builder = FunctionBuilder(self, name, len(parameters) + (1 if resume else 0))
        environment: dict[str, Var] = {}
        for index, capture in enumerate(captures):
            environment[capture["name"]] = Var(index, reference=capture["ownership"] == "borrow-mut", capture=True)
        owned: list[Construct] = []
        if resume is not None:
            variable = environment[resume["name"]] = Var(0)
            builder.resume_slot = 0
            if self.droppable_type(self.checker.parse_type(resume["type"])):
                variable.construct = builder.drop_slot_construct(0)
                builder.open_construct(variable.construct)
                owned.append(variable.construct)
        for index, parameter in enumerate(parameters):
            mode = parameter["ownership"]
            variable = Var(index, reference=mode == "borrow-mut")
            if mode == "steal" and self.droppable_type(self.checker.parse_type(parameter["type"])):
                variable.construct = builder.drop_slot_construct(index)
                builder.open_construct(variable.construct)
                owned.append(variable.construct)
            environment[parameter["name"]] = variable
        self.functions[name] = {}  # reserve the name so recursive references resolve
        self.expression(builder, body, environment)
        definition = self.program["definitions"].get(name, {}) if kind == "function" else {}
        if definition.get("generator") and definition["result"] == "unit" and not builder.dead:
            # A generator declared to return unit has no last item, whatever its body's final value.
            self.discard(builder, body)
            builder.emit("constant", value=None)
        for construct in reversed(owned):
            builder.close(construct)
        builder.emit("return")
        self.functions[name] = builder.finish()

    def discard(self, builder: FunctionBuilder, node: dict[str, Any]) -> None:
        builder.emit("drop_value" if self.droppable(node) else "drop")

    # ------------------------------------------------------------------ operands

    def operands(self, builder: FunctionBuilder, nodes: list[dict[str, Any]], environment: dict[str, Var], modes: list[str] | None = None) -> list[tuple[int, Construct | None, str]]:
        """Evaluate operands left to right, leaving them on the stack in order.

        An owned value that needs a drop cannot sit on the operand stack while a later operand may
        fail, so when any operand is droppable (or must be lent exclusively from a temporary) every
        operand is held in a temporary slot and reloaded.  Returned entries describe temporaries
        that are still guarded: the caller either hands them on (disarm) or keeps and drops them.
        """
        modes = modes or ["value"] * len(nodes)
        needs_slots = any(
            self.droppable(node) or (mode == "borrow-mut" and not loan_of(node.get("static_type")))
            for node, mode in zip(nodes, modes)
        )
        if not needs_slots:
            for node in nodes:
                self.expression(builder, node, environment)
            return []
        held: list[tuple[int, Construct | None, str]] = []
        for node, mode in zip(nodes, modes):
            self.expression(builder, node, environment)
            slot = builder.local()
            builder.emit("local_set", index=slot)
            construct = None
            if self.droppable(node) and not builder.dead:
                construct = builder.drop_slot_construct(slot)
                builder.open_construct(construct)
            lend = mode == "borrow-mut" and not loan_of(node.get("static_type"))
            held.append((slot, construct, "lend" if lend else mode))
        for slot, _, mode in held:
            builder.emit("local_ref" if mode == "lend" else "local_get", index=slot)
        return held

    def consume(self, builder: FunctionBuilder, held: list[tuple[int, Construct | None, str]]) -> None:
        """Every held temporary has been moved into the value just built."""
        for _, construct, _ in reversed(held):
            if construct is not None:
                builder.disarm(construct)

    # ------------------------------------------------------------------ expressions

    def expression(self, builder: FunctionBuilder, node: dict[str, Any], environment: dict[str, Var]) -> None:
        if builder.dead:
            return
        form = node["form"].replace("-", "_")
        handler = getattr(self, "lower_" + form, None)
        if handler is None:
            raise LoweringGap(f"the reference lowering has no rule for form {node['form']!r}")
        handler(builder, node, environment)

    def lower_literal(self, builder, node, environment):
        builder.emit("constant", value=node["value"])

    def push_value(self, builder: FunctionBuilder, variable: Var) -> None:
        builder.emit("capture_get" if variable.capture else "local_get", index=variable.slot)
        if variable.reference:
            builder.emit("ref_get")

    def lower_read(self, builder, node, environment):
        variable = environment[node["place"]]
        if node.get("mode") == "borrow-mut":
            if variable.reference:
                builder.emit("capture_get" if variable.capture else "local_get", index=variable.slot)
            elif variable.capture:
                raise LoweringGap("an exclusive borrow of a captured value")
            else:
                builder.emit("local_ref", index=variable.slot)
            return
        self.push_value(builder, variable)
        if node["static_mode"] == "move" and variable.construct is not None:
            builder.disarm(variable.construct)

    def lower_let(self, builder, node, environment):
        self.expression(builder, node["initializer"], environment)
        slot = builder.local()
        builder.emit("local_set", index=slot)
        variable = Var(slot)
        if self.droppable(node["initializer"]) and not builder.dead:
            variable.construct = builder.drop_slot_construct(slot)
            builder.open_construct(variable.construct)
        self.expression(builder, node["body"], {**environment, node["name"]: variable})
        if variable.construct is not None:
            builder.close(variable.construct)

    def lower_assign(self, builder, node, environment):
        variable = environment[node["place"]]
        self.expression(builder, node["value"], environment)
        if variable.reference:
            builder.emit("capture_get" if variable.capture else "local_get", index=variable.slot)
            builder.emit("ref_set")
        else:
            if variable.construct is not None:
                builder.emit("local_get", index=variable.slot)
                builder.emit("drop_value")
            builder.emit("local_set", index=variable.slot)
        builder.emit("constant", value=None)

    def lower_sequence(self, builder, node, environment):
        if not node["items"]:
            builder.emit("constant", value=None)
            return
        for index, item in enumerate(node["items"]):
            self.expression(builder, item, environment)
            if index + 1 != len(node["items"]):
                self.discard(builder, item)

    def lower_if(self, builder, node, environment):
        self.expression(builder, node["condition"], environment)
        base = builder.height - 1
        then_block, else_block = builder.new_block(), builder.new_block()
        builder.emit("branch", delta=-1, **{"then": then_block, "else": else_block})
        builder.branches([
            (then_block, base, lambda: self.expression(builder, node["then"], environment)),
            (else_block, base, lambda: self.expression(builder, node["else"], environment)),
        ], base + 1)

    def lower_while(self, builder, node, environment):
        base = builder.height
        head = builder.new_block()
        builder.emit("jump", target=head)
        builder.switch(head, base)
        body, leave, again = builder.new_block(), builder.new_block(), builder.new_block()
        self.expression(builder, node["condition"], environment)
        builder.emit("branch", delta=-1, **{"then": body, "else": leave})
        builder.loops.append({"open": len(builder.open), "exit": leave, "continue": again, "height": base})
        builder.switch(body, base)
        self.expression(builder, node["body"], environment)
        self.discard(builder, node["body"])
        builder.emit("jump", target=again)
        builder.loops.pop()
        builder.switch(again, base)
        builder.emit("safepoint")
        builder.emit("jump", target=head)
        builder.switch(leave, base)
        builder.emit("constant", value=None)

    def loop_transfer(self, builder: FunctionBuilder, edge: str) -> None:
        loop = builder.loops[-1]
        for _ in range(builder.height - loop["height"]):
            builder.emit("drop")
        saved = builder.exit_to(loop["open"])
        builder.emit("jump", target=loop[edge])
        builder.open = saved

    def lower_break(self, builder, node, environment):
        self.loop_transfer(builder, "exit")

    def lower_continue_loop(self, builder, node, environment):
        self.loop_transfer(builder, "continue")

    def lower_return(self, builder, node, environment):
        value = node["value"]
        if value["form"] == "call" and value.get("tail"):
            self.expression(builder, value, environment)
            return
        self.expression(builder, value, environment)
        saved = builder.exit_to(0)
        builder.emit("return")
        builder.open = saved

    def lower_throw(self, builder, node, environment):
        self.expression(builder, node["value"], environment)
        builder.emit("raise")

    def lower_rethrow(self, builder, node, environment):
        builder.emit("rethrow", trace=True)

    def lower_yield(self, builder, node, environment):
        self.expression(builder, node["value"], environment)
        builder.emit("yield")

    # ------------------------------------------------------------------ calls

    def call_modes(self, node: dict[str, Any]) -> list[str]:
        callee = node["callee"]
        if isinstance(callee, str):
            return [parameter["ownership"] for parameter in self.program["definitions"][callee]["parameters"]]
        type_ = callee.get("static_type")
        type_ = type_[1] if loan_of(type_) else type_
        if isinstance(type_, tuple) and type_[0] == "callable":
            return [mode for mode, _ in type_[1]]
        return ["steal"] * len(node["arguments"])

    def lower_call(self, builder, node, environment):
        callee = node["callee"]
        if isinstance(callee, str) and callee in INTRINSICS and callee not in self.program["definitions"]:
            if callee == "drop":
                self.expression(builder, node["arguments"][0], environment)
                builder.emit("drop_value")
                builder.emit("constant", value=None)
                return
            for argument in node["arguments"]:
                self.expression(builder, argument, environment)
            builder.emit("intrinsic", delta=1 - len(node["arguments"]), name=callee, arity=len(node["arguments"]))
            return
        modes = self.call_modes(node)
        arguments = node["arguments"]
        indirect = not isinstance(callee, str)
        nodes = ([callee] if indirect else []) + arguments
        held = self.operands(builder, nodes, environment, (["value"] if indirect else []) + modes)
        kept: list[tuple[int, Construct, int]] = []
        for position, (slot, construct, mode) in enumerate(held):
            if construct is None:
                continue
            argument = position - (1 if indirect else 0)
            if argument >= 0 and mode in {"borrow", "lend"}:
                kept.append((slot, construct, argument))
            else:
                builder.disarm(construct)
        arity = len(arguments)
        if node.get("tail"):
            for _, construct, _ in reversed(kept):
                builder.disarm(construct)
            saved = builder.exit_to(0)
            adopt = [argument for _, _, argument in kept]
            if indirect and node.get("adopt_callee") and self.droppable(callee):
                adopt.insert(0, -1)
            forward = node.get("forward", [])
            if indirect:
                builder.emit("tail_call_closure", arity=arity, adopt=adopt, forward=forward)
            else:
                builder.emit("tail_call", function=callee, arity=arity, adopt=adopt, forward=forward)
            builder.open = saved
            return
        if indirect:
            builder.emit("call_closure", delta=-arity, arity=arity)
        else:
            builder.emit("call", delta=1 - arity, function=callee, arity=arity)
        for _, construct, _ in reversed(kept):
            builder.close(construct)

    def lower_variant_construct(self, builder, node, environment):
        held = self.operands(builder, node["arguments"], environment)
        self.consume(builder, held)
        builder.emit("make_variant", delta=1 - len(node["arguments"]), case=node["case"], count=len(node["arguments"]))

    def lower_record_construct(self, builder, node, environment):
        held = self.operands(builder, [field["value"] for field in node["fields"]], environment)
        self.consume(builder, held)
        names = [field["name"] for field in node["fields"]]
        builder.emit("make_record", delta=1 - len(names), type=node["type"], fields=names)

    def lower_member(self, builder, node, environment):
        target = node["target"]
        self.expression(builder, target, environment)
        if self.droppable(target):
            slot = builder.local()
            builder.emit("local_set", index=slot)
            construct = builder.drop_slot_construct(slot)
            builder.open_construct(construct)
            builder.emit("local_get", index=slot)
            builder.emit("record_get", field=node["name"])
            builder.close(construct)
        else:
            builder.emit("record_get", field=node["name"])

    # ------------------------------------------------------------------ matching

    def test(self, builder: FunctionBuilder, pattern: Any, slot: int, fail: int, bound: list[tuple[str, int]], base: int,
             path: tuple = (), slots: dict[tuple, int] | None = None) -> None:
        """Test ``pattern`` against the value in ``slot``; fall through on success, jump to ``fail`` otherwise.

        ``slots`` records which local holds each part the pattern projected, keyed by its path.
        """
        slots = slots if slots is not None else {}
        slots[path] = slot
        if pattern == "_":
            return
        if not isinstance(pattern, dict):
            builder.emit("local_get", index=slot)
            builder.emit("constant", value=pattern)
            builder.emit("intrinsic", delta=-1, name="==", arity=2)
            matched = builder.new_block()
            builder.emit("branch", delta=-1, **{"then": matched, "else": fail})
            builder.switch(matched, base)
            return
        if "bind" in pattern:
            bound.append((pattern["bind"], slot))
            return
        if "constructor" in pattern:
            count = len(pattern["arguments"])
            builder.emit("local_get", index=slot)
            matched, missed = builder.new_block(), builder.new_block()
            builder.emit("branch_variant", case=pattern["constructor"], count=count, **{"then": matched, "else": missed})
            builder.switch(missed, base + 1)
            builder.emit("drop")
            builder.emit("jump", target=fail)
            builder.switch(matched, base + count)
            parts = [builder.local() for _ in range(count)]
            for part in reversed(parts):
                builder.emit("local_set", index=part)
            for index, (sub, part) in enumerate(zip(pattern["arguments"], parts)):
                self.test(builder, sub, part, fail, bound, base, path + (("argument", index),), slots)
            return
        if "record" in pattern:
            for field in pattern["fields"]:
                part = builder.local()
                builder.emit("local_get", index=slot)
                builder.emit("record_get", field=field["name"])
                builder.emit("local_set", index=part)
                self.test(builder, field["pattern"], part, fail, bound, base, path + (("field", field["name"]),), slots)
            return
        raise LoweringGap(f"pattern {pattern!r}")

    def discarded(self, pattern: Any, type_: Any, path: tuple, out: list[tuple[tuple, Any]]) -> None:
        """Parts an ownership pattern does not bind, with their types, in declaration order."""
        if pattern == "_":
            out.append((path, type_))
        elif isinstance(pattern, dict) and "constructor" in pattern:
            payload = self.checker.payload(pattern["constructor"], type_)
            for index, sub in enumerate(pattern["arguments"]):
                self.discarded(sub, payload[index] if index < len(payload) else "unknown", path + (("argument", index),), out)
        elif isinstance(pattern, dict) and "record" in pattern:
            named = {field["name"]: field["pattern"] for field in pattern["fields"]}
            for name, field_type in self.checker.records.get(pattern["record"], {}).items():
                self.discarded(named.get(name, "_"), field_type, path + (("field", name),), out)

    def lower_match(self, builder, node, environment):
        scrutinee = node["scrutinee"]
        steal = node["ownership"] == "steal"
        self.expression(builder, scrutinee, environment)
        base = builder.height - 1
        slot = builder.local()
        builder.emit("local_set", index=slot)
        guard = None
        if self.droppable(scrutinee) and not steal and not builder.dead:
            guard = builder.drop_slot_construct(slot)
            builder.open_construct(guard)
        value_type = scrutinee.get("static_type")
        value_type = value_type[1] if loan_of(value_type) else value_type
        start = builder.new_block()
        builder.emit("jump", target=start)
        arms = []
        cursor = start
        for arm in node["arms"]:
            following = builder.new_block()
            arms.append((cursor, base, self.arm(builder, arm, slot, following, steal, value_type, environment, base, guard)))
            cursor = following
        builder.switch(cursor, base)
        builder.emit("trap", code="match-not-exhaustive")
        builder.branches(arms, base + 1)

    def arm(self, builder, arm, slot, fail, steal, value_type, environment, base, guard=None):
        def body() -> None:
            bound: list[tuple[str, int]] = []
            slots: dict[tuple, int] = {}
            self.test(builder, arm["pattern"], slot, fail, bound, base, (), slots)
            inner = dict(environment)
            parts = dict(self.checker.parts(arm["pattern"], value_type, arm))
            opened: list[Construct] = []
            for name, part in bound:
                variable = Var(part)
                if steal and self.droppable_type(parts.get(name, "unknown")):
                    variable.construct = builder.drop_slot_construct(part)
                    builder.open_construct(variable.construct)
                    opened.append(variable.construct)
                inner[name] = variable
            if steal:
                unbound: list[tuple[tuple, Any]] = []
                self.discarded(arm["pattern"], value_type, (), unbound)
                for path, part_type in reversed(unbound):
                    if not self.droppable_type(part_type):
                        continue
                    held = max((known for known in slots if path[: len(known)] == known), key=len)
                    builder.emit("local_get", index=slots[held])
                    for step in path[len(held) :]:
                        if step[0] != "field":
                            raise LoweringGap("dropping an unbound part of a variant payload that was not projected")
                        builder.emit("record_get", field=step[1])
                    builder.emit("drop_value")
            self.expression(builder, arm["body"], inner)
            for construct in reversed(opened):
                builder.close(construct)
            if guard is not None:
                builder.close(guard)

        return body

    # ------------------------------------------------------------------ exceptions and cleanup

    def moved_outer(self, node: Any, environment: dict[str, Var], found: list[str]) -> list[str]:
        """Outer owned bindings that ``node`` moves, in first-move order."""
        if isinstance(node, list):
            for item in node:
                self.moved_outer(item, environment, found)
        elif isinstance(node, dict):
            if node.get("form") == "read" and node.get("static_mode") == "move":
                variable = environment.get(node["place"])
                if variable is not None and variable.construct is not None and node["place"] not in found:
                    found.append(node["place"])
            for capture in node.get("captures", []) if node.get("form") in {"lambda", "fiber"} else []:
                variable = environment.get(capture["name"])
                if capture["ownership"] == "steal" and variable is not None and variable.construct is not None and capture["name"] not in found:
                    found.append(capture["name"])
            for key, value in node.items():
                if key != "static_type":
                    self.moved_outer(value, environment, found)
        return found

    def lower_try(self, builder, node, environment):
        base = builder.height
        # A binding the body moves has its drop obligation moved inside the catch region first, so an
        # exception that arrives before the move drops it and one that arrives after does not.
        shadowed = [name for name in node.get("unwind_drops", []) if environment[name].construct in builder.open]
        for name in shadowed:
            builder.disarm(environment[name].construct)
        snapshot = list(builder.open)
        handler_block = builder.new_block()
        body_block = builder.new_block()
        builder.emit("jump", target=body_block)
        builder.switch(body_block, base)
        catch = Construct("catch", handler_block, base, None)
        builder.open_construct(catch)
        shadows = []
        for name in reversed(shadowed):
            shadow = builder.drop_slot_construct(environment[name].slot)
            builder.open_construct(shadow)
            environment[name].construct = shadow
            shadows.append(shadow)
        self.expression(builder, node["body"], environment)
        for shadow in reversed(shadows):
            builder.close(shadow)
        builder.close(catch)
        for name in shadowed:
            environment[name].construct = None
        ends = [(builder.current, list(builder.open), builder.dead, builder.height)]

        builder.open = list(snapshot)
        builder.switch(handler_block, base + 1)
        slot = builder.local()
        builder.emit("local_set", index=slot)
        for clause in node["catches"]:
            following = builder.new_block()
            bound: list[tuple[str, int]] = []
            self.test(builder, clause["pattern"], slot, following, bound, base)
            inner = {**environment, **{name: Var(part) for name, part in bound}}
            depth = builder.height

            def release() -> None:
                builder.emit("end_catch")

            def unwinding() -> None:
                builder.emit("end_catch")
                builder.emit("resume_unwind")

            active = Construct("cleanup", builder.handler(depth, unwinding), depth, release)
            builder.open_construct(active)
            self.expression(builder, clause["body"], inner)
            builder.close(active)
            ends.append((builder.current, list(builder.open), builder.dead, builder.height))
            builder.open = list(snapshot)
            builder.switch(following, base)
        builder.emit("rethrow", trace=False)
        builder.reconcile(snapshot, ends, base + 1)

    def guard_code(self, builder: FunctionBuilder, guard: dict[str, Any], environment: dict[str, Var]) -> None:
        self.expression(builder, guard["body"], environment)
        self.discard(builder, guard["body"])

    def lower_scope(self, builder, node, environment):
        self.scope(builder, node["guards"], node, environment)

    def scope(self, builder: FunctionBuilder, guards: list[dict[str, Any]], node: dict[str, Any], environment: dict[str, Var]) -> None:
        if not guards:
            self.expression(builder, node["body"], environment)
            return
        guard, rest = guards[0], guards[1:]
        reason = guard["reason"]
        depth = builder.height

        def unwinding() -> None:
            if reason == "success":
                builder.emit("resume_unwind")
                return
            done = builder.new_block()
            if reason == "cancel":
                run = builder.new_block()
                builder.emit("unwind_is_cancel")
                builder.emit("branch", delta=-1, **{"then": run, "else": done})
                builder.switch(run, depth)
            suppress = Construct("suppress", done, depth, None)
            builder.open_construct(suppress)
            self.guard_code(builder, guard, environment)
            builder.open.remove(suppress)
            builder.emit("jump", target=done)
            builder.switch(done, depth)
            builder.emit("resume_unwind")

        def leaving() -> None:
            if reason in {"exit", "success"}:
                self.guard_code(builder, guard, environment)

        construct = Construct("cleanup", builder.handler(depth, unwinding), depth, leaving)
        builder.open_construct(construct)
        self.scope(builder, rest, node, environment)
        if builder.dead or construct not in builder.open:
            if construct in builder.open:
                builder.open.remove(construct)
            return
        if self.droppable(node):
            slot = builder.local()
            builder.emit("local_set", index=slot)
            held = builder.drop_slot_construct(slot)
            builder.open.remove(construct)
            builder.continue_in_new_block()
            builder.open_construct(held)
            leaving()
            builder.emit("local_get", index=slot)
            builder.disarm(held)
        else:
            builder.close(construct)

    # ------------------------------------------------------------------ closures, fibers, tasks

    def captures(self, builder: FunctionBuilder, node: dict[str, Any], environment: dict[str, Var]) -> list[str]:
        modes = []
        for capture in node["captures"]:
            variable = environment[capture["name"]]
            mode = capture["ownership"]
            if mode == "borrow-mut":
                if variable.reference:
                    builder.emit("capture_get" if variable.capture else "local_get", index=variable.slot)
                elif variable.capture:
                    raise LoweringGap("an exclusive borrow of a captured value")
                else:
                    builder.emit("local_ref", index=variable.slot)
            else:
                self.push_value(builder, variable)
                if mode == "steal" and variable.construct is not None:
                    builder.disarm(variable.construct)
            modes.append(mode)
        return modes

    def lower_lambda(self, builder, node, environment):
        if "function" in node:
            builder.emit("make_closure", delta=1, function=node["function"], modes=[])
            return
        self.counter += 1
        name = f"lambda${self.counter}"
        modes = self.captures(builder, node, environment)
        self.function(name, node["parameters"], node["captures"], node["body"], kind="lambda")
        builder.emit("make_closure", delta=1 - len(modes), function=name, modes=modes)

    def lower_generator_create(self, builder, node, environment):
        modes = [parameter["ownership"] for parameter in self.program["definitions"][node["callee"]]["parameters"]]
        held = self.operands(builder, node["arguments"], environment, modes)
        self.consume(builder, held)
        arity = len(node["arguments"])
        builder.emit("make_generator", delta=1 - arity, function=node["callee"], arity=arity)

    def lower_generator_op(self, builder, node, environment):
        operation = node["operation"]
        arguments = node["arguments"]
        modes = ["value" if operation == "start" else "borrow"] + ["value"] * len(arguments)
        held = self.operands(builder, [node["target"], *arguments], environment, modes)
        kept = []
        for position, (_, construct, _) in enumerate(held):
            if construct is None:
                continue
            if position == 0 and operation != "start":
                kept.append(construct)  # a generator made for this one operation is dropped after it
            else:
                builder.disarm(construct)
        builder.emit("generator_op", delta=-len(arguments), operation=operation, arity=len(arguments))
        for construct in reversed(kept):
            builder.close(construct)

    def unary(self, builder, node, environment, op):
        held = self.operands(builder, [node["operand"]], environment)
        self.consume(builder, held)
        builder.emit(op)

    def lower_spawn(self, builder, node, environment):
        self.unary(builder, node, environment, "spawn")

    def lower_join(self, builder, node, environment):
        self.unary(builder, node, environment, "join")

    def lower_cancel(self, builder, node, environment):
        self.unary(builder, node, environment, "cancel")

    # ------------------------------------------------------------------ host boundary

    def host(self, builder, node, environment, check):
        for argument in node["arguments"]:
            self.expression(builder, argument, environment)
        arity = len(node["arguments"])
        # Arguments written as literals are the request's static arguments; the manifest lists them.
        static = check and all(argument["form"] == "literal" for argument in node["arguments"])
        builder.emit("capability_request", delta=1 - arity, operation=node["operation"], arity=arity, check=check, static=static)

    def lower_effect(self, builder, node, environment):
        self.host(builder, node, environment, True)

    def lower_await(self, builder, node, environment):
        self.host(builder, node, environment, False)

    def lower_emit(self, builder, node, environment):
        self.expression(builder, node["event"], environment)
        builder.emit("emit", operation=node["operation"])
        builder.emit("constant", value=None)


def lower(program: dict[str, Any], checker: Checker) -> dict[str, Any]:
    """Lower a statically checked program to an IR version 6 module."""
    return Lowering(program, checker).module()
