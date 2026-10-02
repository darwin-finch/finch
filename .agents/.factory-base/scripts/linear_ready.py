#!/usr/bin/env python3
"""READY Linear issues in dependency order from a query JSON dump.

Does not call `linear`. Feed it the output of:
  linear issue query --team TEAM --json --limit 0

Filters to repository: owner/name in the description. NEEDS_SPECIFICATION
(no acceptance/owner/repo line, or no scores) is omitted.
Relations type "blocks" / incoming inverse "blocks" are edges.

Usage:
  python3 .agents/scripts/linear_ready.py --repo owner/name issues.json
  python3 .agents/scripts/linear_ready.py --repo owner/name --format json < issues.json
"""

from __future__ import annotations

import argparse
import json
import re
import sys
from pathlib import Path

from ticket_poset import CycleError, waves
from ticket_triage import REQUIRED_MARKERS, score

REPO_LINE = re.compile(r"^repository:\s*(\S+)\s*$", re.M)


def flatten(payload: object) -> list[dict]:
    if isinstance(payload, list):
        return [item for item in payload if isinstance(item, dict)]
    if isinstance(payload, dict):
        if "nodes" in payload:
            return flatten(payload["nodes"])
        if "issues" in payload:
            return flatten(payload["issues"])
        if "identifier" in payload or "id" in payload:
            return [payload]
    raise ValueError("expected a list of issues or a {nodes: [...]} object")


def body_of(issue: dict) -> str:
    for key in ("description", "body", "content"):
        value = issue.get(key)
        if isinstance(value, str):
            return value
    return ""


def identifier_of(issue: dict) -> str | None:
    for key in ("identifier", "id"):
        value = issue.get(key)
        if isinstance(value, str) and value.strip():
            return value.strip()
    return None


def related_identifier(node: dict) -> str | None:
    if not isinstance(node, dict):
        return None
    if isinstance(node.get("identifier"), str):
        return node["identifier"]
    for nested in ("relatedIssue", "issue"):
        inner = node.get(nested)
        if isinstance(inner, dict) and isinstance(inner.get("identifier"), str):
            return inner["identifier"]
    return None


def relation_edges(issue: dict) -> tuple[list[str], list[str]]:
    """Return (blocks_ids, blocked_by_ids) from outgoing + inverse relations."""
    blocks: list[str] = []
    blocked_by: list[str] = []
    outgoing = issue.get("relations")
    if isinstance(outgoing, dict):
        outgoing = outgoing.get("nodes", [])
    if isinstance(outgoing, list):
        for node in outgoing:
            if not isinstance(node, dict):
                continue
            other = related_identifier(node)
            rel = str(node.get("type") or "").lower()
            if not other:
                continue
            if rel == "blocks":
                blocks.append(other)
            elif rel in ("blocked-by", "blockedby"):
                blocked_by.append(other)
    incoming = issue.get("inverseRelations")
    if isinstance(incoming, dict):
        incoming = incoming.get("nodes", [])
    if isinstance(incoming, list):
        for node in incoming:
            if not isinstance(node, dict):
                continue
            other = related_identifier(node)
            rel = str(node.get("type") or "").lower()
            if not other:
                continue
            # Incoming blocks: the other issue blocks this one.
            if rel == "blocks":
                blocked_by.append(other)
    return blocks, blocked_by


def is_ready(issue: dict, repo: str) -> bool:
    body = body_of(issue)
    lowered = body.lower()
    if any(marker not in lowered for marker in REQUIRED_MARKERS):
        return False
    match = REPO_LINE.search(body)
    if not match or match.group(1) != repo:
        return False
    synthetic = {"body": body}
    for axis in ("value", "cost", "certainty", "unblocking"):
        found = re.search(rf"^{axis}:\s*(\d)\s*$", body, re.M)
        if found:
            synthetic[axis] = int(found.group(1))
    return score(synthetic) is not None


def blocked_by_map(issues: list[dict], repo: str) -> dict[str, list[str]]:
    ready = [issue for issue in issues if identifier_of(issue) and is_ready(issue, repo)]
    ids = {identifier_of(issue) for issue in ready}
    mapping: dict[str, list[str]] = {ident: [] for ident in ids if ident}
    for issue in ready:
        ident = identifier_of(issue)
        if not ident:
            continue
        _blocks, blocked_by = relation_edges(issue)
        mapping[ident] = [other for other in blocked_by if other in ids]
    # also apply outgoing blocks as the inverse
    for issue in ready:
        ident = identifier_of(issue)
        if not ident:
            continue
        blocks, _ = relation_edges(issue)
        for other in blocks:
            if other in mapping and ident not in mapping[other]:
                mapping[other].append(ident)
    return mapping


def string_waves(blocked: dict[str, list[str]]) -> list[list[str]]:
    keys = sorted(blocked)
    index = {key: i for i, key in enumerate(keys)}
    numeric = {index[key]: [index[b] for b in blocked[key] if b in index] for key in keys}
    try:
        numbered = waves(numeric)
    except CycleError:
        raise
    return [[keys[i] for i in wave] for wave in numbered]


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--repo", required=True, help="owner/name that must appear as repository: in the description")
    parser.add_argument("json_file", nargs="?", help="linear issue query --json dump; stdin if omitted")
    parser.add_argument("--format", choices=("text", "json"), default="text")
    args = parser.parse_args()
    raw = Path(args.json_file).read_text() if args.json_file else sys.stdin.read()
    try:
        issues = flatten(json.loads(raw))
    except (json.JSONDecodeError, ValueError) as error:
        print(f"linear_ready: {error}", file=sys.stderr)
        return 2
    try:
        planned = string_waves(blocked_by_map(issues, args.repo))
    except CycleError:
        print("linear_ready: dependency cycle", file=sys.stderr)
        return 1
    dispatchable = planned[0] if planned else []
    if args.format == "json":
        print(json.dumps({"repository": args.repo, "waves": planned, "dispatchable": dispatchable}, indent=2))
        return 0
    print(f"repository {args.repo}")
    print(f"ready {len(dispatchable)}")
    print("dispatchable " + (" ".join(dispatchable) if dispatchable else "(none)"))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
