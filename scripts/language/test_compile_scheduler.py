#!/usr/bin/env python3
"""Regression tests for the compile-time job scheduler model."""

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

from compile_scheduler import run_case, schedules  # noqa: E402


class CompileSchedulerTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        cls.specification = json.loads((LANG / "semantics/compile-scheduler.json").read_text())
        cls.cases = {case["id"]: case for case in json.loads((LANG / "fixtures/compile-scheduler.json").read_text())["cases"]}

    def test_every_case_has_one_outcome_under_every_schedule(self) -> None:
        self.assertGreater(len(schedules()), 10, "the order-independence claim needs more than a couple of orders")
        for name, case in self.cases.items():
            for schedule in schedules():
                with self.subTest(case=name, schedule=schedule):
                    self.assertEqual(
                        run_case(self.specification, case, **schedule),
                        case["expected"],
                        "scheduling order must not change states, diagnostics, or dependency edges",
                    )

    def test_body_change_reaches_only_body_dependents(self) -> None:
        outcome = self.cases["signature-constant-runs-a-recursive-function"]["expected"]
        self.assertIn(["table", "fib", "body"], outcome["edges"], "a compile-time call depends on the callee's body")
        outcome = self.cases["call-needs-only-the-callee-contract"]["expected"]
        self.assertEqual(outcome["edges"], [["f", "g", "contract"]], "a run-time call depends on the contract only")

    def test_cycle_report_does_not_depend_on_where_it_is_entered(self) -> None:
        case = copy.deepcopy(self.cases["evaluation-cycle-through-a-caller"])
        forward = run_case(self.specification, case, "fifo")
        case["demands"].reverse()
        backward = run_case(self.specification, case, "lifo")
        self.assertEqual(forward["diagnostics"], backward["diagnostics"], "a cycle is reported from its smallest goal whichever job found it")
        self.assertEqual(forward["diagnostics"][0]["goals"][0], "a:SignatureReady")

    def test_primary_diagnostic_is_never_a_dependency_failure(self) -> None:
        for name, case in self.cases.items():
            diagnostics = case["expected"]["diagnostics"]
            if diagnostics:
                with self.subTest(case=name):
                    self.assertNotEqual(diagnostics[0]["code"], "F-DIAG-DEPENDENCY-FAILED", "a consequence must not be reported before its cause")

    def test_changing_executed_path_changes_what_is_required(self) -> None:
        case = copy.deepcopy(self.cases["unexecuted-callee-failure-does-not-block-evaluation"])
        self.assertEqual(run_case(self.specification, case)["states"]["table"], "FunctionCertified")
        case["symbols"]["run"]["requires"][0]["executed"] = True
        outcome = run_case(self.specification, case)
        self.assertEqual(outcome["states"]["table"], "Declared", "an executed callee that cannot be certified blocks the evaluation")
        self.assertIn(["table", "rare", "body"], outcome["edges"])


if __name__ == "__main__":
    unittest.main()
