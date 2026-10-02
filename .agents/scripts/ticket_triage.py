#!/usr/bin/env python3
"""Report readiness, queue scores, and declared-scope conflicts.

Input is a JSON list of objects with number, title, body, and optional numeric
value, cost, certainty, unblocking, and files fields. This script only reports;
it does not edit issues, assign work, or authorize implementation.

Score is (value × certainty × (1 + unblocking)) / cost on 1–5 axes.
"""

from __future__ import annotations

import argparse
import json
import sys
from pathlib import Path

AXES = ("value", "cost", "certainty", "unblocking")
REQUIRED_MARKERS = ("acceptance", "owner")


def load(path: str | None) -> list[dict]:
    source = Path(path).read_text() if path else sys.stdin.read()
    payload = json.loads(source)
    if not isinstance(payload, list) or not all(isinstance(item, dict) for item in payload):
        raise ValueError("input must be a JSON list of issue objects")
    return payload


def score(issue: dict) -> float | None:
    values = [issue.get(axis) for axis in AXES]
    if not all(isinstance(value, int) and 1 <= value <= 5 for value in values):
        return None
    value, cost, certainty, unblocking = values
    return value * certainty * (1 + unblocking) / cost


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("json_file", nargs="?", help="JSON export; defaults to stdin")
    args = parser.parse_args()
    try:
        issues = load(args.json_file)
    except (OSError, json.JSONDecodeError, ValueError) as error:
        print(f"ticket_triage: {error}", file=sys.stderr)
        return 2

    rows = []
    file_owners: dict[str, list[str]] = {}
    for issue in issues:
        body = str(issue.get("body") or "").lower()
        missing = [marker for marker in REQUIRED_MARKERS if marker not in body]
        current_score = score(issue)
        state = "READY" if not missing and current_score is not None else "NEEDS_SPECIFICATION"
        for file in issue.get("files", []):
            if isinstance(file, str) and file.strip():
                file_owners.setdefault(file.strip(), []).append(str(issue.get("number", "?")))
        rows.append((current_score if current_score is not None else -1, issue, state, missing))

    print("score\tstate\tissue\ttitle\tmissing")
    for current_score, issue, state, missing in sorted(
        rows, reverse=True, key=lambda row: (row[0], str(row[1].get("number", "")))
    ):
        score_text = "" if current_score < 0 else f"{current_score:.2f}"
        title = str(issue.get("title", "")).replace("\t", " ").replace("\n", " ")
        print(f"{score_text}\t{state}\t{issue.get('number', '')}\t{title}\t{','.join(missing)}")
    conflicts = {file: numbers for file, numbers in sorted(file_owners.items()) if len(numbers) > 1}
    if conflicts:
        print("\n## Declared scope conflicts")
        for file, numbers in conflicts.items():
            print(f"- {file}: " + ", ".join(f"#{number}" for number in numbers))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
