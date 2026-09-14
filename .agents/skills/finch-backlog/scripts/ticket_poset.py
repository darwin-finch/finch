#!/usr/bin/env python3
"""Print the GitHub issue poset from native blocked-by edges.

Ready work is open issues with zero open blockers. Serial chains are the
blocked-by DAG. File-overlap mutexes are scheduling advice, not GitHub edges;
see the grooming issue that introduced this script.

Usage:
  python3 .agents/skills/finch-backlog/scripts/ticket_poset.py
  python3 .agents/skills/finch-backlog/scripts/ticket_poset.py --milestone 'v0.7.31'
"""

from __future__ import annotations

import argparse
import json
import subprocess
import sys
QUERY = """
query($cursor: String) {
  repository(owner: "darwin-finch", name: "finch") {
    issues(first: 100, states: OPEN, after: $cursor) {
      pageInfo { hasNextPage endCursor }
      nodes {
        number
        title
        milestone { title }
        blockedBy(first: 30) {
          nodes { number state title }
        }
        blocking(first: 30) {
          nodes { number state }
        }
      }
    }
  }
}
"""


def graphql(cursor: str | None) -> dict:
    cmd = ["gh", "api", "graphql", "-f", f"query={QUERY}"]
    if cursor:
        cmd.extend(["-F", f"cursor={cursor}"])
    result = subprocess.run(cmd, capture_output=True, text=True, check=False)
    if result.returncode != 0:
        sys.stderr.write(result.stderr or result.stdout)
        raise SystemExit(result.returncode)
    payload = json.loads(result.stdout)
    if payload.get("errors"):
        sys.stderr.write(json.dumps(payload["errors"], indent=2) + "\n")
        raise SystemExit(1)
    return payload["data"]["repository"]["issues"]


def load_open_issues() -> list[dict]:
    issues = []
    cursor = None
    while True:
        page = graphql(cursor)
        issues.extend(page["nodes"])
        if not page["pageInfo"]["hasNextPage"]:
            return issues
        cursor = page["pageInfo"]["endCursor"]


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--milestone", help="Restrict output to this milestone title")
    args = parser.parse_args()

    issues = load_open_issues()
    if args.milestone:
        issues = [
            issue
            for issue in issues
            if (issue.get("milestone") or {}).get("title") == args.milestone
        ]

    by_number = {issue["number"]: issue for issue in issues}
    blocked_by: dict[int, list[int]] = {}
    for issue in issues:
        open_blockers = [
            node["number"]
            for node in issue["blockedBy"]["nodes"]
            if node["state"] == "OPEN"
        ]
        blocked_by[issue["number"]] = sorted(open_blockers)

    ready = sorted(
        number for number, blockers in blocked_by.items() if not blockers
    )
    blocked = sorted(
        number for number, blockers in blocked_by.items() if blockers
    )

    print(f"open_in_scope {len(issues)}")
    print(f"ready {len(ready)}")
    print(f"blocked {len(blocked)}")
    print()
    print("## Ready (no open blockers)")
    for number in ready:
        title = by_number[number]["title"]
        print(f"- #{number} {title}")

    print()
    print("## Blocked")
    for number in blocked:
        blockers = ", ".join(f"#{b}" for b in blocked_by[number])
        print(f"- #{number} blocked by {blockers} — {by_number[number]['title']}")

    print()
    print("## Mermaid")
    print("```mermaid")
    print("flowchart TD")
    edges = []
    for number, blockers in blocked_by.items():
        for blocker in blockers:
            edges.append((blocker, number))
    if not edges:
        print("  empty[no blocked-by edges]")
    for blocker, blocked_n in sorted(edges):
        print(f"  n{blocker}[#{blocker}] --> n{blocked_n}[#{blocked_n}]")
    print("```")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
