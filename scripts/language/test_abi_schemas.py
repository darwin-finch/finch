#!/usr/bin/env python3
"""Regression tests for native/portable ABI separation and fixed-width wire identity."""

from __future__ import annotations

import copy
import json
import unittest
from pathlib import Path

from jsonschema import Draft202012Validator


ROOT = Path(__file__).resolve().parents[2]
LANG = ROOT / "docs/language"


class AbiSchemaTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        fixtures = json.loads((LANG / "fixtures/schema-instances.json").read_text())
        cls.message = fixtures["portable_message"]
        cls.native = fixtures["native_call"]
        cls.message_validator = Draft202012Validator(json.loads((LANG / "schemas/portable-message-abi.schema.json").read_text()))
        cls.native_validator = Draft202012Validator(json.loads((LANG / "schemas/native-call-abi.schema.json").read_text()))

    def test_json_integer_cannot_replace_fixed_width_sequence(self) -> None:
        message = copy.deepcopy(self.message)
        message["sequence"] = 0
        self.assertTrue(
            list(self.message_validator.iter_errors(message)),
            "portable sequence accepted a precision-losing JSON integer",
        )

    def test_inline_result_cannot_claim_release_operation(self) -> None:
        message = copy.deepcopy(self.message)
        message["result"]["release_operation_key"] = "00000002"
        self.assertTrue(
            list(self.message_validator.iter_errors(message)),
            "inline scalar result incorrectly accepted owned-result release machinery",
        )

    def test_native_descriptor_cannot_admit_unwind(self) -> None:
        native = copy.deepcopy(self.native)
        native["unwind"] = "allowed"
        self.assertTrue(
            list(self.native_validator.iter_errors(native)),
            "native descriptor allowed a Finch unwind to cross the foreign frame",
        )


if __name__ == "__main__":
    unittest.main()
