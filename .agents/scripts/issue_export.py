#!/usr/bin/env python3
"""Export open GitHub issues as ticket_triage.py JSON.

Parses value/cost/certainty/unblocking and a files: line from the body.
Does not claim, comment, or change issues.

Usage:
  python3 .agents/scripts/issue_export.py --repo owner/name
"""

from __future__ import annotations

import argparse
import json
import re
import subprocess
import sys

AXES = ("value", "cost", "certainty", "unblocking")


def parse_body(body: str) -> dict:
    row: dict = {"files": []}
    match = re.search(r"^files:\s*(.+)$", body, re.M)
    if match:
        row["files"] = match.group(1).split()
    for axis in AXES:
        match = re.search(rf"^{axis}:\s*(\d)\s*$", body, re.M)
        if match:
            row[axis] = int(match.group(1))
    return row


def from_gh_issues(issues: list[dict]) -> list[dict]:
    out = []
    for issue in issues:
        row = {
            "number": issue["number"],
            "title": issue.get("title", ""),
            "body": issue.get("body") or "",
        }
        row.update(parse_body(row["body"]))
        out.append(row)
    return out


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--repo", help="owner/name (default: current gh repo)")
    args = parser.parse_args()
    cmd = ["gh", "issue", "list", "--state", "open", "--limit", "100", "--json", "number,title,body"]
    if args.repo:
        cmd.extend(["--repo", args.repo])
    result = subprocess.run(cmd, capture_output=True, text=True, check=False)
    if result.returncode != 0:
        sys.stderr.write(result.stderr or result.stdout)
        return result.returncode or 1
    issues = json.loads(result.stdout)
    json.dump(from_gh_issues(issues), sys.stdout, indent=2)
    sys.stdout.write("\n")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
