#!/usr/bin/env python3
"""Fixture tests for linear_ready. Does not call the linear CLI."""

from __future__ import annotations

from linear_ready import blocked_by_map, is_ready, string_waves
from ticket_poset import CycleError

READY_BODY = """## Acceptance criteria
- done
owner: @x
repository: acme/app
value: 3
cost: 1
certainty: 5
unblocking: 1
"""


def test_repo_and_readiness() -> None:
    ok = {"identifier": "ENG-1", "description": READY_BODY}
    assert is_ready(ok, "acme/app")
    assert not is_ready(ok, "other/repo")
    bare = {"identifier": "ENG-2", "description": "owner: @x\nacceptance\nvalue: 3\ncost: 1\ncertainty: 5\nunblocking: 1\n"}
    assert not is_ready(bare, "acme/app")


def test_blocks_edge() -> None:
    issues = [
        {
            "identifier": "ENG-1",
            "description": READY_BODY,
            "relations": [{"type": "blocks", "relatedIssue": {"identifier": "ENG-2"}}],
        },
        {"identifier": "ENG-2", "description": READY_BODY},
        {"identifier": "ENG-3", "description": READY_BODY},
    ]
    mapping = blocked_by_map(issues, "acme/app")
    assert mapping["ENG-2"] == ["ENG-1"]
    planned = string_waves(mapping)
    assert planned[0] == ["ENG-1", "ENG-3"] or set(planned[0]) == {"ENG-1", "ENG-3"}
    assert "ENG-2" in planned[1]


def test_inverse_blocks() -> None:
    issues = [
        {"identifier": "ENG-1", "description": READY_BODY},
        {
            "identifier": "ENG-2",
            "description": READY_BODY,
            "inverseRelations": [{"type": "blocks", "issue": {"identifier": "ENG-1"}}],
        },
    ]
    mapping = blocked_by_map(issues, "acme/app")
    assert "ENG-1" in mapping["ENG-2"]


def test_cycle() -> None:
    issues = [
        {
            "identifier": "ENG-1",
            "description": READY_BODY,
            "relations": [{"type": "blocks", "identifier": "ENG-2"}],
        },
        {
            "identifier": "ENG-2",
            "description": READY_BODY,
            "relations": [{"type": "blocks", "identifier": "ENG-1"}],
        },
    ]
    mapping = blocked_by_map(issues, "acme/app")
    try:
        string_waves(mapping)
    except CycleError:
        return
    raise AssertionError("expected CycleError")


def main() -> int:
    test_repo_and_readiness()
    test_blocks_edge()
    test_inverse_blocks()
    test_cycle()
    print("ok")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
