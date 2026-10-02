#!/usr/bin/env python3
"""Print a dependency-aware GitHub issue work plan from blocked-by edges.

The output is topological: every issue appears after its open blockers. Issues
in one wave have no dependency ordering and are candidates for parallel workers.
File-overlap mutexes are scheduling advice, not GitHub edges. This script only
reads GitHub; it never claims, assigns, comments, or creates worktrees.

Usage:
  python3 .agents/scripts/ticket_poset.py
  python3 .agents/scripts/ticket_poset.py --repo owner/name
  python3 .agents/scripts/ticket_poset.py --milestone 'v1.2.0'
  python3 .agents/scripts/ticket_poset.py --workers 4 --format json
"""

from __future__ import annotations

import argparse
import json
import subprocess
import sys

QUERY = """
query($owner: String!, $name: String!, $cursor: String) {
  repository(owner: $owner, name: $name) {
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


def run(cmd: list[str]) -> subprocess.CompletedProcess[str]:
    return subprocess.run(cmd, capture_output=True, text=True, check=False)


def default_repo() -> str:
    result = run(["gh", "repo", "view", "--json", "nameWithOwner", "-q", ".nameWithOwner"])
    if result.returncode != 0 or not result.stdout.strip():
        sys.stderr.write(result.stderr or "ticket_poset: could not detect owner/name; pass --repo\n")
        raise SystemExit(result.returncode or 1)
    return result.stdout.strip()


def graphql(owner: str, name: str, cursor: str | None) -> dict:
    cmd = [
        "gh",
        "api",
        "graphql",
        "-f",
        f"query={QUERY}",
        "-F",
        f"owner={owner}",
        "-F",
        f"name={name}",
    ]
    if cursor:
        cmd.extend(["-F", f"cursor={cursor}"])
    result = run(cmd)
    if result.returncode != 0:
        sys.stderr.write(result.stderr or result.stdout)
        raise SystemExit(result.returncode)
    payload = json.loads(result.stdout)
    if payload.get("errors"):
        sys.stderr.write(json.dumps(payload["errors"], indent=2) + "\n")
        raise SystemExit(1)
    repository = (payload.get("data") or {}).get("repository")
    if repository is None:
        sys.stderr.write("ticket_poset: repository not found or not readable\n")
        raise SystemExit(1)
    return repository["issues"]


def load_open_issues(owner: str, name: str) -> list[dict]:
    issues = []
    cursor = None
    while True:
        page = graphql(owner, name, cursor)
        issues.extend(page["nodes"])
        if not page["pageInfo"]["hasNextPage"]:
            return issues
        cursor = page["pageInfo"]["endCursor"]


def parse_repo(value: str) -> tuple[str, str]:
    parts = value.split("/")
    if len(parts) != 2 or not all(parts):
        raise argparse.ArgumentTypeError("expected owner/name")
    return parts[0], parts[1]


class CycleError(Exception):
    """blocked_by contains a cycle among in-scope issues."""


def waves(blocked_by: dict[int, list[int]]) -> list[list[int]]:
    remaining = {number: set(blockers) for number, blockers in blocked_by.items()}
    planned: list[list[int]] = []
    while remaining:
        wave = sorted(number for number, blockers in remaining.items() if not blockers)
        if not wave:
            in_scope = set(remaining)
            # Out-of-scope blockers keep the dependent unplanned; they are not a cycle.
            external = [
                number
                for number, blockers in remaining.items()
                if not blockers & in_scope
            ]
            if not external:
                raise CycleError("ticket_poset: dependency cycle detected")
            for number in external:
                del remaining[number]
            continue
        planned.append(wave)
        for number in wave:
            del remaining[number]
        for blockers in remaining.values():
            blockers.difference_update(wave)
    return planned


def blocked_after(blocked_by: dict[int, list[int]], planned: list[list[int]]) -> list[int]:
    """Later waves, then in-scope leftovers waiting on out-of-scope blockers."""
    planned_ids = {number for wave in planned for number in wave}
    later = [number for wave in planned[1:] for number in wave]
    leftover = sorted(number for number in blocked_by if number not in planned_ids)
    return later + leftover


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--repo", help="GitHub owner/name (default: current gh repo)")
    parser.add_argument("--milestone", help="Restrict output to this milestone title")
    parser.add_argument("--workers", type=int, default=1, help="Maximum workers shown per wave")
    parser.add_argument("--format", choices=("text", "json"), default="text")
    args = parser.parse_args()
    if args.workers < 1:
        parser.error("--workers must be positive")

    owner, name = parse_repo(args.repo) if args.repo else parse_repo(default_repo())
    issues = load_open_issues(owner, name)
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

    try:
        planned = waves(blocked_by)
    except CycleError as error:
        print(error, file=sys.stderr)
        return 1

    plan = {
        "repository": f"{owner}/{name}",
        "open_in_scope": len(issues),
        "workers": args.workers,
        "scheduling": "replenish_on_completion",
        "waves": [
            {
                "wave": index,
                "parallel": numbers,
            }
            for index, numbers in enumerate(planned)
        ],
    }
    if args.format == "json":
        print(json.dumps(plan, indent=2))
        return 0

    ready = planned[0] if planned else []
    # Leftovers waiting on out-of-scope blockers are blocked, not a dispatch wave.
    blocked = blocked_after(blocked_by, planned)

    print(f"repository {owner}/{name}")
    print(f"open_in_scope {len(issues)}")
    print(f"ready {len(ready)}")
    print(f"blocked {len(blocked)}")
    print(f"waves {len(planned)}")
    print()
    print("## Dependency waves (parallel candidates)")
    for wave_index, numbers in enumerate(planned):
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
