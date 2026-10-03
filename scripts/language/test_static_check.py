#!/usr/bin/env python3
"""Regression tests proving ownership decisions are made before execution, not during it."""

from __future__ import annotations

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
from reference_machine import MachineError, execute, prepare  # noqa: E402
from static_check import StaticError, static_check  # noqa: E402


TOKEN = {"lifecycle": ["Token"]}


def check(source: str, context: dict | None = None):
    return static_check(prepare(strip_spans(elaborate("colisp", source)), context))


class StaticOwnershipTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        cls.rules = json.loads((LANG / "semantics/transitions.json").read_text())
        cls.vectors = json.loads((LANG / "fixtures/execution-vectors.json").read_text())["vectors"]
        cls.rejections = json.loads((LANG / "fixtures/static-rejections.json").read_text())["vectors"]
        cls.replay = staticmethod(replay_acceptor())

    def test_every_executed_read_matches_its_static_decision(self) -> None:
        compared = 0
        for vector in self.vectors:
            program = prepare(vector["ast"], vector.get("context"))
            ownership = static_check(program)
            _, machine = execute(self.rules, vector["ast"], vector.get("context"), self.replay, program)
            for read in ownership.reads:
                observed = machine.read_modes.get(id(read))
                if observed is None:
                    continue
                compared += 1
                self.assertEqual(
                    observed,
                    {read["static_mode"]},
                    f"{vector['id']}: read of {read['place']!r} decided {read['static_mode']} statically, executed as {sorted(observed)}",
                )
        self.assertGreater(compared, 50, "the comparison must cover real reads, not pass vacuously")

    def test_every_rejection_is_rejected_without_executing(self) -> None:
        for vector in self.rejections:
            with self.subTest(vector=vector["id"]):
                if "ast" not in vector:
                    continue  # refused by the frontends; test_reference_machine asserts that
                with self.assertRaises(MachineError) as caught:
                    static_check(prepare(vector["ast"], vector.get("context")))
                self.assertEqual(caught.exception.code, vector["code"], f"{vector['id']}: {caught.exception}")

    def test_generator_item_and_reply_types_are_fixed_by_its_signature(self) -> None:
        """Rules a type checker owns and the dynamic machine cannot see, so no rejection vector pins them."""
        cases = {
            "F-DIAG-YIELD-TYPE": [
                ('(define (g) : string ! inferred (begin (yield 1) "x")) (let [x (g)] 0)', "a returned value is the last item, so it has the item type"),
                ('(define (g) : unit ! inferred (begin (yield 1) (yield "x"))) (let [x (g)] 0)', "every yield in one body yields one type"),
            ],
            "F-DIAG-REPLY-ARGUMENTS": [
                ("(define (g (n : int)) : unit ! inferred (yield n)) (let [x (g 1)] (reply x 1 2))", "reply takes exactly the generator's parameters"),
            ],
        }
        for code, programs in cases.items():
            for source, rule in programs:
                with self.subTest(rule=rule):
                    with self.assertRaises(StaticError) as caught:
                        static_check(prepare(strip_spans(elaborate("colisp", source)), None))
                    self.assertEqual(caught.exception.code, code, f"{rule}: rejected as {caught.exception.code}: {caught.exception}")
        accepted = "(define (g (n : int)) : int ! inferred (begin (yield n) (+ n 1))) (let [x (g 1)] (front x))"
        program = prepare(strip_spans(elaborate("colisp", accepted)), None)
        checker = static_check(program)
        self.assertEqual(checker.generator_items["g"], "int", "a generator whose yields and result agree has that item type")

    def test_move_on_one_branch_invalidates_the_binding_after_the_join(self) -> None:
        source = "(let [a : Token (Token :id 1)] (begin (if true (drop a) ()) (match a (_ 7))))"
        with self.assertRaises(StaticError) as caught:
            check(source, TOKEN)
        self.assertEqual(caught.exception.code, "F-DIAG-USE-AFTER-MOVE", "a binding moved on any incoming path is moved after the join")

    def test_move_inside_a_loop_is_rejected(self) -> None:
        source = "(let [a : Token (Token :id 1)] (while true (drop a)))"
        with self.assertRaises(StaticError) as caught:
            check(source, TOKEN)
        self.assertEqual(caught.exception.code, "F-DIAG-USE-AFTER-MOVE", "the second iteration would read a moved binding")

    def test_copy_values_are_never_moved(self) -> None:
        ownership = check("(let [x : int 1] (+ x (+ x x)))")
        self.assertEqual({read["static_mode"] for read in ownership.reads}, {"copy"}, "reading an int any number of times is a copy")

    def test_callee_body_is_decided_independently_of_its_call_sites(self) -> None:
        body = "(define (peek (t : Token)) : int ! plain (match t (_ 1)))"
        for call in ("(peek (Token :id 1))", "(+ (peek (Token :id 1)) 0)"):
            with self.subTest(call=call):
                source = f"{body} {call}"
                program = prepare(strip_spans(elaborate("colisp", source)), TOKEN)
                ownership = static_check(program)
                _, machine = execute(self.rules, {}, TOKEN, self.replay, program)
                reads = [read for read in ownership.reads if read["place"] == "t"]
                self.assertEqual([read["static_mode"] for read in reads], ["borrow"], "a borrowed parameter is read as a borrow")
                self.assertEqual(machine.read_modes[id(reads[0])], {"borrow"}, "tail and non-tail calls must execute the same body the same way")

    def test_field_types_are_learned_before_their_uses(self) -> None:
        source = "(define (first (p : Pair)) : int ! plain (. p b)) (+ (first (Pair :a (Token :id 1) :b 2)) 0)"
        ownership = check(source, TOKEN)
        self.assertTrue(ownership.records["Pair"], "a record constructed after the function that reads it must still be known")


if __name__ == "__main__":
    unittest.main()
