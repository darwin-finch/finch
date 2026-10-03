#!/usr/bin/env python3
"""Regression tests proving transitions.json is the executed oracle, not documentation beside one."""

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

from check_language_spec import check_transition_coverage, replay_acceptor, validate_transition_programs  # noqa: E402
from elaborate import ElaborationError, elaborate, strip_spans  # noqa: E402
from reference_machine import MachineError, execute  # noqa: E402


class ReferenceMachineTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        cls.rules = json.loads((LANG / "semantics/transitions.json").read_text())
        cls.vectors = json.loads((LANG / "fixtures/execution-vectors.json").read_text())["vectors"]
        cls.rejections = json.loads((LANG / "fixtures/static-rejections.json").read_text())["vectors"]
        cls.replay = staticmethod(replay_acceptor())

    def vector(self, name: str) -> dict:
        return next(item for item in self.vectors if item["id"] == name)

    def run_vector(self, vector: dict, rules: dict | None = None) -> dict:
        outcome, _ = execute(rules or self.rules, vector["ast"], vector.get("context"), self.replay)
        return outcome

    def mutated(self, rule_id: str, edit) -> dict:
        rules = copy.deepcopy(self.rules)
        rule = next(rule for rule in rules["rules"] if rule["rule"] == rule_id)
        edit(rule["program"])
        return rules

    def test_every_vector_reproduces_its_complete_outcome(self) -> None:
        for vector in self.vectors:
            with self.subTest(vector=vector["id"]):
                outcome = self.run_vector(vector)
                self.assertEqual(outcome["trace"], vector["transitions"], "transition trace must match exactly")
                self.assertEqual(outcome["terminal"], vector["terminal"], "terminal outcome must match exactly")
                self.assertEqual(outcome["state"], vector["state"], "final machine state must match exactly")

    def test_every_static_rejection_reports_its_stable_code(self) -> None:
        for vector in self.rejections:
            with self.subTest(vector=vector["id"]):
                if "ast" not in vector:
                    # Rejected while the program is constructed: every frontend must refuse it.
                    for syntax in ("colisp", "clike", "coforth"):
                        if vector.get(syntax) is None:
                            continue
                        with self.assertRaises(ElaborationError) as refused:
                            elaborate(syntax, vector[syntax])
                        self.assertEqual(refused.exception.code, vector["code"], f"{vector['id']} in {syntax}: {refused.exception}")
                    continue
                with self.assertRaises(MachineError) as caught:
                    execute(self.rules, vector["ast"], vector.get("context"), self.replay)
                self.assertEqual(
                    caught.exception.code,
                    vector["code"],
                    f"{vector['id']} rejected for the wrong reason: {caught.exception}",
                )

    def test_removing_a_rule_instruction_changes_observable_behaviour(self) -> None:
        vector = self.vector("assignment-rhs-first")
        rules = self.mutated("F-DYN-ASSIGN", lambda program: program.pop(3))
        outcome = self.run_vector(vector, rules)
        self.assertNotEqual(
            outcome["trace"],
            vector["transitions"],
            "deleting drop_destination from F-DYN-ASSIGN went unnoticed, so the rule program is not what executes",
        )

    def test_reordering_journal_after_exposure_is_rejected(self) -> None:
        vector = self.vector("emit-journals-before-exposure")

        def swap(program: list) -> None:
            program[1], program[2] = program[2], program[1]

        with self.assertRaises((MachineError, KeyError)) as caught:
            self.run_vector(vector, self.mutated("F-DYN-EMIT", swap))
        self.assertIsNotNone(caught.exception, "exposing an event before journaling it must not execute")

    def test_request_must_be_journaled_before_await(self) -> None:
        vector = self.vector("effect-request-parks-until-resumed")

        def swap(program: list) -> None:
            program[2], program[3] = program[3], program[2]

        with self.assertRaises((MachineError, KeyError)):
            self.run_vector(vector, self.mutated("F-DYN-EFFECT", swap))

    def test_undeclared_instruction_is_not_executed(self) -> None:
        rules = self.mutated("F-DYN-LITERAL", lambda program: program.insert(0, {"op": "teleport"}))
        with self.assertRaisesRegex(MachineError, "undeclared instruction"):
            self.run_vector(self.vector("literal-return"), rules)
        self.assertTrue(
            any("unknown operation 'teleport'" in error for error in validate_transition_programs(rules)),
            "the static program check must also reject an instruction outside the vocabulary",
        )

    def test_rule_that_leaves_two_operands_is_rejected(self) -> None:
        rules = self.mutated("F-DYN-LITERAL", lambda program: program.insert(0, {"op": "push_unit"}))
        with self.assertRaisesRegex(MachineError, "exactly one value"):
            self.run_vector(self.vector("literal-return"), rules)

    def test_tail_calls_run_in_bounded_frames_at_scale(self) -> None:
        source = (
            "(define (count (n : int) (acc : int)) : int ! plain "
            "(if (== n 0) acc (count (- n 1) (+ acc 1)))) (+ (count 20000 0) 0)"
        )
        outcome, _ = execute(self.rules, strip_spans(elaborate("colisp", source)), None, self.replay)
        self.assertEqual(outcome["terminal"], {"kind": "complete", "values": [20000]}, "tail recursion must reach its result")
        self.assertEqual(
            outcome["state"]["max_frames"],
            2,
            f"20000 tail calls must reuse one frame; live frame peak was {outcome['state']['max_frames']}",
        )

    def test_non_tail_recursion_does_grow_frames(self) -> None:
        source = "(define (sum (n : int)) : int ! plain (if (== n 0) 0 (+ n (sum (- n 1))))) (+ (sum 20) 0)"
        outcome, _ = execute(self.rules, strip_spans(elaborate("colisp", source)), None, self.replay)
        self.assertEqual(outcome["terminal"]["values"], [210], "recursive sum must compute its result")
        self.assertEqual(outcome["state"]["max_frames"], 22, "the frame counter must count real frames, not always report a bound")

    def test_coverage_reports_a_rule_with_an_untaken_branch_as_pending(self) -> None:
        errors: list[str] = []
        check_transition_coverage(self.rules, set(), set(), [], errors, write=False)
        self.assertTrue(
            any("executable_rules is stale" in error for error in errors),
            f"coverage with no executed branch still claimed executable rules: {errors[:2]!r}",
        )

    def test_exit_class_selects_guards(self) -> None:
        expected = {
            "scope-runs-success-cleanup-lifo": ['event:log:"s"', 'event:log:"a"'],
            "scope-runs-failure-cleanup-lifo": ['event:log:"f"', 'event:log:"a"'],
            "cancellation-at-loop-safepoint": ['event:log:"tick"', 'event:log:"tick"', 'event:log:"c"', 'event:log:"f"', 'event:log:"a"'],
        }
        for name, journal in expected.items():
            with self.subTest(vector=name):
                self.assertEqual(self.run_vector(self.vector(name))["state"]["journal"], journal, "guards run by exit class, LIFO; on-failure also runs on cancel")

    def test_suppressed_cleanup_failure_never_replaces_the_primary_outcome(self) -> None:
        failure = self.run_vector(self.vector("guard-failure-during-failure-is-suppressed"))["terminal"]
        self.assertEqual((failure["exception"]["value"], failure["exception"]["suppressed"]), (1, ["exception:9"]))
        cancel = self.run_vector(self.vector("guard-failure-during-cancel-is-suppressed"))["terminal"]
        self.assertEqual((cancel["edge"], cancel["suppressed"]), ("Cancel", ["exception:9"]))

    def test_hostile_resumes_never_dispatch_twice(self) -> None:
        outcome = self.run_vector(self.vector("hostile-resumes-are-rejected-or-replayed"))
        accepted = [entry for entry in outcome["state"]["host_log"] if entry.endswith("accepted-dispatch-once")]
        self.assertEqual(len(accepted), 2, f"two requests must be resumed exactly once each: {outcome['state']['host_log']!r}")
        self.assertEqual(
            [entry for entry in outcome["state"]["journal"] if entry.startswith("resume:")],
            ["resume:read-file#0", "resume:read-file#1"],
            "the journal must record one resume per request",
        )


if __name__ == "__main__":
    unittest.main()
