#!/usr/bin/env python3
"""Regression tests for canonical transition-program validation."""

from __future__ import annotations

import copy
import json
import sys
import unittest
from pathlib import Path


SCRIPT_DIR = Path(__file__).resolve().parent
ROOT = SCRIPT_DIR.parents[1]
sys.path.insert(0, str(SCRIPT_DIR))

from check_language_spec import validate_transition_programs  # noqa: E402


class TransitionProgramTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        cls.rules = json.loads((ROOT / "docs/language/semantics/transitions.json").read_text())

    def test_checked_in_instruction_vocabulary_covers_every_rule_program(self) -> None:
        self.assertEqual(
            validate_transition_programs(self.rules),
            [],
            "every canonical transition instruction must have a declared operation identity",
        )

    def test_unknown_rule_operation_is_rejected_even_when_rule_is_pending(self) -> None:
        rules = copy.deepcopy(self.rules)
        rules["rules"][0]["program"][0]["op"] = "not-a-machine-operation"
        errors = validate_transition_programs(rules)
        self.assertTrue(
            any("unknown operation 'not-a-machine-operation'" in error for error in errors),
            f"unknown transition instruction was accepted: {errors!r}",
        )


if __name__ == "__main__":
    unittest.main()
