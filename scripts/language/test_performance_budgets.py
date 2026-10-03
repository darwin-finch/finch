#!/usr/bin/env python3
"""Regression tests for Finch's pay-for-use performance boundaries."""

from __future__ import annotations

import copy
import json
import sys
import unittest
from pathlib import Path


SCRIPT_DIR = Path(__file__).resolve().parent
ROOT = SCRIPT_DIR.parents[1]
sys.path.insert(0, str(SCRIPT_DIR))

from check_language_spec import validate_performance_budgets  # noqa: E402


class PerformanceBudgetTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        cls.budgets = json.loads((ROOT / "docs/language/semantics/performance-budgets.json").read_text())

    def test_checked_in_budgets_are_well_formed(self) -> None:
        self.assertEqual(validate_performance_budgets(self.budgets), [])

    def test_hidden_continuation_allocation_cannot_enter_zero_tax_budget(self) -> None:
        budgets = copy.deepcopy(self.budgets)
        budgets["runtime_semantic_taxes"]["non_suspending_direct_call_continuation_allocations"] = 1
        errors = validate_performance_budgets(budgets)
        self.assertTrue(any("must remain exactly zero" in error for error in errors), errors)


if __name__ == "__main__":
    unittest.main()
