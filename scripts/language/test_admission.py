#!/usr/bin/env python3
"""Regression tests for the capability manifest and the admission step."""

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

from admission import admit, grant_allows, manifest_of_ir, manifest_of_program  # noqa: E402
from check_language_spec import replay_acceptor  # noqa: E402
from ir_lower import lower  # noqa: E402
from ir_machine import execute_ir  # noqa: E402
from reference_machine import execute, prepare  # noqa: E402
from static_check import static_check  # noqa: E402


class AdmissionTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        cls.rules = json.loads((LANG / "semantics/transitions.json").read_text())
        cls.vectors = {v["id"]: v for v in json.loads((LANG / "fixtures/execution-vectors.json").read_text())["vectors"]}
        cls.replay = staticmethod(replay_acceptor())

    def lowered(self, vector: dict) -> tuple[dict, dict]:
        program = prepare(vector["ast"], vector.get("context"))
        return program, lower(program, static_check(program))

    def test_manifest_from_ir_equals_manifest_from_program(self) -> None:
        nonempty = 0
        for name, vector in self.vectors.items():
            program, module = self.lowered(vector)
            with self.subTest(vector=name):
                self.assertEqual(manifest_of_ir(module, {}), manifest_of_program(program), "a host must be able to recompute the manifest from IR")
                self.assertEqual(vector["manifest"], manifest_of_program(program), "the pinned manifest must be current")
            nonempty += bool(vector["manifest"]["effects"])
        self.assertGreater(nonempty, 15, "the comparison must cover programs that actually request capabilities")

    def test_refused_program_performs_no_transition(self) -> None:
        vector = self.vectors["preflight-refuses-before-anything-runs"]
        self.assertEqual(vector["transitions"], ['NotAdmitted(read-file("f"))'], "nothing may run before or after a refusal")
        self.assertEqual(
            (vector["state"]["journal"], vector["state"]["transaction"], vector["state"]["max_frames"]),
            ([], "not-started", 0),
            "a refused program journals nothing, starts no transaction, and enters no frame",
        )
        lazy = self.vectors["lazy-admission-fails-at-the-first-dispatch"]
        self.assertEqual(lazy["colisp"], vector["colisp"], "the two vectors must differ only in admission mode")
        self.assertEqual(lazy["state"]["journal"], ['event:log:"started"'], "without preflight the earlier event has already happened")

    def test_ir_executor_refuses_identically(self) -> None:
        vector = self.vectors["preflight-prompt-deny-refuses"]
        program, module = self.lowered(vector)
        outcome = execute_ir(module, program, vector.get("context"), self.replay)
        self.assertEqual(outcome["terminal"], vector["terminal"])
        self.assertEqual(outcome["state"], vector["state"])

    def test_request_added_to_ir_appears_in_the_manifest(self) -> None:
        program, module = self.lowered(self.vectors["literal-return"])
        self.assertEqual(manifest_of_ir(module, {})["effects"], [])
        tampered = copy.deepcopy(module)
        tampered["functions"]["<entry>"]["blocks"][0]["instructions"][:0] = [
            {"op": "constant", "value": "/etc/passwd"},
            {"op": "capability_request", "operation": "read-file", "arity": 1, "check": True, "static": True},
            {"op": "drop"},
        ]
        self.assertEqual(
            manifest_of_ir(tampered, {})["effects"],
            [{"operation": "read-file", "arguments": ["/etc/passwd"]}],
            "a request cannot hide from a manifest derived from the IR that will run",
        )
        del program

    def test_non_static_request_is_not_covered_by_a_restricted_grant(self) -> None:
        restricted = {"read-file": {"arguments": [["a.txt"]]}}
        self.assertTrue(grant_allows(restricted, "read-file", ["a.txt"]))
        self.assertFalse(grant_allows(restricted, "read-file", ["b.txt"]))
        self.assertFalse(grant_allows(restricted, "read-file", None), "an unbounded request needs an unrestricted grant")
        self.assertTrue(grant_allows({"read-file": True}, "read-file", None))

    def test_prompt_answer_grants_only_what_was_asked(self) -> None:
        manifest = {"effects": [{"operation": "read-file", "arguments": ["a.txt"]}], "awaits": [], "events": []}
        decision = admit(manifest, {"grants": {}, "admission": {"mode": "preflight", "prompt": {"read-file": "allow"}}})
        self.assertEqual(decision["refused"], [])
        self.assertEqual(decision["grants"], {"read-file": {"arguments": [["a.txt"]]}}, "allowing one static request must not grant the whole operation")

    def test_admitted_program_matches_between_executors(self) -> None:
        vector = self.vectors["each-uncovered-request-is-prompted"]
        program, module = self.lowered(vector)
        expected, _ = execute(self.rules, vector["ast"], vector.get("context"), self.replay, prepare(vector["ast"], vector.get("context")))
        actual = execute_ir(module, program, vector.get("context"), self.replay)
        self.assertEqual(actual["state"]["host_log"], expected["state"]["host_log"], "both executors must prompt for the same requests in the same order")
        self.assertEqual(actual["terminal"], {"kind": "complete", "values": [3]})


if __name__ == "__main__":
    unittest.main()
