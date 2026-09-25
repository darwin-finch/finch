#!/usr/bin/env python3
"""Structural assertion for the supervised-test isolation shape in scripts/factory/gates.

A library test that mutates state shared by the whole supervised process tree —
the sealed wrapper-proof inode, the sealed listeners, or the process-relative
root — or refuses a broad test process outright must never run inside the broad
concurrent suite: a concurrent validator observes the mutation window and fails
with a sealed-state error (issue #1116: a production-constructor forgery child
rewrote the shared sealed proof inode while another test's subprocess validated
it). The reviewed contract is therefore a pair: the broad library gate skips the
test, and a dedicated exact gate line proves the test alone in its own
supervisor-owned subprocess. This checker pins that pairing so the isolation
regresses the gate here instead of as an intermittent concurrent failure.
"""

from __future__ import annotations

import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
GATES_DOCUMENT = Path("scripts/factory/gates")

# Full libtest paths whose broad-run execution is forbidden and whose dedicated
# exact gate line is required. Ordered by file appearance for readable diffs.
ISOLATED_TESTS = (
    "server::tests::supervised_http_fixture_pins_state_root_across_ancestor_swap",
    "server::tests::production_constructor_rejects_unverified_environment_before_store_mutation",
    "server::tests::production_constructor_rejects_rewritten_proof_and_accepts_exact_restore",
)

FULL_STAGE_RE = re.compile(r"^  full\)\n(.*?)^    ;;$", re.MULTILINE | re.DOTALL)
LIBRARY_GATE_RE = re.compile(
    r'^\s*gate\s+"library tests"\s+\./scripts/test_brains\.sh\s+cargo\s+test\s+--lib\b[^\n]*',
    re.MULTILINE,
)
SKIP_RE = re.compile(r"--skip\s+(\S+)")
EXACT_GATE_RE = re.compile(
    r'^\s*gate\s+"([^"]+)"\s+\./scripts/test_brains\.sh\s+'
    r"cargo\s+test\s+-p\s+finch\s+--lib\s+(\S+)\s+--\s+--exact(?:\s|$)",
    re.MULTILINE,
)


def full_stage(text: str) -> str | None:
    match = FULL_STAGE_RE.search(text)
    return match.group(1) if match else None


def duplicates(names: list[str]) -> list[str]:
    return sorted({name for name in names if names.count(name) > 1})


def check_gates(text: str) -> list[str]:
    errors: list[str] = []
    stage = full_stage(text)
    if stage is None:
        return [f"{GATES_DOCUMENT}: the full stage block is missing or malformed"]

    library_gates = LIBRARY_GATE_RE.findall(stage)
    if len(library_gates) != 1:
        return [
            f"{GATES_DOCUMENT}: expected exactly one broad 'library tests' gate in the full "
            f"stage passing through ./scripts/test_brains.sh, found {len(library_gates)}"
        ]
    skips = SKIP_RE.findall(library_gates[0])
    for name in duplicates(skips):
        errors.append(
            f"{GATES_DOCUMENT}: the broad library gate skips {name} more than once"
        )
    for name in ISOLATED_TESTS:
        count = skips.count(name)
        if count == 0:
            errors.append(
                f"{GATES_DOCUMENT}: the broad library run does not skip {name}; a test that "
                "mutates shared supervised state must be excluded from the concurrent suite"
            )
        elif count > 1:
            errors.append(f"{GATES_DOCUMENT}: the broad library run skips {name} {count} times")
    for name in skips:
        if name not in ISOLATED_TESTS:
            errors.append(
                f"{GATES_DOCUMENT}: broad-run skip {name} is not in the reviewed isolation "
                "inventory of check_factory_gates.py; add it with evidence that it mutates "
                "shared supervised state, and give it a dedicated exact gate line"
            )

    exact_gates = EXACT_GATE_RE.findall(stage)
    exact_names = [name for _, name in exact_gates]
    for name in duplicates(exact_names):
        errors.append(
            f"{GATES_DOCUMENT}: more than one dedicated exact gate line runs {name}"
        )
    for name in ISOLATED_TESTS:
        count = exact_names.count(name)
        if count == 0:
            errors.append(
                f"{GATES_DOCUMENT}: no dedicated exact gate line runs {name} alone through "
                "./scripts/test_brains.sh; every skipped test needs its own exact subprocess "
                "gate in the full stage"
            )
        elif count > 1:
            errors.append(
                f"{GATES_DOCUMENT}: {count} exact gate lines run {name}; keep one per test"
            )
    for _, name in exact_gates:
        if name not in ISOLATED_TESTS:
            errors.append(
                f"{GATES_DOCUMENT}: exact gate line for {name} is not in the reviewed isolation "
                "inventory of check_factory_gates.py; enroll it or remove the gate line"
            )
    return errors


def self_test() -> int:
    errors: list[str] = []
    gates_text = (ROOT / GATES_DOCUMENT).read_text()
    if check_gates(gates_text):
        errors.append(f"the current {GATES_DOCUMENT} was rejected by its own shape check")

    def expect_failure(label: str, mutant: str, fragment: str) -> None:
        found = check_gates(mutant)
        if not any(fragment in error for error in found):
            errors.append(
                f"isolation mutant escaped ({label}): expected {fragment!r}, got {found}"
            )

    name = ISOLATED_TESTS[1]
    other = ISOLATED_TESTS[2]
    containment = ISOLATED_TESTS[0]
    expect_failure(
        "missing broad-run skip",
        gates_text.replace(f" --skip {name}", "", 1),
        f"the broad library run does not skip {name}",
    )
    expect_failure(
        "missing exact gate line",
        gates_text.replace(f" --lib {name} -- --exact", f" --lib {name} -- --exact-skipped", 1),
        f"no dedicated exact gate line runs {name}",
    )
    expect_failure(
        "renamed test",
        gates_text.replace(f" --lib {name} -- --exact", f" --lib {name}_renamed -- --exact", 1),
        "is not in the reviewed isolation inventory",
    )
    expect_failure(
        "unreviewed broad-run skip",
        gates_text.replace(
            f" --skip {other}",
            f" --skip {other} --skip server::tests::not_in_the_inventory",
            1,
        ),
        "server::tests::not_in_the_inventory is not in the reviewed isolation inventory",
    )
    duplicate_line = (
        f'gate "exact duplicate" ./scripts/test_brains.sh cargo test -p finch --lib {other} '
        '-- --exact\n    gate "exact HTTP fixture containment"'
    )
    expect_failure(
        "duplicated exact gate line",
        gates_text.replace('gate "exact HTTP fixture containment"', duplicate_line, 1),
        f"more than one dedicated exact gate line runs {other}",
    )
    expect_failure(
        "exact gate leaves the supervised launcher",
        gates_text.replace(
            'gate "exact HTTP fixture containment" ./scripts/test_brains.sh cargo test',
            'gate "exact HTTP fixture containment" cargo test',
            1,
        ),
        f"no dedicated exact gate line runs {containment}",
    )
    expect_failure(
        "broad library gate leaves the supervised launcher",
        gates_text.replace(
            'gate "library tests" ./scripts/test_brains.sh cargo test --lib',
            'gate "library tests" cargo test --lib',
            1,
        ),
        "expected exactly one broad 'library tests' gate",
    )
    expect_failure(
        "missing full stage",
        gates_text.replace("  full)\n", "  allstages)\n", 1),
        "the full stage block is missing or malformed",
    )

    # The gate label is prose, not contract: renaming one stays accepted so the
    # checker keys on the command shapes rather than on wording.
    relabeled = gates_text.replace("exact HTTP fixture containment", "renamed containment", 1)
    if check_gates(relabeled):
        errors.append("a gate-label-only rename was rejected; labels are not load-bearing")

    if errors:
        for error in errors:
            print(f"factory gates checker self-test: {error}", file=sys.stderr)
        return 1
    print("factory gates checker self-test: isolation pairing probes passed")
    return 0


def main() -> int:
    errors = check_gates((ROOT / GATES_DOCUMENT).read_text())
    if errors:
        for error in errors:
            print(f"factory gates check: {error}", file=sys.stderr)
        return 1
    print(f"factory gates check: supervised isolation pairing holds for {len(ISOLATED_TESTS)} tests")
    return 0


if __name__ == "__main__":
    if sys.argv[1:] == ["--self-test"]:
        raise SystemExit(self_test())
    if sys.argv[1:]:
        print("usage: check_factory_gates.py [--self-test]", file=sys.stderr)
        raise SystemExit(2)
    raise SystemExit(main())
