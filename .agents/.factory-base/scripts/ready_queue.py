#!/usr/bin/env python3
"""Print READY issues in dependency order from poset waves and triage rows.

NEEDS_SPECIFICATION never appears. Wave-0 READY is the dispatchable set; later
waves stay blocked even when a wave-0 issue is omitted as unready. This script
does not claim, assign, comment, or merge.

Usage:
  python3 .agents/scripts/ready_queue.py --waves waves.json --triage issues.json
  python3 .agents/scripts/ready_queue.py --repo owner/name --workers 4 --format json
"""

from __future__ import annotations

import argparse
import json
import subprocess
import sys
from pathlib import Path

from ticket_triage import REQUIRED_MARKERS, score

SCRIPTS = Path(__file__).resolve().parent


def issue_state(issue: dict) -> str:
    body = str(issue.get("body") or "").lower()
    missing = [marker for marker in REQUIRED_MARKERS if marker not in body]
    return "READY" if not missing and score(issue) is not None else "NEEDS_SPECIFICATION"


def load_waves(payload: object) -> list[list[int]]:
    if isinstance(payload, dict) and "waves" in payload:
        return load_waves(payload["waves"])
    if not isinstance(payload, list):
        raise ValueError("waves must be poset JSON or a list of waves")
    waves: list[list[int]] = []
    for wave in payload:
        if isinstance(wave, dict) and "parallel" in wave:
            numbers = wave["parallel"]
        elif isinstance(wave, list):
            numbers = wave
        else:
            raise ValueError("each wave must be a list of issue numbers or a {parallel: [...]} object")
        waves.append([int(number) for number in numbers])
    return waves


def load_issues(payload: object) -> list[dict]:
    if not isinstance(payload, list) or not all(isinstance(item, dict) for item in payload):
        raise ValueError("triage must be a JSON list of issue objects")
    return payload


def ready_waves(waves: list[list[int]], issues: list[dict]) -> list[list[int]]:
    states = {int(issue["number"]): issue_state(issue) for issue in issues if "number" in issue}
    return [[number for number in wave if states.get(number) == "READY"] for wave in waves]


def dispatchable(filtered: list[list[int]]) -> list[int]:
    return list(filtered[0]) if filtered else []


def load_json_file(path: str) -> object:
    return json.loads(Path(path).read_text())


def run_json(cmd: list[str]) -> object:
    result = subprocess.run(cmd, capture_output=True, text=True, check=False)
    if result.returncode != 0:
        sys.stderr.write(result.stderr or result.stdout)
        raise SystemExit(result.returncode or 1)
    return json.loads(result.stdout)


def fetch_inputs(repo: str | None, workers: int) -> tuple[object, object]:
    poset = [
        sys.executable,
        str(SCRIPTS / "ticket_poset.py"),
        "--format",
        "json",
        "--workers",
        str(workers),
    ]
    export = [sys.executable, str(SCRIPTS / "issue_export.py")]
    if repo:
        poset.extend(["--repo", repo])
        export.extend(["--repo", repo])
    return run_json(poset), run_json(export)


def titles(issues: list[dict]) -> dict[int, str]:
    return {
        int(issue["number"]): str(issue.get("title") or "").replace("\t", " ").replace("\n", " ")
        for issue in issues
        if "number" in issue
    }


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--waves", help="poset JSON file (skip gh)")
    parser.add_argument("--triage", help="issue-export JSON file (skip gh)")
    parser.add_argument("--repo", help="GitHub owner/name (default: current gh repo)")
    parser.add_argument("--workers", type=int, default=1, help="passed to ticket_poset.py in live mode")
    parser.add_argument("--format", choices=("text", "json"), default="text")
    args = parser.parse_args()
    if args.workers < 1:
        parser.error("--workers must be positive")
    if (args.waves is None) != (args.triage is None):
        parser.error("--waves and --triage must be used together")

    try:
        if args.waves is not None:
            wave_payload = load_json_file(args.waves)
            issue_payload = load_json_file(args.triage)
        else:
            wave_payload, issue_payload = fetch_inputs(args.repo, args.workers)
        waves = load_waves(wave_payload)
        issues = load_issues(issue_payload)
    except (OSError, json.JSONDecodeError, ValueError) as error:
        print(f"ready_queue: {error}", file=sys.stderr)
        return 2

    filtered = ready_waves(waves, issues)
    current = dispatchable(filtered)
    plan = {
        "dispatchable": current,
        "ready": current,
        "waves": [{"wave": index, "parallel": numbers} for index, numbers in enumerate(filtered)],
    }
    if args.format == "json":
        print(json.dumps(plan, indent=2))
        return 0

    by_title = titles(issues)
    for number in current:
        title = by_title.get(number, "")
        print(f"#{number} {title}".rstrip())
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
