#!/usr/bin/env python3
"""Static ownership pass for the executable core.

Ownership in Finch is proven at compile time and erased: nothing at run time tracks who owns a
value.  This pass is the reference for that proof on the resolved program.  For every place read it
decides, from types and binding kinds alone, whether the read is a copy, a move, or a borrow; it
tracks which bindings a path has moved from; and it rejects the programs a compiler must reject,
with the same diagnostic codes the static-rejection vectors name.

The reference machine makes the same three-way decision dynamically, from the value it finds.  The
checker requires the two to agree on every read every vector executes, which is the evidence that
the decision really is static.

Types here are only as precise as ownership needs: enough to know whether a value is ``Copy`` and
what a pattern or member projects.  This is not the language's type checker.
"""

from __future__ import annotations

from typing import Any

from reference_machine import INTRINSICS, MachineError


class StaticError(MachineError):
    """A program rejected before execution; ``code`` is its stable diagnostic."""


SCALARS = {"int", "bool", "unit", "string", "never", "unknown"}
COMPARISONS = {"==", "!=", "<", "<=", ">", ">="}


class Binding:
    _next = 0

    def __init__(self, name: str, type_: Any, loan: bool, mutable: bool, local: bool):
        self.name = name
        self.type = type_
        self.loan = loan  # the place holds a loan rather than owning its value
        self.mutable = mutable
        self.local = local  # a loan taken from it names a place owned by the current frame
        self.parameter: int | None = None  # parameter index, or -1 for a capture of the closure being run
        Binding._next += 1
        self.id = Binding._next


class Context:
    def __init__(self, function: str, resume: Any = None):
        self.function = function  # entry | function | lambda
        self.reply: Any = "unit"  # what a `yield` here may be sent: the callable's parameter list
        self.loops = 0
        self.guards = 0
        self.catches = 0
        self.resume = resume
        self.resume_binding: Binding | None = None
        self.yields: list[Any] = []

    def nested(self, **changes: int) -> "Context":
        other = Context(self.function, self.resume)
        other.loops, other.guards, other.catches, other.yields = self.loops, self.guards, self.catches, self.yields
        other.resume_binding = self.resume_binding
        other.reply = self.reply
        for key, value in changes.items():
            setattr(other, key, value)
        return other


def loan_of(type_: Any) -> bool:
    return isinstance(type_, tuple) and type_[0] == "loan"


def underlying(type_: Any) -> Any:
    return type_[1] if loan_of(type_) else type_


def join(left: Any, right: Any) -> Any:
    if left == "never" or left == "unknown":
        return right
    return left


class Checker:
    def __init__(self, program: dict[str, Any]):
        self.program = program
        self.definitions = program["definitions"]
        self.operations = program.get("operations", {})
        self.copy_types = set(program.get("copy_types", []))
        self.records: dict[str, dict[str, Any]] = {}
        self.variants: dict[str, dict[str, list[Any]]] = {}
        self.case_owner: dict[str, str] = {}
        self.reads: list[dict[str, Any]] = []

    # ------------------------------------------------------------------ types

    def is_copy(self, type_: Any) -> bool:
        if loan_of(type_):
            return True
        if isinstance(type_, str):
            return type_ in SCALARS
        return type_[0] == "record" and type_[1] in self.copy_types

    def parse_type(self, text: str | None) -> Any:
        if text is None:
            return "unknown"
        if text in {"int", "bool", "unit", "string", "unknown"}:
            return text
        if text.startswith("callable<("):
            depth = 0
            close = 0
            for index, character in enumerate(text[len("callable<") :]):
                depth += character == "("
                depth -= character == ")"
                if depth == 0:
                    close = index + len("callable<")
                    break
            inner = text[len("callable<(") : close]
            parameters = []
            for part in self.split(inner):
                mode = "borrow"
                for prefix, name in (("steal ", "steal"), ("borrow-mut ", "borrow-mut")):
                    if part.startswith(prefix):
                        mode, part = name, part[len(prefix) :]
                parameters.append((mode, self.parse_type(part)))
            result = text[close + 3 :].split("!")[0]
            return ("callable", parameters, self.parse_type(result), True)
        if text in self.variants:
            return ("variant", text)
        return ("record", text)

    @staticmethod
    def split(text: str) -> list[str]:
        parts: list[str] = []
        depth = 0
        current = ""
        for character in text:
            if character == "," and depth == 0:
                parts.append(current)
                current = ""
                continue
            depth += character in "<("
            depth -= character in ">)"
            current += character
        return [part for part in parts + [current] if part]

    # ------------------------------------------------------------------ entry

    def run(self) -> None:
        declared = self.program.get("variants", {})
        for name, cases in declared.items():
            self.variants[name] = {}
            for case in cases:
                self.case_owner[case] = name
        for name, cases in declared.items():
            self.variants[name] = {case: [self.parse_type(text) for text in payload] for case, payload in cases.items()}
        # Record field types are learned from construction sites, which may follow their uses: note
        # every constructed field first, learn types with errors deferred, then check for real.
        self.generator_items: dict[str, Any] = {}
        self.scan([definition["body"] for definition in self.definitions.values()] + [self.program["body"]])
        self.analyse(tolerant=True)
        self.reads = []
        self.analyse(tolerant=False)

    def scan(self, value: Any) -> None:
        if isinstance(value, list):
            for item in value:
                self.scan(item)
        elif isinstance(value, dict):
            if value.get("form") == "record-construct":
                fields = self.records.setdefault(value["type"], {})
                for field in value["fields"]:
                    fields.setdefault(field["name"], "unknown")
            for item in value.values():
                self.scan(item)

    def analyse(self, tolerant: bool) -> None:
        units = [
            lambda name=name, definition=definition: self.definition_body(name, definition)
            for name, definition in self.definitions.items()
        ]
        units.append(lambda: self.walk(self.program["body"], {}, set(), Context("entry")))
        for unit in units:
            try:
                unit()
            except StaticError:
                if not tolerant:
                    raise

    def parameter_binding(self, parameter: dict[str, Any]) -> Binding:
        type_ = self.parse_type(parameter["type"])
        mode = parameter["ownership"]
        loan = mode == "borrow-mut" or (mode == "borrow" and not self.is_copy(type_))
        return Binding(parameter["name"], type_, loan, mode == "borrow-mut", local=False)

    def definition_body(self, name: str, definition: dict[str, Any]) -> None:
        context = Context("function")
        result = self.callable_body(definition["parameters"], definition["body"], {}, context)
        if not definition.get("generator"):
            return
        # A generator's items are what it yields.  A returned value is its last item, so the
        # declared result is either that same type or unit.
        declared = self.parse_type(definition["result"])
        yielded = next((type_ for type_ in context.yields if type_ not in {"unknown", "never"}), "unknown")
        if declared != "unit" and "unknown" not in {declared, yielded} and declared != yielded:
            raise StaticError(f"generator {name} yields {yielded!r} but returns {declared!r}; they must be the same type, or the result unit", "F-DIAG-YIELD-TYPE")
        self.generator_items[name] = declared if declared != "unit" else yielded
        del result

    def callable_body(self, parameters: list[dict[str, Any]], body: dict[str, Any], captured: dict[str, Binding], context: Context) -> Any:
        environment = dict(captured)
        for index, parameter in enumerate(parameters):
            binding = self.parameter_binding(parameter)
            binding.parameter = index
            environment[parameter["name"]] = binding
        types = [self.parse_type(parameter["type"]) for parameter in parameters]
        context.reply = "unit" if not types else types[0] if len(types) == 1 else ("tuple", tuple(types))
        type_, _ = self.walk(body, environment, set(), context)
        return type_

    # ------------------------------------------------------------------ patterns

    def parts(self, pattern: Any, type_: Any, node: dict[str, Any]) -> list[tuple[str, Any]]:
        """Names a pattern binds, each with the type of the part it names."""
        if not isinstance(pattern, dict):
            return []
        if "bind" in pattern:
            return [(pattern["bind"], type_)]
        if "constructor" in pattern:
            payload = self.payload(pattern["constructor"], type_)
            found: list[tuple[str, Any]] = []
            for sub, part in zip(pattern["arguments"], payload + ["unknown"] * len(pattern["arguments"])):
                found.extend(self.parts(sub, part, node))
            return found
        if "record" in pattern:
            fields = self.records.get(pattern["record"], {})
            found = []
            for field in pattern["fields"]:
                found.extend(self.parts(field["pattern"], fields.get(field["name"], "unknown"), node))
            return found
        raise StaticError(f"pattern {pattern!r} is outside the executable core")

    def payload(self, case: str, type_: Any) -> list[Any]:
        if isinstance(type_, tuple) and type_[0] == "option":
            return {"some": [type_[1]], "none": []}.get(case, [])
        owner = self.case_owner.get(case)
        return list(self.variants[owner][case]) if owner else []

    def irrefutable(self, pattern: Any) -> bool:
        if pattern == "_" or (isinstance(pattern, dict) and "bind" in pattern):
            return True
        return isinstance(pattern, dict) and "record" in pattern and all(self.irrefutable(field["pattern"]) for field in pattern["fields"])

    def exhaustive(self, patterns: list[Any], type_: Any) -> bool:
        if any(self.irrefutable(pattern) for pattern in patterns):
            return True
        covered = {
            pattern["constructor"] for pattern in patterns
            if isinstance(pattern, dict) and "constructor" in pattern and all(self.irrefutable(argument) for argument in pattern["arguments"])
        }
        if isinstance(type_, tuple) and type_[0] == "option":
            return {"some", "none"} <= covered
        if isinstance(type_, tuple) and type_[0] == "variant":
            return set(self.variants[type_[1]]) <= covered
        return False

    # ------------------------------------------------------------------ expressions

    def walk(self, node: dict[str, Any], environment: dict[str, Binding], moved: set[int], context: Context) -> tuple[Any, set[int]]:
        handler = getattr(self, "form_" + node["form"].replace("-", "_"), None)
        if handler is None:
            raise StaticError(f"the static pass has no rule for form {node['form']!r}")
        type_, moved = handler(node, environment, moved, context)
        node["static_type"] = type_
        return type_, moved

    def form_literal(self, node, environment, moved, context):
        value = node["value"]
        type_ = "unit" if value is None else "bool" if isinstance(value, bool) else "int" if isinstance(value, int) else "string"
        return type_, moved

    def form_read(self, node, environment, moved, context):
        binding = environment.get(node["place"])
        if binding is None:
            raise StaticError(f"unbound local {node['place']!r}", "F-DIAG-UNBOUND-NAME")
        if binding.id in moved:
            raise StaticError(f"{node['place']!r} is used after it was moved", "F-DIAG-USE-AFTER-MOVE")
        mode = node.get("mode")
        copy = self.is_copy(binding.type)
        self.reads.append(node)
        if mode == "borrow-mut" or (not copy and (mode == "borrow" or binding.loan)):
            node["static_mode"] = "borrow"
            return ("loan", binding.type, binding.local and not binding.loan), moved
        if copy:
            node["static_mode"] = "copy"
            return binding.type, moved
        node["static_mode"] = "move"
        return binding.type, moved | {binding.id}

    def form_let(self, node, environment, moved, context):
        type_, moved = self.walk(node["initializer"], environment, moved, context)
        binding = Binding(node["name"], underlying(type_), loan_of(type_), bool(node.get("mutable")), local=True)
        return self.walk(node["body"], {**environment, node["name"]: binding}, moved, context)

    def form_assign(self, node, environment, moved, context):
        _, moved = self.walk(node["value"], environment, moved, context)
        binding = environment.get(node["place"])
        if binding is None:
            raise StaticError(f"unbound local {node['place']!r}", "F-DIAG-UNBOUND-NAME")
        if not binding.mutable:
            raise StaticError(f"assignment to immutable local {node['place']!r}", "F-DIAG-ASSIGN-IMMUTABLE")
        if binding.id in moved:
            raise StaticError(f"{node['place']!r} is assigned after it was moved", "F-DIAG-USE-AFTER-MOVE")
        return "unit", moved

    def form_sequence(self, node, environment, moved, context):
        type_: Any = "unit"
        for item in node["items"]:
            type_, moved = self.walk(item, environment, moved, context)
        return type_, moved

    @staticmethod
    def join_drops(environment: dict[str, Binding], arm_moved: set[int], total: set[int]) -> list[str]:
        """Bindings another path moved and this one did not: this path drops them before the join.

        Most recent binding first, which is the order their cleanup would otherwise have run in.
        """
        owed = total - arm_moved
        bindings = sorted((binding for binding in environment.values() if binding.id in owed), key=lambda binding: -binding.id)
        return [binding.name for binding in bindings]

    def form_if(self, node, environment, moved, context):
        _, moved = self.walk(node["condition"], environment, moved, context)
        then_type, then_moved = self.walk(node["then"], environment, moved, context)
        else_type, else_moved = self.walk(node["else"], environment, moved, context)
        total = then_moved | else_moved
        node["join_drops"] = {
            "then": self.join_drops(environment, then_moved, total),
            "else": self.join_drops(environment, else_moved, total),
        }
        return join(then_type, else_type), total

    def form_while(self, node, environment, moved, context):
        inner = context.nested(loops=context.loops + 1)
        _, after_condition = self.walk(node["condition"], environment, moved, inner)
        _, after_body = self.walk(node["body"], environment, after_condition, inner)
        outer = {binding.id: name for name, binding in environment.items()}
        for identity in after_body - moved:
            if identity in outer:
                raise StaticError(f"{outer[identity]!r} is moved inside a loop and would be used again", "F-DIAG-USE-AFTER-MOVE")
        return "unit", after_body

    def form_break(self, node, environment, moved, context):
        if context.loops == 0:
            raise StaticError("break has no lexically enclosing loop", "F-DIAG-LOOP-TARGET")
        return "never", moved

    form_continue_loop = form_break

    def form_return(self, node, environment, moved, context):
        if context.guards:
            raise StaticError("a guard body cannot return from its enclosing callable", "F-DIAG-GUARD-ESCAPE")
        if context.function == "entry":
            raise StaticError("return has no enclosing function or lambda body", "F-DIAG-RETURN-TARGET")
        _, moved = self.walk(node["value"], environment, moved, context)
        return "never", moved

    def form_throw(self, node, environment, moved, context):
        _, moved = self.walk(node["value"], environment, moved, context)
        return "never", moved

    def form_rethrow(self, node, environment, moved, context):
        if context.catches == 0:
            raise StaticError("rethrow outside a catch arm", "F-DIAG-RETHROW-TARGET")
        return "never", moved

    def form_yield(self, node, environment, moved, context):
        if context.function == "entry" or context.guards:
            raise StaticError("yield outside a generator body", "F-DIAG-YIELD-TARGET")
        type_, moved = self.walk(node["value"], environment, moved, context)
        if loan_of(type_) and not self.is_copy(underlying(type_)):
            raise StaticError("a yielded value is handed to the consumer, so it cannot be a loan", "F-DIAG-TRANSFER-REQUIRES-OWNED")
        known = [earlier for earlier in context.yields if earlier not in {"unknown", "never"}]
        item = underlying(type_)
        if known and item not in {"unknown", "never"} and item != known[0]:
            raise StaticError(f"every yield in one body yields the same type; found {known[0]!r} and {item!r}", "F-DIAG-YIELD-TYPE")
        context.yields.append(item)
        # The value of `yield` is the reply, if the consumer sent one: fresh arguments for this callable.
        return ("option", context.reply), moved

    def arguments(self, node, parameters, environment, moved, context):
        types = []
        for argument in node["arguments"]:
            type_, moved = self.walk(argument, environment, moved, context)
            types.append(type_)
        if parameters is not None:
            if len(parameters) != len(types):
                raise StaticError(f"call passes {len(types)} arguments for {len(parameters)} parameters")
            for (mode, _), type_ in zip(parameters, types):
                if mode == "steal" and loan_of(type_):
                    raise StaticError("a stealing parameter cannot take a borrowed argument", "F-DIAG-MOVE-FROM-BORROW")
        def escapes(type_: Any) -> bool:
            inner = underlying(type_)
            holds = isinstance(inner, tuple) and inner[0] == "callable" and len(inner) > 4 and inner[4]
            return bool((loan_of(type_) and type_[2]) or holds)

        if node.get("tail") and any(escapes(type_) for type_ in types):
            raise StaticError("a tail call cannot pass a loan of a place owned by the frame it discards", "F-DIAG-LOAN-ESCAPES-FRAME")
        if node.get("tail"):
            # A loan this frame received and passes on: if the frame adopted the value behind it, the
            # adoption follows the loan into the callee's frame rather than ending here.
            forward = []
            callee = node["callee"]
            if isinstance(callee, dict) and callee.get("form") == "read":
                binding = environment.get(callee["place"])
                if binding is not None and binding.parameter is not None:
                    # The closure being called was lent to this frame; position -1 names the callee.
                    forward.append([binding.parameter, -1])
            for position, argument in enumerate(node["arguments"]):
                root = argument
                while root.get("form") == "member":
                    root = root["target"]
                binding = environment.get(root["place"]) if root.get("form") == "read" else None
                if binding is not None and binding.parameter is not None:
                    forward.append([binding.parameter, position])
                closure = underlying(types[position])
                if isinstance(closure, tuple) and closure[0] == "callable" and len(closure) > 5:
                    # A closure that captured a loan this frame received carries it onward.
                    forward.extend([source, position] for source in sorted(closure[5]) if [source, position] not in forward)
            node["forward"] = forward
        return types, moved

    def form_call(self, node, environment, moved, context):
        callee = node["callee"]
        if isinstance(callee, dict):
            callee_type, moved = self.walk(callee, environment, moved, context)
            callable_type = underlying(callee_type)
            parameters = callable_type[1] if isinstance(callable_type, tuple) and callable_type[0] == "callable" else None
            _, moved = self.arguments(node, parameters, environment, moved, context)
            result = callable_type[2] if parameters is not None else "unknown"
            return result, moved
        if callee in self.definitions:
            definition = self.definitions[callee]
            parameters = [(p["ownership"], self.parse_type(p["type"])) for p in definition["parameters"]]
            _, moved = self.arguments(node, parameters, environment, moved, context)
            return self.parse_type(definition["result"]), moved
        if callee in INTRINSICS:
            _, moved = self.arguments(node, None, environment, moved, context)
            return ("bool" if callee in COMPARISONS else "unit" if callee == "drop" else "int"), moved
        raise StaticError(f"unresolved callee {callee!r}", "F-DIAG-UNBOUND-NAME")

    def form_variant_construct(self, node, environment, moved, context):
        types, moved = self.arguments(node, None, environment, moved, context)
        if node["type"] == "Option":
            return ("option", underlying(types[0]) if types else "unknown"), moved
        return ("variant", node["type"]), moved

    def form_record_construct(self, node, environment, moved, context):
        fields = self.records.setdefault(node["type"], {})
        for field in node["fields"]:
            type_, moved = self.walk(field["value"], environment, moved, context)
            fields[field["name"]] = join(fields.get(field["name"], "unknown"), underlying(type_))
        return ("record", node["type"]), moved

    def form_member(self, node, environment, moved, context):
        target, moved = self.walk(node["target"], environment, moved, context)
        aggregate = underlying(target)
        fields = self.records.get(aggregate[1], {}) if isinstance(aggregate, tuple) and aggregate[0] == "record" else {}
        if node["name"] not in fields:
            raise StaticError(f"value has no field {node['name']!r}", "F-DIAG-UNKNOWN-MEMBER")
        field = fields[node["name"]]
        if self.is_copy(field):
            return field, moved
        if not loan_of(target):
            raise StaticError(f"field {node['name']!r} cannot be moved out of its aggregate", "F-DIAG-PARTIAL-MOVE")
        return ("loan", field, target[2]), moved

    def form_match(self, node, environment, moved, context):
        scrutinee, moved = self.walk(node["scrutinee"], environment, moved, context)
        value_type = underlying(scrutinee)
        if not self.exhaustive([arm["pattern"] for arm in node["arms"]], value_type):
            raise StaticError("match is not exhaustive", "F-DIAG-MATCH-NOT-EXHAUSTIVE")
        steal = node["ownership"] == "steal"
        if steal and loan_of(scrutinee):
            raise StaticError("an ownership match cannot consume a borrowed scrutinee", "F-DIAG-MOVE-FROM-BORROW")
        result: Any = "never"
        after = set(moved)
        ends = []
        for arm in node["arms"]:
            inner = dict(environment)
            for name, part in self.parts(arm["pattern"], value_type, node):
                loan = not steal and not self.is_copy(part)
                inner[name] = Binding(name, part, loan, False, local=True)
            type_, arm_moved = self.walk(arm["body"], inner, moved, context)
            result = join(result, type_)
            after |= arm_moved
            ends.append((arm, arm_moved))
        for arm, arm_moved in ends:
            arm["join_drops"] = self.join_drops(environment, arm_moved, after)
        return result, after

    def form_try(self, node, environment, moved, context):
        result, after = self.walk(node["body"], environment, moved, context)
        handler_context = context.nested(catches=context.catches + 1)
        total = set(after)
        ends = []
        for clause in node["catches"]:
            inner = dict(environment)
            for name, part in self.parts(clause["pattern"], "unknown", node):
                inner[name] = Binding(name, part, False, False, local=True)
            type_, clause_moved = self.walk(clause["body"], inner, after, handler_context)
            result = join(result, type_)
            total |= clause_moved
            ends.append((clause, clause_moved))
        # An exception can arrive before or after the body moved a binding, so a handler starts
        # with every binding the body moves already gone: unwinding drops the ones still held.
        node["unwind_drops"] = self.join_drops(environment, moved, after)
        node["join_drops"] = self.join_drops(environment, after, total)
        for clause, clause_moved in ends:
            clause["join_drops"] = self.join_drops(environment, clause_moved, total)
        return result, total

    def form_scope(self, node, environment, moved, context):
        type_, after = self.walk(node["body"], environment, moved, context)
        guard_context = context.nested(guards=context.guards + 1, loops=0)
        for guard in node["guards"]:
            _, after = self.walk(guard["body"], environment, after, guard_context)
        return type_, after

    def closure_environment(self, node, environment, moved):
        captured: dict[str, Binding] = {}
        owned_only = True
        self.borrows_frame = False
        self.borrows_parameters: frozenset[int] = frozenset()
        for capture in node["captures"]:
            source = environment.get(capture["name"])
            if source is None:
                raise StaticError(f"capture of unbound local {capture['name']!r}", "F-DIAG-UNBOUND-NAME")
            if source.id in moved:
                raise StaticError(f"{capture['name']!r} is captured after it was moved", "F-DIAG-USE-AFTER-MOVE")
            mode = capture["ownership"]
            if mode in {"borrow", "borrow-mut"}:
                owned_only = False
                loan = Binding(capture["name"], source.type, True, mode == "borrow-mut", local=False)
                loan.parameter = -1  # reached through the closure being run, which position -1 names
                captured[capture["name"]] = loan
                # The closure holds a loan of a place this frame owns, directly or through a
                # captured closure that does.
                holds = source.type[4] if isinstance(source.type, tuple) and source.type[0] == "callable" and len(source.type) > 4 else False
                self.borrows_frame = self.borrows_frame or (source.local and not source.loan) or holds
                if source.parameter is not None:
                    self.borrows_parameters |= {source.parameter}
                if isinstance(source.type, tuple) and source.type[0] == "callable" and len(source.type) > 5:
                    self.borrows_parameters |= source.type[5]
                continue
            if mode == "copy" and not self.is_copy(source.type):
                raise StaticError(f"copy capture of non-Copy binding {capture['name']!r}", "F-DIAG-COPY-EVIDENCE")
            if mode == "steal" and not self.is_copy(source.type):
                moved = moved | {source.id}
            captured[capture["name"]] = Binding(capture["name"], source.type, False, False, local=False)
            captured[capture["name"]].parameter = -1  # owned by the closure being run
        return captured, moved, owned_only

    def form_lambda(self, node, environment, moved, context):
        if "function" in node:
            definition = self.definitions[node["function"]]
            parameters = [(p["ownership"], self.parse_type(p["type"])) for p in definition["parameters"]]
            return ("callable", parameters, self.parse_type(definition["result"]), True), moved
        captured, moved, owned_only = self.closure_environment(node, environment, moved)
        borrows_frame, borrows_parameters = self.borrows_frame, self.borrows_parameters
        result = self.callable_body(node["parameters"], node["body"], captured, Context("lambda"))
        parameters = [(p["ownership"], self.parse_type(p["type"])) for p in node["parameters"]]
        return ("callable", parameters, result, owned_only, borrows_frame, borrows_parameters), moved

    def form_generator_create(self, node, environment, moved, context):
        definition = self.definitions[node["callee"]]
        parameters = [(p["ownership"], self.parse_type(p["type"])) for p in definition["parameters"]]
        types, moved = self.arguments(node, parameters, environment, moved, context)
        if any(loan_of(type_) and not self.is_copy(underlying(type_)) for type_ in types):
            # A dormant or suspended fiber outlives the call that made it.
            raise StaticError("a generator cannot be given a borrowed argument", "F-DIAG-TRANSFER-REQUIRES-OWNED")
        return ("generator", node["callee"]), moved

    def form_generator_op(self, node, environment, moved, context):
        operation = node["operation"]
        target, moved = self.walk(node["target"], environment, moved, context)
        generator = underlying(target)
        name = generator[1] if isinstance(generator, tuple) and generator[0] == "generator" else None
        for argument in node["arguments"]:
            _, moved = self.walk(argument, environment, moved, context)
        if name is not None and operation == "reply" and len(node["arguments"]) != len(self.definitions[name]["parameters"]):
            raise StaticError(f"reply takes the same arguments as {name}", "F-DIAG-REPLY-ARGUMENTS")
        if operation == "start":
            return generator, moved
        if operation == "empty?":
            return "bool", moved
        if operation == "pop-front":
            return "unit", moved
        item = self.generator_items.get(name, "unknown") if name else "unknown"
        return (item if self.is_copy(item) else ("loan", item, True)), moved

    def form_spawn(self, node, environment, moved, context):
        type_, moved = self.walk(node["operand"], environment, moved, context)
        if not (isinstance(type_, tuple) and type_[0] == "callable"):
            raise StaticError("spawn requires an owned callable")
        if not type_[3]:
            raise StaticError("a spawned callable cannot capture a loan", "F-DIAG-TRANSFER-REQUIRES-OWNED")
        return ("task", type_[2]), moved

    def form_join(self, node, environment, moved, context):
        type_, moved = self.walk(node["operand"], environment, moved, context)
        return (type_[1] if isinstance(type_, tuple) and type_[0] == "task" else "unknown"), moved

    def form_cancel(self, node, environment, moved, context):
        _, moved = self.walk(node["operand"], environment, moved, context)
        return "unit", moved

    def host(self, node, environment, moved, context):
        for argument in node["arguments"]:
            _, moved = self.walk(argument, environment, moved, context)
        operation = self.operations.get(node["operation"], {})
        return self.parse_type(operation.get("result_type", "int")), moved

    form_effect = host
    form_await = host

    def form_emit(self, node, environment, moved, context):
        _, moved = self.walk(node["event"], environment, moved, context)
        return "unit", moved


def static_check(program: dict[str, Any]) -> Checker:
    """Decide every read's ownership mode on a resolved program, or raise ``StaticError``."""
    checker = Checker(program)
    checker.run()
    return checker
