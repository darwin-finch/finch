#!/usr/bin/env python3
"""Executor for Finch typed stack IR version 6.

This runs the IR that ``ir_lower`` produces and that ``docs/language/semantics/ir.json`` defines.
It is deliberately shaped like the implemented interpreter (``crates/finch-vm``): an explicit
operand stack, a vector of frames each holding a block and instruction index, and no host-language
recursion per call.  What version 6 adds to version 5 is here too: region tables for handlers and
cleanup, explicit lifecycle drops, slot references, and frame-replacing tail calls.

Ownership is erased.  Nothing below records who owns a value or whether a place was moved from; the
only ownership work is ``drop_value`` at the points the compiler chose.

The executor is checked differentially: for every execution vector it must reproduce the reference
machine's observable sequence, terminal, journal, drops, and host log.
"""

from __future__ import annotations

import hashlib
import json
from typing import Any, Callable

from admission import admit, grant_allows, manifest_of_ir
from reference_machine import ARITHMETIC, COMPARISON, INT_MAX, INT_MIN, MachineError, exported, shown


class Ref:
    """A reference to one local slot of a live frame: the run-time form of an exclusive borrow."""

    __slots__ = ("slots", "index")

    def __init__(self, slots: list[Any], index: int):
        self.slots = slots
        self.index = index


class Signal(Exception):
    pass


class RaiseSignal(Signal):
    def __init__(self, envelope: dict[str, Any]):
        super().__init__("raise")
        self.envelope = envelope

    def note(self, text: str) -> None:
        self.envelope["suppressed"].append(text)

    def describe(self) -> str:
        return f"exception:{shown(self.envelope['value'])}"


class ProtectedSignal(Signal):
    def __init__(self, edge: str, detail: str | None = None):
        super().__init__(edge)
        self.edge = edge
        self.detail = detail
        self.suppressed: list[str] = []

    def note(self, text: str) -> None:
        self.suppressed.append(text)

    def spelling(self) -> str:
        return self.edge if self.detail is None else f"{self.edge}({self.detail})"

    def describe(self) -> str:
        return f"protected:{self.spelling()}"


class Frame:
    __slots__ = ("function", "block", "pc", "locals", "captures", "stack_base", "unwinds", "active", "adopted")

    def __init__(self, function: dict[str, Any], arguments: list[Any], captures: list[Any], stack_base: int):
        self.function = function
        self.block = function["entry"]
        self.pc = 0
        self.locals = arguments + [None] * (function["locals"] - len(arguments))
        self.captures = captures
        self.stack_base = stack_base
        self.unwinds: list[tuple[Signal, int]] = []
        self.active: list[dict[str, Any]] = []
        self.adopted: list[Any] = []


class Context:
    """One resumable execution: the root program, a fiber, or a task."""

    def __init__(self, kind: str, grants: dict[str, bool], owner: Any = None):
        self.kind = kind
        self.frames: list[Frame] = []
        self.stack: list[Any] = []
        self.grants = grants
        self.owner = owner
        self.resume: Any = None


class IrMachine:
    def __init__(self, module: dict[str, Any], program: dict[str, Any], host: dict[str, Any] | None, replay: Callable[[dict[str, Any], dict[str, Any]], dict[str, Any]],
                 binding: str = "mediated", providers: list[dict[str, Any]] | None = None):
        if binding not in {"mediated", "direct"}:
            raise MachineError(f"unknown effect binding {binding!r}")
        self.binding = binding
        self.providers = list(providers or [])
        if module["version"] != 6:
            raise MachineError(f"unsupported IR version {module['version']}")
        self.module = module
        self.functions = module["functions"]
        self.lifecycle = set(program.get("lifecycle", []))
        self.host = host or {}
        self.replay = replay
        self.observable: list[str] = []
        self.journal: list[str] = []
        self.drops: list[str] = []
        self.host_log: list[str] = []
        self.reaper: list[str] = []
        self.transaction = "open"
        self.closures: list[dict[str, Any]] = []
        self.fibers: list[dict[str, Any]] = []
        self.tasks: list[dict[str, Any]] = []
        self.raises = 0
        self.safepoints = 0
        self.max_frames = 0
        self.terminal: dict[str, Any] | None = None
        self.root_grants: dict[str, bool] = dict(self.host.get("grants", {}))
        generation = f"{self.host.get('generation', 0):016x}"
        self.effects = {"generation": generation, "next_sequence": f"{0:016x}", "terminal": False, "requests": {}}
        self.resumes = {"generation": generation, "next_sequence": f"{0:016x}", "terminal": False, "requests": {}, "outstanding": None}
        self.messages = list(self.host.get("resumes", []))
        self.chain: list[Context] = []

    # ------------------------------------------------------------------ values

    def drop_value(self, value: Any) -> None:
        if not isinstance(value, dict):
            return
        if "$record" in value:
            if value["$record"] in self.lifecycle:
                self.observable.append(f"Continue(drop:{shown(value)})")
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
                self.observable.append(f"Continue(drop:{shown(value)})")
                self.drops.append(shown(value))
                self.reaper.append(shown(value))
        elif "$closure" in value:
            record = self.closures[value["$closure"]]
            if record["live"]:
                record["live"] = False
                for item, owned in reversed(list(zip(record["captures"], record["owned"]))):
                    if owned:
                        self.drop_value(item)

    # ------------------------------------------------------------------ host boundary

    def fingerprint(self, payload: Any) -> str:
        text = json.dumps(exported(payload), ensure_ascii=False, separators=(",", ":"), sort_keys=True)
        return hashlib.sha256(text.encode()).hexdigest()

    def accept(self, state: dict[str, Any], event: dict[str, Any]) -> str:
        outcome = self.replay(state, event)
        state.update(outcome["state"])
        return outcome["output"]

    def request(self, context: Context, operation: str, arguments: list[Any]) -> dict[str, Any]:
        sequence = self.effects["next_sequence"]
        identity = f"{operation}#{int(sequence, 16)}"
        event = {
            "kind": "effect", "generation": self.effects["generation"], "sequence": sequence,
            "request_id": identity, "fingerprint": self.fingerprint([operation, arguments]),
        }
        if self.accept(self.effects, event) != "accepted-dispatch-once":
            raise MachineError("a fresh effect request was not accepted by the replay automaton")
        self.journal.append(f"request:{identity}")
        for revocation in self.host.get("revocations", []):
            if revocation["after_sequence"] == int(sequence, 16):
                self.host_log.append(f"revoke:{revocation['operation']}")
                self.root_grants[revocation["operation"]] = False
                for task in self.tasks:
                    task["grants"][revocation["operation"]] = False
        return {"id": identity, "sequence": sequence}

    def deliver(self, request: dict[str, Any]) -> dict[str, Any] | None:
        self.resumes["next_sequence"] = request["sequence"]
        self.resumes["outstanding"] = request["id"]
        while self.messages:
            message = self.messages.pop(0)
            outcome = message["outcome"]
            event = {
                "kind": "resume", "generation": f"{message['generation']:016x}", "sequence": f"{message['sequence']:016x}",
                "request_id": message["request"], "fingerprint": self.fingerprint(outcome),
            }
            verdict = self.accept(self.resumes, event)
            self.host_log.append(f"resume:{message['request']}:{verdict}")
            if verdict == "accepted-dispatch-once":
                self.journal.append(f"resume:{request['id']}")
                self.resumes["outstanding"] = None
                return outcome
        return None

    # ------------------------------------------------------------------ frames

    def push_frame(self, context: Context, name: str, arguments: list[Any], captures: list[Any]) -> Frame:
        function = self.functions[name]
        if len(arguments) != function["parameters"]:
            raise MachineError(f"{name} takes {function['parameters']} arguments, found {len(arguments)}")
        frame = Frame(function, arguments, captures, len(context.stack))
        context.frames.append(frame)
        self.max_frames = max(self.max_frames, len(context.frames))
        return frame

    def leave_frame(self, context: Context) -> Frame:
        frame = context.frames.pop()
        for value, _ in reversed(frame.adopted):
            self.drop_value(value)
        del context.stack[frame.stack_base :]
        return frame

    def pop(self, context: Context, count: int) -> list[Any]:
        if count == 0:
            return []
        frame = context.frames[-1]
        if len(context.stack) - count < frame.stack_base:
            raise MachineError(f"{frame.function['name']} pops below its frame's operand base")
        values = context.stack[-count:]
        del context.stack[-count:]
        return values

    # ------------------------------------------------------------------ unwinding

    def unwind(self, context: Context, signal: Signal, region: int | None, same_frame: bool = True) -> Signal | None:
        """Find the next handler for ``signal``; return it unchanged when this context has none."""
        while True:
            frame = context.frames[-1]
            regions = frame.function["regions"]
            while region is not None:
                entry = regions[region]
                height = frame.stack_base + entry["depth"]
                if entry["kind"] == "suppress":
                    primary = frame.unwinds[-1][0]
                    primary.note(signal.describe())  # type: ignore[attr-defined]
                    del context.stack[height:]
                    frame.block, frame.pc = entry["target"], 0
                    return None
                if entry["kind"] == "catch" and isinstance(signal, RaiseSignal):
                    del context.stack[height:]
                    frame.active.append({"envelope": signal.envelope, "moved": False})
                    context.stack.append(signal.envelope["value"])
                    frame.block, frame.pc = entry["target"], 0
                    return None
                if entry["kind"] == "cleanup":
                    del context.stack[height:]
                    frame.unwinds.append((signal, region))
                    frame.block, frame.pc = entry["target"], 0
                    return None
                region = entry["parent"]
            self.leave_frame(context)
            if not context.frames:
                return signal
            frame = context.frames[-1]
            region = self.block_of(frame)["region"]

    @staticmethod
    def block_of(frame: Frame) -> dict[str, Any]:
        return frame.function["blocks"][frame.block]

    # ------------------------------------------------------------------ execution

    def run(self) -> dict[str, Any]:
        if self.binding == "mediated" and self.host.get("admission", {}).get("mode") == "preflight":
            decision = admit(manifest_of_ir(self.module, {}), self.host)
            self.host_log.extend(decision["log"])
            self.root_grants.clear()
            self.root_grants.update(decision["grants"])
            if decision["refused"]:
                self.transaction = "not-started"
                self.observable.append(f"NotAdmitted({'; '.join(decision['refused'])})")
                self.terminal = {"kind": "not-admitted", "refused": decision["refused"]}
        root = Context("root", self.root_grants)
        if self.terminal is None:
            self.push_frame(root, self.module["entry"], [], [])
        self.chain = [root]
        self.steps = 0
        while self.terminal is None:
            self.step()
        if self.binding == "direct":
            self.transaction = "none"
        elif self.terminal["kind"] not in {"await", "not-admitted"}:
            self.resumes["terminal"] = True
            self.effects["terminal"] = True
            self.deliver({"id": "<none>", "sequence": self.resumes["next_sequence"]})
        return {
            "observable": self.observable,
            "terminal": self.terminal,
            "state": {
                "transaction": self.transaction, "journal": self.journal, "drops": self.drops,
                "host_log": self.host_log, "reaper": self.reaper, "max_frames": self.max_frames,
            },
        }

    def step(self) -> None:
        """Execute one instruction of the innermost active context."""
        self.steps += 1
        if self.steps > 2_000_000:
            raise MachineError("IR execution exceeded its step limit")
        context = self.chain[-1]
        frame = context.frames[-1]
        block = self.block_of(frame)
        try:
            if context.resume is not None:
                outcome, context.resume = context.resume, None
                self.resume(context, outcome)
                return
            if frame.pc >= len(block["instructions"]):
                raise MachineError(f"{frame.function['name']} block {frame.block} has no terminator")
            instruction = block["instructions"][frame.pc]
            frame.pc += 1
            getattr(self, "op_" + instruction["op"])(context, frame, instruction)
        except Signal as signal:
            self.signal(signal, self.block_of(self.chain[-1].frames[-1])["region"])

    def signal(self, signal: Signal, region: int | None) -> None:
        """Unwind ``signal`` through the active context chain until a handler takes it or it is terminal."""
        while True:
            context = self.chain[-1]
            escaped = self.unwind(context, signal, region)
            if escaped is None:
                return
            self.chain.pop()
            if context.kind == "fiber":
                fiber = context.owner
                fiber["state"] = "done"
                fiber.pop("then", None)
                if fiber.pop("cancelling", False):
                    return  # its cleanup has run; the drop that cancelled it continues
                if fiber.pop("starting", False) and isinstance(escaped, RaiseSignal):
                    # `start` keeps a failure for the first read instead of raising it here.
                    fiber["state"], fiber["failure"] = "failed", escaped
                    self.chain[-1].stack.append(fiber["handle"])
                    return
            elif context.kind == "task":
                context.owner["state"] = "done"
            if not self.chain:
                self.finish_abrupt(escaped)
                return
            region = self.block_of(self.chain[-1].frames[-1])["region"]

    def finish_abrupt(self, signal: Signal) -> None:
        self.transaction = "discarded"
        if isinstance(signal, RaiseSignal):
            envelope = signal.envelope
            self.observable.append("Fail(uncaught-exception)")
            self.terminal = {
                "kind": "fail", "diagnostic": "uncaught-exception",
                "exception": {"value": exported(envelope["value"]), "provenance": envelope["provenance"], "suppressed": envelope["suppressed"]},
            }
            return
        assert isinstance(signal, ProtectedSignal)
        self.observable.append(signal.spelling())
        self.terminal = {"kind": "protected", "edge": signal.edge, "detail": signal.detail, "suppressed": signal.suppressed}

    def finish_context(self, context: Context, value: Any) -> None:
        """A context returned from its outermost frame."""
        self.chain.pop()
        if context.kind == "root":
            self.transaction = "committed"
            self.observable.append(f"Complete([{shown(value)}])")
            self.terminal = {"kind": "complete", "values": [exported(value)]}
            return
        below = self.chain[-1]
        if context.kind == "fiber":
            # A returned value of the item type is the last item; a unit return adds none.
            fiber = context.owner
            fiber["state"], fiber["buffer"] = ("done", None) if value is None else ("last", value)
            fiber.pop("then")()
        else:
            context.owner["state"] = "done"
            below.stack.append(value)

    def resume(self, context: Context, outcome: dict[str, Any]) -> None:
        if "value" in outcome:
            context.stack.append(outcome["value"])
        elif "raise" in outcome:
            envelope = {"value": outcome["raise"], "provenance": self.raises, "suppressed": []}
            self.raises += 1
            self.observable.append(f"Raise(value:{shown(outcome['raise'])})")
            raise RaiseSignal(envelope)
        else:
            raise ProtectedSignal(outcome["protected"], outcome.get("detail"))

    # ------------------------------------------------------------------ instructions: values and places

    def op_constant(self, context: Context, frame: Frame, instruction: dict[str, Any]) -> None:
        context.stack.append(instruction["value"])

    def op_drop(self, context: Context, frame: Frame, instruction: dict[str, Any]) -> None:
        self.pop(context, 1)

    def op_drop_value(self, context: Context, frame: Frame, instruction: dict[str, Any]) -> None:
        self.drop_value(self.pop(context, 1)[0])

    def op_dup(self, context: Context, frame: Frame, instruction: dict[str, Any]) -> None:
        context.stack.append(context.stack[-1])

    def op_local_get(self, context: Context, frame: Frame, instruction: dict[str, Any]) -> None:
        context.stack.append(frame.locals[instruction["index"]])

    def op_local_set(self, context: Context, frame: Frame, instruction: dict[str, Any]) -> None:
        frame.locals[instruction["index"]] = self.pop(context, 1)[0]

    def op_local_ref(self, context: Context, frame: Frame, instruction: dict[str, Any]) -> None:
        context.stack.append(Ref(frame.locals, instruction["index"]))

    def op_ref_get(self, context: Context, frame: Frame, instruction: dict[str, Any]) -> None:
        reference = self.pop(context, 1)[0]
        context.stack.append(reference.slots[reference.index])

    def op_ref_set(self, context: Context, frame: Frame, instruction: dict[str, Any]) -> None:
        value, reference = self.pop(context, 2)
        reference.slots[reference.index] = value

    def op_capture_get(self, context: Context, frame: Frame, instruction: dict[str, Any]) -> None:
        context.stack.append(frame.captures[instruction["index"]])

    def op_make_record(self, context: Context, frame: Frame, instruction: dict[str, Any]) -> None:
        values = self.pop(context, len(instruction["fields"]))
        context.stack.append({"$record": instruction["type"], "fields": dict(zip(instruction["fields"], values))})

    def op_record_get(self, context: Context, frame: Frame, instruction: dict[str, Any]) -> None:
        record = self.pop(context, 1)[0]
        context.stack.append(record["fields"][instruction["field"]])

    def op_make_variant(self, context: Context, frame: Frame, instruction: dict[str, Any]) -> None:
        context.stack.append({"$variant": instruction["case"], "arguments": self.pop(context, instruction["count"])})

    def op_make_closure(self, context: Context, frame: Frame, instruction: dict[str, Any]) -> None:
        captures = self.pop(context, len(instruction["modes"]))
        self.closures.append({
            "function": instruction["function"], "captures": captures,
            "owned": [mode == "steal" for mode in instruction["modes"]], "live": True,
        })
        context.stack.append({"$closure": len(self.closures) - 1})

    def op_intrinsic(self, context: Context, frame: Frame, instruction: dict[str, Any]) -> None:
        name = instruction["name"]
        arguments = self.pop(context, instruction["arity"])
        if name == "-" and len(arguments) == 1:
            if arguments[0] == INT_MIN:
                raise ProtectedSignal("Trap", "integer-overflow")
            context.stack.append(-arguments[0])
            return
        left, right = arguments
        if name in COMPARISON:
            context.stack.append(COMPARISON[name](left, right))
            return
        if name == "/":
            if right == 0:
                raise ProtectedSignal("Trap", "divide-by-zero")
            magnitude = abs(left) // abs(right)
            result = magnitude if (left < 0) == (right < 0) else -magnitude
        else:
            result = ARITHMETIC[name](left, right)
        if not INT_MIN <= result <= INT_MAX:
            raise ProtectedSignal("Trap", "integer-overflow")
        context.stack.append(result)

    # ------------------------------------------------------------------ instructions: control

    def op_jump(self, context: Context, frame: Frame, instruction: dict[str, Any]) -> None:
        frame.block, frame.pc = instruction["target"], 0

    def op_branch(self, context: Context, frame: Frame, instruction: dict[str, Any]) -> None:
        condition = self.pop(context, 1)[0]
        frame.block, frame.pc = (instruction["then"] if condition else instruction["else"]), 0

    def op_branch_variant(self, context: Context, frame: Frame, instruction: dict[str, Any]) -> None:
        value = self.pop(context, 1)[0]
        if isinstance(value, dict) and value.get("$variant") == instruction["case"] and len(value["arguments"]) == instruction["count"]:
            context.stack.extend(value["arguments"])
            frame.block, frame.pc = instruction["then"], 0
        else:
            context.stack.append(value)
            frame.block, frame.pc = instruction["else"], 0

    def op_safepoint(self, context: Context, frame: Frame, instruction: dict[str, Any]) -> None:
        index = self.safepoints
        self.safepoints += 1
        if self.host.get("cancel_at_safepoint") == index:
            raise ProtectedSignal("Cancel")

    def op_trap(self, context: Context, frame: Frame, instruction: dict[str, Any]) -> None:
        raise ProtectedSignal("Trap", instruction["code"])

    def op_call(self, context: Context, frame: Frame, instruction: dict[str, Any]) -> None:
        arguments = self.pop(context, instruction["arity"])
        self.push_frame(context, instruction["function"], arguments, [])

    def op_call_closure(self, context: Context, frame: Frame, instruction: dict[str, Any]) -> None:
        arguments = self.pop(context, instruction["arity"])
        closure = self.closures[self.pop(context, 1)[0]["$closure"]]
        self.push_frame(context, closure["function"], arguments, closure["captures"])

    def replace_frame(self, context: Context, name: str, arguments: list[Any], captures: list[Any], instruction: dict[str, Any], closure: Any = None) -> None:
        # An adopted value follows the loans the call passes on; the rest end with this frame.
        old = context.frames[-1]
        carried = []
        for value, parameters in old.adopted:
            positions = {position for source, position in instruction["forward"] if source in parameters}
            if positions:
                carried.append((value, positions))
        old.adopted = [entry for entry in old.adopted if not any(entry[0] is value for value, _ in carried)]
        self.leave_frame(context)
        frame = self.push_frame(context, name, arguments, captures)
        frame.adopted = carried + [(closure if index == -1 else arguments[index], {index}) for index in instruction["adopt"]]

    def op_tail_call(self, context: Context, frame: Frame, instruction: dict[str, Any]) -> None:
        arguments = self.pop(context, instruction["arity"])
        self.replace_frame(context, instruction["function"], arguments, [], instruction)

    def op_tail_call_closure(self, context: Context, frame: Frame, instruction: dict[str, Any]) -> None:
        arguments = self.pop(context, instruction["arity"])
        handle = self.pop(context, 1)[0]
        closure = self.closures[handle["$closure"]]
        self.replace_frame(context, closure["function"], arguments, closure["captures"], instruction, handle)

    def op_return(self, context: Context, frame: Frame, instruction: dict[str, Any]) -> None:
        value = self.pop(context, 1)[0]
        if len(context.stack) != frame.stack_base:
            raise MachineError(f"{frame.function['name']} returns with {len(context.stack) - frame.stack_base} extra operands")
        self.leave_frame(context)
        if context.frames:
            context.stack.append(value)
        else:
            self.finish_context(context, value)

    # ------------------------------------------------------------------ instructions: exceptions and cleanup

    def op_raise(self, context: Context, frame: Frame, instruction: dict[str, Any]) -> None:
        value = self.pop(context, 1)[0]
        envelope = {"value": value, "provenance": self.raises, "suppressed": []}
        self.raises += 1
        self.observable.append(f"Raise(value:{shown(value)})")
        raise RaiseSignal(envelope)

    def op_rethrow(self, context: Context, frame: Frame, instruction: dict[str, Any]) -> None:
        if instruction["trace"]:
            entry = frame.active[-1]
            entry["moved"] = True
            self.observable.append(f"Raise(rethrow:{shown(entry['envelope']['value'])})")
        else:
            entry = frame.active.pop()
        raise RaiseSignal(entry["envelope"])

    def op_end_catch(self, context: Context, frame: Frame, instruction: dict[str, Any]) -> None:
        entry = frame.active.pop()
        if not entry["moved"]:
            self.drop_value(entry["envelope"]["value"])

    def op_resume_unwind(self, context: Context, frame: Frame, instruction: dict[str, Any]) -> None:
        signal, region = frame.unwinds.pop()
        self.signal(signal, frame.function["regions"][region]["parent"])

    def op_unwind_is_cancel(self, context: Context, frame: Frame, instruction: dict[str, Any]) -> None:
        signal = frame.unwinds[-1][0]
        context.stack.append(isinstance(signal, ProtectedSignal) and signal.edge == "Cancel")

    # ------------------------------------------------------------------ instructions: host boundary

    def op_capability_request(self, context: Context, frame: Frame, instruction: dict[str, Any]) -> None:
        arguments = self.pop(context, instruction["arity"])
        operation = instruction["operation"]
        if self.binding == "direct":
            # The operation is linked to its provider and called like any function: no grant test,
            # no journal, no suspension.  Grants were checked against the manifest when it was built.
            self.observable.append(f"Call({operation})")
            if not self.providers:
                raise MachineError(f"direct provider for {operation} has no result")
            context.resume = self.providers.pop(0)
            return
        if instruction["check"] and not grant_allows(context.grants, operation, arguments):
            raise ProtectedSignal("Denied", operation)
        request = self.request(context, operation, arguments)
        self.observable.append(f"Await({request['id']})")
        outcome = self.deliver(request)
        if outcome is None:
            self.terminal = {"kind": "await", "request": request["id"]}
            return
        context.resume = outcome

    def op_emit(self, context: Context, frame: Frame, instruction: dict[str, Any]) -> None:
        event = f"{instruction['operation']}:{shown(self.pop(context, 1)[0])}"
        if self.binding == "mediated":
            self.journal.append(f"event:{event}")
        self.observable.append(f"Emit({event})")

    # ------------------------------------------------------------------ instructions: fibers and tasks

    def op_make_generator(self, context: Context, frame: Frame, instruction: dict[str, Any]) -> None:
        arguments = self.pop(context, instruction["arity"])
        self.fibers.append({"function": instruction["function"], "arguments": arguments, "state": "dormant", "context": None, "buffer": None, "failure": None})
        handle = {"$fiber": len(self.fibers) - 1}
        self.fibers[-1]["handle"] = handle
        context.stack.append(handle)

    def advance(self, context: Context, handle: dict[str, Any], reply: Any, then: Any) -> None:
        """Switch to the fiber until its next yield or its end; ``then`` finishes the operation."""
        fiber = self.fibers[handle["$fiber"]]
        fiber["then"] = then
        if fiber["state"] == "dormant":
            fiber["context"] = Context("fiber", context.grants, fiber)
            self.push_frame(fiber["context"], fiber["function"], fiber["arguments"], [])
        else:
            fiber["context"].stack.append(reply)
        fiber["state"] = "running"
        self.chain.append(fiber["context"])

    def cancel_fiber(self, handle: dict[str, Any]) -> None:
        """End a fiber nobody can advance again: its cleanup runs here, in the dropping execution."""
        fiber = self.fibers[handle["$fiber"]]
        state = fiber["state"]
        if state == "done":
            return
        self.observable.append(f"Continue(drop:{shown(handle)})")
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
        inner = fiber["context"]
        fiber["cancelling"] = True
        self.chain.append(inner)
        self.signal(ProtectedSignal("Cancel", "handle-dropped"), self.block_of(inner.frames[-1])["region"])
        while inner in self.chain:
            self.step()

    def op_generator_op(self, context: Context, frame: Frame, instruction: dict[str, Any]) -> None:
        values = self.pop(context, instruction["arity"] + 1)
        handle = values[0]
        while isinstance(handle, Ref):  # the generator is borrowed exclusively from its slot
            handle = handle.slots[handle.index]
        self.generator_operation(context, handle, instruction["operation"], values[1:])

    def generator_operation(self, context: Context, handle: dict[str, Any], operation: str, arguments: list[Any]) -> None:
        fiber = self.fibers[handle["$fiber"]]
        def finished() -> RaiseSignal:
            # Reading past the end is an ordinary exception: the value RangeEmpty, catchable by the reader.
            envelope = {"value": {"$record": "RangeEmpty", "fields": {}}, "provenance": self.raises, "suppressed": []}
            self.raises += 1
            self.observable.append(f"Raise(value:{shown(envelope['value'])})")
            return RaiseSignal(envelope)

        if operation == "start":
            if fiber["state"] == "dormant":
                fiber["starting"] = True
                self.advance(context, handle, None, lambda: (fiber.pop("starting", None), context.stack.append(handle)))
            else:
                context.stack.append(handle)
            return
        if fiber["state"] == "failed":
            failure, fiber["failure"], fiber["state"] = fiber["failure"], None, "done"
            raise failure
        if fiber["state"] == "dormant":
            self.advance(context, handle, None, lambda: self.generator_operation(context, handle, operation, arguments))
            return
        if operation == "empty?":
            context.stack.append(fiber["state"] == "done")
            return
        if operation == "front":
            if fiber["state"] == "done":
                raise finished()
            context.stack.append(fiber["buffer"])
            return
        if fiber["state"] == "done" or (operation == "reply" and fiber["state"] == "last"):
            for argument in arguments:
                self.drop_value(argument)
            raise finished()
        self.drop_value(fiber["buffer"])

        def after() -> None:
            if operation == "pop-front":
                context.stack.append(None)
            elif fiber["state"] == "done":
                raise finished()
            else:
                context.stack.append(fiber["buffer"])

        if fiber["state"] == "last":
            fiber["state"], fiber["buffer"] = "done", None
            after()
        elif operation == "pop-front":
            self.advance(context, handle, {"$variant": "none", "arguments": []}, after)
        else:
            payload = arguments[0] if len(arguments) == 1 else {"$tuple": arguments}
            self.advance(context, handle, {"$variant": "some", "arguments": [payload]}, after)

    def op_yield(self, context: Context, frame: Frame, instruction: dict[str, Any]) -> None:
        value = self.pop(context, 1)[0]
        if context.kind != "fiber":
            raise MachineError("yield outside a generator body")
        fiber = context.owner
        if fiber.get("cancelling"):
            raise MachineError("cleanup that suspends while its generator is being dropped is outside the reference machine")
        self.chain.pop()
        fiber["state"], fiber["buffer"] = "suspended", value
        fiber.pop("then")()

    def op_spawn(self, context: Context, frame: Frame, instruction: dict[str, Any]) -> None:
        closure = self.pop(context, 1)[0]
        capacity = self.host.get("task_capacity")
        live = sum(1 for task in self.tasks if task["state"] in {"runnable", "running"})
        if capacity is not None and live >= capacity:
            self.drop_value(closure)
            raise ProtectedSignal("ResourceExhausted", "scheduler-capacity")
        allowed = self.host.get("child_grants")
        grants = dict(context.grants) if allowed is None else {name: (live_grant if allowed.get(name, False) else False) for name, live_grant in context.grants.items()}
        self.tasks.append({"closure": closure, "state": "runnable", "grants": grants})
        context.stack.append({"$task": len(self.tasks) - 1})

    def op_join(self, context: Context, frame: Frame, instruction: dict[str, Any]) -> None:
        task = self.tasks[self.pop(context, 1)[0]["$task"]]
        if task["state"] != "runnable":
            raise MachineError(f"join of a {task['state']} task handle")
        task["state"] = "running"
        child = Context("task", task["grants"], task)
        closure = self.closures[task["closure"]["$closure"]]
        self.push_frame(child, closure["function"], [], closure["captures"])
        self.chain.append(child)

    def op_cancel(self, context: Context, frame: Frame, instruction: dict[str, Any]) -> None:
        task = self.tasks[self.pop(context, 1)[0]["$task"]]
        if task["state"] != "runnable":
            raise MachineError(f"cancel of a {task['state']} task handle")
        task["state"] = "cancelled"
        self.drop_value(task["closure"])
        context.stack.append(None)


def execute_ir(module: dict[str, Any], program: dict[str, Any], context: dict[str, Any] | None, replay: Any,
               binding: str = "mediated", providers: list[dict[str, Any]] | None = None) -> dict[str, Any]:
    return IrMachine(module, program, (context or {}).get("host"), replay, binding, providers).run()


def direct_comparable(context: dict[str, Any] | None, mediated: dict[str, Any]) -> list[dict[str, Any]] | None:
    """Provider results for re-running a mediated vector under the direct binding, or None.

    A vector is comparable when nothing in it depends on mediation: it was admitted lazily, no
    request was denied, no grant was revoked, no host message was rejected or replayed, and it did
    not end parked.  The providers then return exactly what the host resumed with, in order.
    """
    host = (context or {}).get("host", {})
    terminal = mediated["terminal"]
    if terminal["kind"] in {"await", "not-admitted"} or terminal.get("edge") == "Denied":
        return None
    if host.get("admission") or host.get("revocations"):
        return None
    log = [entry for entry in mediated["state"]["host_log"] if entry.startswith("resume:")]
    if any(not entry.endswith(":accepted-dispatch-once") for entry in log):
        return None
    return [message["outcome"] for message in host.get("resumes", [])][: len(log)]


def direct_projection(observable: list[str]) -> list[str]:
    """What the direct binding must reproduce from a mediated run: the same effects in the same order."""
    projected = []
    for entry in observable:
        if entry.startswith("Await("):
            projected.append(f"Call({entry[len('Await('):-1].rsplit('#', 1)[0]})")
        else:
            projected.append(entry)
    return projected
