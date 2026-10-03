#!/usr/bin/env python3
"""Regression tests for session turns and for the direct effect binding."""

from __future__ import annotations

import json
import sys
import unittest
from pathlib import Path


SCRIPT_DIR = Path(__file__).resolve().parent
ROOT = SCRIPT_DIR.parents[1]
LANG = ROOT / "docs/language"
sys.path.insert(0, str(SCRIPT_DIR))

from check_language_spec import check_sessions, replay_acceptor  # noqa: E402
from elaborate import ElaborationError, elaborate, strip_spans  # noqa: E402
from ir_lower import lower  # noqa: E402
from ir_machine import direct_comparable, direct_projection, execute_ir  # noqa: E402
from reference_machine import execute, prepare  # noqa: E402
from session import Session  # noqa: E402
from static_check import static_check  # noqa: E402


class SessionTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        cls.rules = json.loads((LANG / "semantics/transitions.json").read_text())
        cls.sessions = {s["id"]: s for s in json.loads((LANG / "fixtures/session-vectors.json").read_text())["sessions"]}
        cls.vectors = {v["id"]: v for v in json.loads((LANG / "fixtures/execution-vectors.json").read_text())["vectors"]}
        cls.replay = staticmethod(replay_acceptor())

    def run_turn(self, session: Session, source: str) -> dict:
        linked, declarations, declared = session.prepare(strip_spans(elaborate("colisp", source)))
        program = prepare(linked, None)
        static_check(program)
        outcome, _ = execute(self.rules, linked, None, self.replay, program)
        if outcome["terminal"]["kind"] == "complete":
            session.commit(declarations, declared)
        return outcome["terminal"]

    def test_every_session_reproduces(self) -> None:
        errors: list[str] = []
        check_sessions(errors, write=False)
        self.assertEqual(errors, [], "every turn must match its recorded outcome in all three spellings")

    def test_only_a_completed_turn_commits(self) -> None:
        for name in ("failed-turn-commits-nothing", "trapped-turn-commits-nothing", "rejected-turn-commits-nothing", "refused-turn-commits-nothing"):
            first, second = self.sessions[name]["turns"]
            with self.subTest(session=name):
                self.assertFalse(first["expected"]["committed"], "a turn that does not complete commits nothing")
                self.assertEqual(first["expected"]["visible"], {}, "its declarations must not become visible")
                self.assertEqual(second["expected"]["terminal"], {"kind": "rejected", "code": "F-DIAG-UNBOUND-NAME"})

    def test_redefinition_shadows_and_earlier_code_keeps_its_revision(self) -> None:
        session = Session()
        self.run_turn(session, "(define (base) : int ! plain 1) (define (twice) : int ! plain (+ (base) (base))) (twice)")
        terminal = self.run_turn(session, "(define (base) : int ! plain 10) (+ (twice) (base))")
        self.assertEqual(terminal["values"], [12], "twice keeps base@1 (2) and new code sees base@2 (10)")
        self.assertEqual(session.visible, {"base": "base@2", "twice": "twice@1"})

    def test_local_binding_shadows_a_session_function(self) -> None:
        session = Session()
        self.run_turn(session, "(define (base) : int ! plain 1) (base)")
        terminal = self.run_turn(session, "(let [base : int 5] (+ base 1))")
        self.assertEqual(terminal["values"], [6], "a local named like a session function is the local, not a revision")

    def test_recursive_function_calls_its_own_revision(self) -> None:
        session = Session()
        self.run_turn(session, "(define (count (n : int)) : int ! plain (if (== n 0) 0 (+ 1 (count (- n 1))))) (count 2)")
        terminal = self.run_turn(session, "(define (count (n : int)) : int ! plain 100) (+ (count 5) 0)")
        self.assertEqual(terminal["values"], [100], "the new revision is what later turns call")

    def test_coforth_unknown_word_is_rejected_at_construction(self) -> None:
        with self.assertRaises(ElaborationError) as caught:
            elaborate("coforth", "41 inc 0 +")
        self.assertEqual(caught.exception.code, "F-DIAG-UNBOUND-NAME", "a postfix reader cannot guess an unknown word's stack effect")
        known = strip_spans(elaborate("coforth", "41 inc 0 +", None, {"inc": (1, 1, "int")}))
        self.assertEqual(known, strip_spans(elaborate("colisp", "(+ (inc 41) 0)")), "with the session's signatures the turn reads normally")


class DirectBindingTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        cls.rules = json.loads((LANG / "semantics/transitions.json").read_text())
        cls.vectors = {v["id"]: v for v in json.loads((LANG / "fixtures/execution-vectors.json").read_text())["vectors"]}
        cls.replay = staticmethod(replay_acceptor())

    def both(self, name: str):
        vector = self.vectors[name]
        program = prepare(vector["ast"], vector.get("context"))
        module = lower(program, static_check(program))
        mediated = execute_ir(module, program, vector.get("context"), self.replay)
        providers = direct_comparable(vector.get("context"), mediated)
        return vector, program, module, mediated, providers

    def test_direct_binding_keeps_effect_order_and_no_journal(self) -> None:
        vector, program, module, mediated, providers = self.both("effect-resumes-with-value")
        self.assertIsNotNone(providers, "a clean mediated run must be comparable")
        direct = execute_ir(module, program, vector.get("context"), self.replay, "direct", providers)
        self.assertEqual(direct["observable"], ["Call(read-file)", "Complete([6])"], "the request is a plain call to its provider")
        self.assertEqual(direct["observable"], direct_projection(mediated["observable"]))
        self.assertEqual((direct["state"]["journal"], direct["state"]["host_log"], direct["state"]["transaction"]), ([], [], "none"))

    def test_direct_binding_still_runs_cleanup_and_drops(self) -> None:
        vector, program, module, mediated, providers = self.both("effect-resume-cancel-runs-cancel-guards")
        direct = execute_ir(module, program, vector.get("context"), self.replay, "direct", providers)
        self.assertEqual(direct["terminal"], mediated["terminal"], "cancellation unwinds the same way without a broker")
        self.assertEqual([entry for entry in direct["observable"] if entry.startswith("Emit(")], ['Emit(log:"f")', 'Emit(log:"c")'])

    def test_vectors_that_depend_on_mediation_are_not_compared(self) -> None:
        for name in ("effect-without-grant-is-denied", "hostile-resumes-are-rejected-or-replayed", "preflight-refuses-before-anything-runs", "effect-request-parks-until-resumed", "admitted-program-is-still-rechecked-at-dispatch"):
            with self.subTest(vector=name):
                self.assertIsNone(self.both(name)[4], "denial, replay, admission, parking, and revocation exist only under mediation")


if __name__ == "__main__":
    unittest.main()
