#!/usr/bin/env python3
"""Pure-function checks for ticket_triage.score and READY vs NEEDS_SPECIFICATION."""

from __future__ import annotations

import json
import subprocess
import sys
import tempfile
from pathlib import Path

from ticket_triage import score


def test_score_formula() -> None:
    issue = {"value": 5, "cost": 2, "certainty": 4, "unblocking": 1}
    assert score(issue) == 5 * 4 * (1 + 1) / 2


def test_score_rejects_partial() -> None:
    assert score({"value": 5, "cost": 2, "certainty": 4}) is None
    assert score({"value": 6, "cost": 2, "certainty": 4, "unblocking": 1}) is None


def test_script_classifies(tmp_path: Path) -> None:
    payload = [
        {
            "number": 1,
            "title": "ready",
            "body": "acceptance criteria and owner: maintainer",
            "value": 3,
            "cost": 1,
            "certainty": 5,
            "unblocking": 1,
            "files": ["src/a.rs"],
        },
        {
            "number": 2,
            "title": "unready",
            "body": "an idea",
            "files": ["src/a.rs"],
        },
    ]
    path = tmp_path / "issues.json"
    path.write_text(json.dumps(payload))
    script = Path(__file__).with_name("ticket_triage.py")
    result = subprocess.run(
        [sys.executable, str(script), str(path)],
        capture_output=True,
        text=True,
        check=False,
    )
    assert result.returncode == 0, result.stderr
    assert "30.00\tREADY\t1\tready\t" in result.stdout
    assert "NEEDS_SPECIFICATION\t2\tunready\tacceptance,owner" in result.stdout
    assert "src/a.rs: #1, #2" in result.stdout


def main() -> int:
    test_score_formula()
    test_score_rejects_partial()
    with tempfile.TemporaryDirectory() as directory:
        test_script_classifies(Path(directory))
    print("ok")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
