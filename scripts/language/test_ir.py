#!/usr/bin/env python3
"""Regression tests for IR version 6: lowering, structural verification, and execution."""

from __future__ import annotations

import copy
import json
import sys
import unittest
from pathlib import Path


SCRIPT_DIR = Path(__file__).resolve().parent
ROOT = SCRIPT_DIR.parents[1]
LANG = ROOT / "docs/language"
sys.path.insert(0, str(SCRIPT_DIR))

from check_language_spec import replay_acceptor  # noqa: E402
from elaborate import elaborate, strip_spans  # noqa: E402
from admission import manifest_of_ir, manifest_of_program  # noqa: E402
from ir_lower import lower  # noqa: E402
from ir_machine import execute_ir  # noqa: E402
from ir_verify import verify_ir  # noqa: E402
from reference_machine import execute, prepare  # noqa: E402
from static_check import static_check  # noqa: E402


class IrTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        cls.rules = json.loads((LANG / "semantics/transitions.json").read_text())
        cls.specification = json.loads((LANG / "semantics/ir.json").read_text())
        cls.vectors = json.loads((LANG / "fixtures/execution-vectors.json").read_text())["vectors"]
        cls.replay = staticmethod(replay_acceptor())

    def vector(self, name: str) -> dict:
        return next(item for item in self.vectors if item["id"] == name)

    def lowered(self, vector: dict) -> tuple[dict, dict]:
        program = prepare(vector["ast"], vector.get("context"))
        return program, lower(program, static_check(program))

    def compile(self, source: str, context: dict | None = None) -> tuple[dict, dict]:
        program = prepare(strip_spans(elaborate("colisp", source)), context)
        return program, lower(program, static_check(program))

    def test_every_vector_lowers_verifies_and_reproduces_the_reference_machine(self) -> None:
        for vector in self.vectors:
            with self.subTest(vector=vector["id"]):
                program, module = self.lowered(vector)
                self.assertEqual(verify_ir(module, self.specification), [], "lowered IR must verify")
                expected, _ = execute(self.rules, vector["ast"], vector.get("context"), self.replay)
                actual = execute_ir(module, program, vector.get("context"), self.replay)
                for key in ("observable", "terminal", "state"):
                    self.assertEqual(actual[key], expected[key], f"IR {key} must equal the rule machine's")

    def test_removing_a_placed_drop_is_observable(self) -> None:
        vector = self.vector("bindings-drop-in-reverse-order")
        program, module = self.lowered(vector)
        mutated = copy.deepcopy(module)
        removed = False
        for block in mutated["functions"]["<entry>"]["blocks"]:
            for index, instruction in enumerate(block["instructions"]):
                if instruction["op"] == "drop_value" and block["instructions"][-1]["op"] != "resume_unwind":
                    block["instructions"][index] = {"op": "drop"}
                    removed = True
                    break
            if removed:
                break
        self.assertTrue(removed, "the vector must contain a compiler-placed drop on its normal path")
        outcome = execute_ir(mutated, program, vector.get("context"), self.replay)
        self.assertNotEqual(outcome["state"]["drops"], vector["state"]["drops"], "drops exist only where the compiler placed them")

    def test_removing_cleanup_regions_loses_failure_cleanup(self) -> None:
        vector = self.vector("scope-runs-failure-cleanup-lifo")
        program, module = self.lowered(vector)
        mutated = copy.deepcopy(module)
        for block in mutated["functions"]["<entry>"]["blocks"]:
            block["region"] = None
        outcome = execute_ir(mutated, program, vector.get("context"), self.replay)
        self.assertEqual(outcome["state"]["journal"], [], "without regions no guard runs on failure")
        self.assertNotEqual(outcome["state"]["journal"], vector["state"]["journal"])

    def test_tail_calls_reuse_one_frame_at_scale(self) -> None:
        source = (
            "(define (count (n : int) (acc : int)) : int ! plain "
            "(if (== n 0) acc (count (- n 1) (+ acc 1)))) (+ (count 20000 0) 0)"
        )
        program, module = self.compile(source)
        outcome = execute_ir(module, program, None, self.replay)
        self.assertEqual(outcome["terminal"], {"kind": "complete", "values": [20000]})
        self.assertEqual(outcome["state"]["max_frames"], 2, "20000 tail calls must not grow the frame vector")

    def test_ordinary_recursion_grows_frames(self) -> None:
        program, module = self.compile("(define (sum (n : int)) : int ! plain (if (== n 0) 0 (+ n (sum (- n 1))))) (+ (sum 20) 0)")
        outcome = execute_ir(module, program, None, self.replay)
        self.assertEqual((outcome["terminal"]["values"], outcome["state"]["max_frames"]), ([210], 22))

    def test_move_ends_the_binding_cleanup_region(self) -> None:
        source = "(define (eat (steal t : Token)) : int ! plain 2) (let [a : Token (Token :id 1)] (+ (eat a) (/ 1 0)))"
        program, module = self.compile(source, {"lifecycle": ["Token"]})
        outcome = execute_ir(module, program, {"lifecycle": ["Token"]}, self.replay)
        self.assertEqual(outcome["terminal"]["edge"], "Trap", f"unexpected terminal {outcome['terminal']!r}")
        self.assertEqual(outcome["state"]["drops"], ["Token{id=1}"], "a moved binding is dropped once, by its new owner, not again on unwind")

    def test_verifier_rejects_malformed_modules(self) -> None:
        _, module = self.lowered(self.vector("left-to-right-add"))
        entry = "<entry>"

        def mutate(edit) -> list[str]:
            mutated = copy.deepcopy(module)
            edit(mutated["functions"][entry])
            return verify_ir(mutated, self.specification)

        cases = {
            "unknown operation": lambda f: f["blocks"][0]["instructions"].insert(0, {"op": "teleport"}),
            "missing block": lambda f: f["blocks"][0]["instructions"].__setitem__(-1, {"op": "jump", "target": 99}),
            "terminator in the wrong position": lambda f: f["blocks"][0]["instructions"].append({"op": "constant", "value": 1}),
            "extra operands": lambda f: f["blocks"][0]["instructions"].insert(0, {"op": "constant", "value": 1}),
            "has fields": lambda f: f["blocks"][0]["instructions"].__setitem__(0, {"op": "constant"}),
            "needs": lambda f: f["blocks"][0]["instructions"].insert(0, {"op": "drop"}),
        }
        for expected, edit in cases.items():
            with self.subTest(defect=expected):
                errors = mutate(edit)
                self.assertTrue(any(expected in error for error in errors), f"verifier missed {expected!r}: {errors!r}")

    def test_static_argument_claim_is_verified_not_trusted(self) -> None:
        """The manifest read from IR cannot be narrowed by a compiler that lies about its arguments."""
        context = {"operations": {"read-file": {"kind": "effect", "parameters": 1, "result": True}}}
        literal = '(read-file "a")'
        wrapped = '(read-file (begin "a"))'
        for source, expected in ((literal, ["a"]), (wrapped, None)):
            program = prepare(strip_spans(elaborate("colisp", source, context["operations"])), context)
            module = lower(program, static_check(program))
            from_ir = manifest_of_ir(module, context["operations"])["effects"]
            from_source = manifest_of_program(program)["effects"]
            self.assertEqual(from_ir, from_source, f"{source}: the IR manifest must equal the source manifest")
            self.assertEqual(from_ir, [{"operation": "read-file", "arguments": expected}], f"{source}: only a literal written as the argument is static")
        program = prepare(strip_spans(elaborate("colisp", '(let [p "a"] (read-file p))', context["operations"])), context)
        module = lower(program, static_check(program))
        forged = copy.deepcopy(module)
        for function in forged["functions"].values():
            for block in function["blocks"]:
                for instruction in block["instructions"]:
                    if instruction["op"] == "capability_request":
                        instruction["static"] = True
        errors = verify_ir(forged, self.specification)
        self.assertTrue(any("claims static arguments" in error for error in errors), f"a forged static claim must fail verification: {errors!r}")

    def test_tail_call_forward_pairs_are_verified(self) -> None:
        source = "(define (g (t : Token)) : int ! plain 1) (define (peek (t : Token)) : int ! plain (g t)) (peek (Token :id 1))"
        program, module = self.compile(source, {"lifecycle": ["Token"]})
        calls = [i for b in module["functions"]["peek"]["blocks"] for i in b["instructions"] if i["op"] == "tail_call"]
        self.assertEqual([call["forward"] for call in calls], [[[0, 0]]], "peek passes its borrowed parameter on, so the tail call forwards it")
        forged = copy.deepcopy(module)
        for block in forged["functions"]["peek"]["blocks"]:
            for instruction in block["instructions"]:
                if instruction["op"] == "tail_call":
                    instruction["forward"] = [[3, 0]]
        errors = verify_ir(forged, self.specification)
        self.assertTrue(any("forwards" in error for error in errors), f"a forward pair naming no parameter must fail verification: {errors!r}")

    def test_move_inside_try_keeps_its_drop_obligation_inside_the_catch_region(self) -> None:
        """A binding the try body moves is dropped exactly once on every path out of the body."""
        declarations = "(define (eat (steal t : Token)) : int ! plain 2) (define (boom) : int ! inferred (throw 9)) "
        cases = {
            "moved, then completes": "(try (eat a) (catch _ 0))",
            "raises before the move": "(try (+ (boom) (eat a)) (catch _ 0))",
            "raises after the move": "(try (+ (eat a) (boom)) (catch _ 0))",
            "moved only by the handler": "(try (boom) (catch _ (eat a)))",
            "nested try": "(try (try (+ (boom) (eat a)) (catch 1 0)) (catch _ 5))",
        }
        context = {"lifecycle": ["Token"]}
        for name, body in cases.items():
            with self.subTest(case=name):
                source = f"{declarations}(let [a : Token (Token :id 1)] {body})"
                program = prepare(strip_spans(elaborate("colisp", source)), context)
                module = lower(program, static_check(program))
                self.assertEqual(verify_ir(module, self.specification), [], f"{name}: lowered module must verify")
                expected, _ = execute(self.rules, None, context, self.replay, program)
                actual = execute_ir(module, program, context, self.replay)
                self.assertEqual(actual["observable"], expected["observable"], f"{name}: IR and rule machine must drop at the same point")
                self.assertEqual(expected["state"]["drops"], ["Token{id=1}"], f"{name}: the token is dropped exactly once, trace {expected['observable']!r}")


if __name__ == "__main__":
    unittest.main()
