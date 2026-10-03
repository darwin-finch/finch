#!/usr/bin/env python3
"""Regression tests for canonical JSON admission."""

from __future__ import annotations

import sys
import unittest
from pathlib import Path


SCRIPT_DIR = Path(__file__).resolve().parent
sys.path.insert(0, str(SCRIPT_DIR))

from check_language_spec import canonical_bytes, strict_json_loads  # noqa: E402


class CanonicalJsonTests(unittest.TestCase):
    def test_duplicate_key_is_rejected_before_dictionary_construction(self) -> None:
        with self.assertRaisesRegex(ValueError, "duplicate JSON object key"):
            strict_json_loads('{"a":1,"a":2}', "fixture")

    def test_nfc_colliding_keys_are_rejected(self) -> None:
        with self.assertRaisesRegex(ValueError, "collide after NFC"):
            strict_json_loads('{"é":1,"e\\u0301":2}', "fixture")

    def test_surrogate_value_is_rejected(self) -> None:
        value = strict_json_loads('{"value":"\\ud800"}', "fixture")
        with self.assertRaisesRegex(ValueError, "surrogate"):
            canonical_bytes(value)


if __name__ == "__main__":
    unittest.main()
