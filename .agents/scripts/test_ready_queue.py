#!/usr/bin/env python3
"""Fixture checks for ready_queue: READY in dependency order, no gh.

NEEDS_SPECIFICATION items (the #4 case) must not appear as dispatchable.
An unready wave-0 blocker must not promote a later READY issue.
"""

from __future__ import annotations

import json
import subprocess
import sys
import tempfile
from pathlib import Path

from ready_queue import dispatchable, load_waves, ready_waves


READY_BODY = "acceptance criteria and owner: maintainer"
READY_AXES = {"value": 5, "cost": 2, "certainty": 5, "unblocking": 3}


def ready_issue(number: int, title: str) -> dict:
    return {"number": number, "title": title, "body": READY_BODY, **READY_AXES}


def test_needs_specification_omitted() -> None:
    waves = load_waves({"waves": [{"wave": 0, "parallel": [4, 10]}]})
    issues = [
        {"number": 4, "title": "clustering", "body": "an idea"},
        ready_issue(10, "ready queue"),
    ]
    filtered = ready_waves(waves, issues)
    assert filtered == [[10]]
    assert dispatchable(filtered) == [10]


def test_dependency_order() -> None:
    waves = load_waves([[10], [12]])
    issues = [ready_issue(12, "later"), ready_issue(10, "first")]
    filtered = ready_waves(waves, issues)
    assert filtered == [[10], [12]]
    assert dispatchable(filtered) == [10]


def test_unready_blocker_does_not_promote() -> None:
    waves = load_waves({"waves": [{"wave": 0, "parallel": [4]}, {"wave": 1, "parallel": [11]}]})
    issues = [
        {"number": 4, "title": "clustering", "body": "an idea"},
        ready_issue(11, "depends on four"),
    ]
    filtered = ready_waves(waves, issues)
    assert filtered == [[], [11]]
    assert dispatchable(filtered) == []


def test_script_omits_needs_specification(tmp_path: Path) -> None:
    waves_path = tmp_path / "waves.json"
    triage_path = tmp_path / "triage.json"
    waves_path.write_text(
        json.dumps({"waves": [{"wave": 0, "parallel": [4, 10]}, {"wave": 1, "parallel": [11]}]})
    )
    triage_path.write_text(
        json.dumps(
            [
                {"number": 4, "title": "clustering", "body": "an idea"},
                ready_issue(10, "ready queue"),
                ready_issue(11, "depends on four"),
            ]
        )
    )
    script = Path(__file__).with_name("ready_queue.py")
    result = subprocess.run(
        [sys.executable, str(script), "--waves", str(waves_path), "--triage", str(triage_path)],
        capture_output=True,
        text=True,
        check=False,
    )
    assert result.returncode == 0, result.stderr
    assert "#4" not in result.stdout
    assert "clustering" not in result.stdout
    assert "#10 ready queue" in result.stdout
    assert "#11" not in result.stdout

    json_result = subprocess.run(
        [
            sys.executable,
            str(script),
            "--waves",
            str(waves_path),
            "--triage",
            str(triage_path),
            "--format",
            "json",
        ],
        capture_output=True,
        text=True,
        check=False,
    )
    assert json_result.returncode == 0, json_result.stderr
    payload = json.loads(json_result.stdout)
    assert payload["dispatchable"] == [10]
    assert payload["ready"] == [10]
    assert payload["waves"] == [
        {"wave": 0, "parallel": [10]},
        {"wave": 1, "parallel": [11]},
    ]
    assert 4 not in payload["dispatchable"]
    assert 4 not in payload["ready"]
    assert 4 not in payload["waves"][0]["parallel"]
    assert 4 not in payload["waves"][1]["parallel"]


def main() -> int:
    test_needs_specification_omitted()
    test_dependency_order()
    test_unready_blocker_does_not_promote()
    with tempfile.TemporaryDirectory() as directory:
        test_script_omits_needs_specification(Path(directory))
    print("ok")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
