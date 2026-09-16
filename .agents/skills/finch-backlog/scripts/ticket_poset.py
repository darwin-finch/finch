#!/usr/bin/env python3
"""Print a dependency-aware GitHub issue work plan from blocked-by edges.

The output is topological: every issue appears after its open blockers. Issues
in one wave have no dependency ordering and are candidates for parallel workers.
File-overlap mutexes are scheduling advice, not GitHub edges. This script only
reads GitHub; it never claims, assigns, comments, or creates worktrees.

Usage:
  python3 .agents/skills/finch-backlog/scripts/ticket_poset.py
  python3 .agents/skills/finch-backlog/scripts/ticket_poset.py --milestone 'v0.7.31'
  python3 .agents/skills/finch-backlog/scripts/ticket_poset.py --workers 4 --format json
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
    parser.add_argument("--workers", type=int, default=1, help="Maximum workers shown per wave")
    parser.add_argument("--format", choices=("text", "json"), default="text")
    args = parser.parse_args()
    if args.workers < 1:
        parser.error("--workers must be positive")

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

    # Kahn's algorithm gives dependency waves rather than merely a ready/
    # blocked split. A dependent ticket cannot enter a wave until every open
    # blocker has entered an earlier wave.
    remaining = {number: set(blockers) for number, blockers in blocked_by.items()}
    waves: list[list[int]] = []
    while remaining:
        wave = sorted(number for number, blockers in remaining.items() if not blockers)
        if not wave:
            print("ticket_poset: dependency cycle detected", file=sys.stderr)
            return 1
        waves.append(wave)
        for number in wave:
            del remaining[number]
        for blockers in remaining.values():
            blockers.difference_update(wave)

    plan = {
        "open_in_scope": len(issues),
        "workers": args.workers,
        "scheduling": "replenish_on_completion",
        "waves": [
            {
                "wave": index,
                "parallel": numbers,
            }
            for index, numbers in enumerate(waves)
        ],
    }
    if args.format == "json":
        print(json.dumps(plan, indent=2))
        return 0

    ready = waves[0] if waves else []
    blocked = [number for wave in waves[1:] for number in wave]

    print(f"open_in_scope {len(issues)}")
    print(f"ready {len(ready)}")
    print(f"blocked {len(blocked)}")
    print(f"waves {len(waves)}")
    print()
    print("## Dependency waves (parallel candidates)")
    for wave_index, numbers in enumerate(waves):
        print(f"### Wave {wave_index} (dispatch up to {args.workers}; refill on completion)")
        print("- " + ", ".join(f"#{number} {by_number[number]['title']}" for number in numbers))

    print()
    print("## Dependency edges")
    for number in blocked:
        blockers = ", ".join(f"#{b}" for b in blocked_by[number])
        print(f"- #{number} after {blockers} — {by_number[number]['title']}")

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
