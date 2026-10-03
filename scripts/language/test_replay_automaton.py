#!/usr/bin/env python3
"""Regression tests for durable replay admission."""

from __future__ import annotations

import copy
import json
import sys
import unittest
from pathlib import Path


SCRIPT_DIR = Path(__file__).resolve().parent
ROOT = SCRIPT_DIR.parents[1]
sys.path.insert(0, str(SCRIPT_DIR))

from check_language_spec import execute_replay_automaton  # noqa: E402


class ReplayAutomatonTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        cls.automaton = json.loads((ROOT / "docs/language/semantics/replay-automaton.json").read_text())

    def test_unknown_program_word_is_rejected(self) -> None:
        automaton = copy.deepcopy(self.automaton)
        automaton["rules"][0]["actions"] = ["dispatch-again-by-mistake"]
        state = {"generation": "0000000000000001", "next_sequence": "0000000000000000", "terminal": True, "requests": {}}
        event = {"kind": "effect", "generation": "0000000000000001", "sequence": "0000000000000000", "request_id": "0" * 32, "fingerprint": "0" * 64}
        with self.assertRaisesRegex(ValueError, "unknown replay program words"):
            execute_replay_automaton(automaton, state, event)


if __name__ == "__main__":
    unittest.main()
