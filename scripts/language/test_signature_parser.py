#!/usr/bin/env python3
"""Regression tests for required-prelude signature parsing and kind checking."""

from __future__ import annotations

import copy
import json
import sys
import unittest
from pathlib import Path


SCRIPT_DIR = Path(__file__).resolve().parent
ROOT = SCRIPT_DIR.parents[1]
sys.path.insert(0, str(SCRIPT_DIR))

from signature_parser import validate_prelude  # noqa: E402


class SignatureParserTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        cls.definitions = json.loads((ROOT / "docs/language/prelude-definitions.json").read_text())

    def test_checked_in_prelude_signatures_parse_and_kind_check(self) -> None:
        self.assertEqual(
            validate_prelude(self.definitions),
            [],
            "the reviewed required-prelude source must parse and kind-check without diagnostics",
        )

    def test_exception_set_used_as_plain_type_is_rejected(self) -> None:
        definitions = copy.deepcopy(self.definitions)
        operation = next(item for item in definitions["operations"] if item["name"] == "join")
        operation["signature"] = operation["signature"].replace("exceptions X", "X")
        errors = validate_prelude(definitions)
        self.assertTrue(
            any("task argument 2" in error and "expected ExceptionSet" in error for error in errors),
            f"kind-invalid task signature was accepted: {errors!r}",
        )

    def test_wrong_path_selector_arity_is_rejected(self) -> None:
        definitions = copy.deepcopy(self.definitions)
        operation = next(item for item in definitions["operations"] if item["name"] == "include-str")
        operation["signature"] = operation["signature"].replace('path<project:"**">', 'path<project,"**">')
        errors = validate_prelude(definitions)
        self.assertTrue(
            any("path expects 1 generic arguments" in error for error in errors),
            f"wrong path-selector arity was accepted: {errors!r}",
        )

    def test_unbalanced_signature_is_rejected(self) -> None:
        definitions = copy.deepcopy(self.definitions)
        definitions["operations"][0]["signature"] += ")"
        errors = validate_prelude(definitions)
        self.assertTrue(
            any("unbalanced delimiter" in error for error in errors),
            f"unbalanced signature was accepted: {errors!r}",
        )

    def test_unknown_generic_constructor_is_rejected(self) -> None:
        definitions = copy.deepcopy(self.definitions)
        operation = next(item for item in definitions["operations"] if item["name"] == "attempt")
        operation["signature"] = operation["signature"].replace("result<T,exception-value<X>>", "reslut<T,exception-value<X>>")
        errors = validate_prelude(definitions)
        self.assertTrue(
            any("unknown generic constructor reslut" in error for error in errors),
            f"unknown generic constructor was accepted: {errors!r}",
        )

    def test_malformed_parameter_is_rejected(self) -> None:
        definitions = copy.deepcopy(self.definitions)
        operation = next(item for item in definitions["operations"] if item["name"] == "join")
        operation["signature"] = operation["signature"].replace("handle:steal task", "handle::steal task")
        errors = validate_prelude(definitions)
        self.assertTrue(
            any("operation join" in error for error in errors),
            f"malformed parameter syntax was accepted: {errors!r}",
        )

    def test_duplicate_contract_axis_is_rejected(self) -> None:
        definitions = copy.deepcopy(self.definitions)
        operation = next(item for item in definitions["operations"] if item["name"] == "length")
        operation["signature"] = operation["signature"].replace("! pure", "! pure | effects<>")
        errors = validate_prelude(definitions)
        self.assertTrue(
            any("duplicate effect axis" in error for error in errors),
            f"duplicate contract axis was accepted: {errors!r}",
        )

    def test_contract_row_parameter_kind_is_checked(self) -> None:
        definitions = copy.deepcopy(self.definitions)
        operation = next(item for item in definitions["operations"] if item["name"] == "attempt")
        operation["signature"] = operation["signature"].replace("effects<e>", "effects<X>")
        errors = validate_prelude(definitions)
        self.assertTrue(
            any("EffectRow" in error for error in errors),
            f"exception-set parameter was accepted as an effect-row tail: {errors!r}",
        )

    def test_loan_bearing_result_requires_a_canonical_origin(self) -> None:
        definitions = copy.deepcopy(self.definitions)
        operation = next(item for item in definitions["operations"] if item["name"] == "front")
        operation["signature"] = operation["signature"].replace(" | returns-loan<arg(source)>", "")
        errors = validate_prelude(definitions)
        self.assertTrue(
            any("loan-bearing result" in error for error in errors),
            f"loan-bearing result without an origin was accepted: {errors!r}",
        )

    def test_nested_loan_result_requires_an_origin(self) -> None:
        definitions = copy.deepcopy(self.definitions)
        operation = next(item for item in definitions["operations"] if item["name"] == "front")
        operation["signature"] = operation["signature"].replace("scoped &R::Item", "option<&R::Item>")
        operation["signature"] = operation["signature"].replace(" | returns-loan<arg(source)>", "")
        errors = validate_prelude(definitions)
        self.assertTrue(
            any("loan-bearing result" in error for error in errors),
            f"nested loan-bearing result without an origin was accepted: {errors!r}",
        )

    def test_return_loan_origin_must_be_a_borrowed_parameter(self) -> None:
        definitions = copy.deepcopy(self.definitions)
        operation = next(item for item in definitions["operations"] if item["name"] == "front")
        operation["signature"] = operation["signature"].replace("source:&R", "source:R")
        errors = validate_prelude(definitions)
        self.assertTrue(
            any("borrowed parameter" in error for error in errors),
            f"by-value return-loan origin was accepted: {errors!r}",
        )

    def test_concept_operation_contract_is_structurally_checked(self) -> None:
        definitions = copy.deepcopy(self.definitions)
        concept = next(item for item in definitions["concepts"] if item["name"] == "Copy")
        concept["signature"] = concept["signature"].replace("! pure", "! pure | effects<>")
        errors = validate_prelude(definitions)
        self.assertTrue(
            any("concept Copy" in error and "duplicate effect axis" in error for error in errors),
            f"malformed concept operation contract was accepted: {errors!r}",
        )

    def test_capability_selectors_are_parsed_not_accepted_as_opaque_text(self) -> None:
        valid = copy.deepcopy(self.definitions)
        operation = next(item for item in valid["operations"] if item["name"] == "length")
        operation["signature"] = operation["signature"].replace(
            "pure", 'fs.read{path=narrow(root(workspace),"assets/**")}'
        )
        self.assertEqual(
            validate_prelude(valid),
            [],
            "the closed selector grammar should accept a nested canonical selector",
        )

        invalid = copy.deepcopy(valid)
        operation = next(item for item in invalid["operations"] if item["name"] == "length")
        operation["signature"] = operation["signature"].replace("root(workspace)", "workspace")
        errors = validate_prelude(invalid)
        self.assertTrue(
            any("invalid selector expression" in error for error in errors),
            f"opaque selector text escaped structural parsing: {errors!r}",
        )

    def test_generated_prelude_contains_structural_signature_asts(self) -> None:
        generated = json.loads((ROOT / "docs/language/spec-prelude.json").read_text())
        self.assertTrue(
            all("signature_ast" in entry for entry in generated["operations"] + generated["concepts"]),
            "every generated callable and concept must carry its structural signature AST",
        )
        encoded = json.dumps([entry["signature_ast"] for entry in generated["operations"]])
        self.assertNotIn(
            '"kind": "predicate"',
            encoded,
            "generated contracts must use typed axis nodes rather than opaque predicate strings",
        )

    def test_ad_hoc_law_member_is_not_a_hidden_signature_language(self) -> None:
        definitions = copy.deepcopy(self.definitions)
        concept = next(item for item in definitions["concepts"] if item["name"] == "HashKey")
        concept["signature"] = concept["signature"].replace(
            " }", "; law equal(a,b) implies hash(a) == hash(b) }"
        )
        errors = validate_prelude(definitions)
        self.assertTrue(
            any("concept HashKey" in error and "invalid concept operation name" in error for error in errors),
            f"non-normative law mini-syntax was accepted: {errors!r}",
        )

    def test_trusted_law_ast_references_are_range_checked(self) -> None:
        definitions = copy.deepcopy(self.definitions)
        concept = next(item for item in definitions["concepts"] if item["name"] == "KnownLength")
        concept["trusted_laws"][0]["expression"]["right"]["ordinal"] = 99
        errors = validate_prelude(definitions)
        self.assertTrue(
            any("concept KnownLength" in error and "invalid generic reference" in error for error in errors),
            f"out-of-range trusted-law reference was accepted: {errors!r}",
        )


if __name__ == "__main__":
    unittest.main()
