#!/usr/bin/env python3
"""Generate docs/language/spec-prelude.json from its reviewed definitions."""

from __future__ import annotations

import argparse
import hashlib
import json
import re
from pathlib import Path

from signature_parser import (
    generic_environment,
    matching_angle,
    parse_callable_signature,
    parse_concept_signature,
    parse_kind_signature,
    validate_prelude,
)


ROOT = Path(__file__).resolve().parents[2]
SOURCE = ROOT / "docs/language/prelude-definitions.json"
OUTPUT = ROOT / "docs/language/spec-prelude.json"


def canonical_bytes(value: object) -> bytes:
    return json.dumps(value, ensure_ascii=False, separators=(",", ":"), sort_keys=True).encode()


def render() -> str:
    definitions = json.loads(SOURCE.read_text())
    errors = validate_prelude(definitions)
    if errors:
        raise ValueError("invalid prelude definitions:\n" + "\n".join(errors))
    constructors = {entry["name"]: parse_kind_signature(entry["kind"]) for entry in definitions["types"]}
    for concept in definitions["concepts"]:
        match = re.search(r"\bconcept\s+([A-Za-z_][A-Za-z0-9_?!-]*)\s*<", concept["signature"])
        if match:
            opening = concept["signature"].index("<", match.start())
            closing = matching_angle(concept["signature"], opening)
            constructors[concept["name"]] = (list(generic_environment(concept["signature"][opening + 1 : closing]).values()), "Evidence")
        else:
            constructors[concept["name"]] = ([], "Evidence")
    types = [{**entry, "kind_ast": {"parameters": parse_kind_signature(entry["kind"])[0], "result": parse_kind_signature(entry["kind"])[1]}} for entry in definitions["types"]]
    concepts = []
    for entry in definitions["concepts"]:
        signature_ast = parse_concept_signature(entry["signature"], constructors)
        signature_ast["trusted_laws"] = entry.get("trusted_laws", [])
        concepts.append({**{key: value for key, value in entry.items() if key != "trusted_laws"}, "signature_ast": signature_ast})
    operations = [{**entry, "signature_ast": parse_callable_signature(entry["signature"], constructors)} for entry in definitions["operations"]]
    digest = hashlib.sha256(canonical_bytes(definitions)).hexdigest()
    generated = {
        "schema_version": 2,
        "language_version": definitions["language_version"],
        "generated_from": "prelude-definitions.json",
        "source_sha256": digest,
        "unicode_version": definitions["unicode_version"],
        "types": types,
        "concepts": concepts,
        "core_forms": definitions["core_forms"],
        "operations": operations,
        "reader_literals": definitions["reader_literals"],
        "reserved_effect_predicates": definitions["reserved_effect_predicates"],
        "extension_rule": "An absent name may be supplied by a library but is not required by language version 0.1."
    }
    return json.dumps(generated, ensure_ascii=False, indent=2) + "\n"


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--write", action="store_true")
    args = parser.parse_args()
    expected = render()
    if args.write:
        OUTPUT.write_text(expected)
        return 0
    if not OUTPUT.exists() or OUTPUT.read_text() != expected:
        print(f"{OUTPUT.relative_to(ROOT)} is stale; run {Path(__file__).relative_to(ROOT)} --write")
        return 1
    print("generated prelude is current")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
