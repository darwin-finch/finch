#!/usr/bin/env python3
"""Print a work-claim:v1 GitHub comment body. Does not post it.

Usage:
  python3 .agents/scripts/emit_claim.py --event claim --issue 1 \\
    --worker grok-implement-1 --actor @schancel --branch issue-1 \\
    --worktree machine-id/workspace-id --base SHA --scope 'bounded scope'
  python3 .agents/scripts/emit_claim.py --event recovery_needed --issue 1 \\
    --worker grok-implement-1 --claim-id UUID --observer coordinator-session \\
    --workspace-reachable false --durable-ref origin:refs/heads/issue-1 \\
    --durable-commit SHA --disposition-owner @maintainer
  python3 .agents/scripts/emit_claim.py --event complete --issue 1 \\
    --worker grok-implement-1 --claim-id UUID --reason 'merged as SHA'
"""

from __future__ import annotations

import argparse
import re
import subprocess
import sys
import uuid
from datetime import datetime, timezone


COMMIT_RE = re.compile(r"(?:[0-9a-f]{40}|[0-9a-f]{64})\Z")
RFC3339_RE = re.compile(
    r"[0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9]{2}:[0-9]{2}:[0-9]{2}"
    r"(?:\.[0-9]+)?(?:Z|[+-][0-9]{2}:[0-9]{2})\Z"
)
REMOTE_RE = re.compile(r"[A-Za-z0-9][A-Za-z0-9._-]*\Z")


def single_line(value: str, name: str) -> str:
    if not value.isprintable():
        raise SystemExit(f"emit_claim: --{name.replace('_', '-')} must be one printable line")
    if "<!--" in value or "-->" in value or "--!>" in value:
        raise SystemExit(f"emit_claim: --{name.replace('_', '-')} contains a comment marker")
    return value


def valid_uuid(value: str, name: str) -> str:
    single_line(value, name)
    try:
        parsed = uuid.UUID(value)
    except ValueError as error:
        raise SystemExit(f"emit_claim: --{name.replace('_', '-')} must be a UUID") from error
    if str(parsed) != value:
        raise SystemExit(f"emit_claim: --{name.replace('_', '-')} must be a canonical UUID")
    return value


def valid_timestamp(value: str, name: str = "timestamp") -> str:
    single_line(value, name)
    if not RFC3339_RE.fullmatch(value):
        raise SystemExit(f"emit_claim: --{name.replace('_', '-')} must be RFC 3339")
    try:
        parsed = datetime.fromisoformat(value.replace("Z", "+00:00"))
    except ValueError as error:
        raise SystemExit(f"emit_claim: --{name.replace('_', '-')} must be RFC 3339") from error
    if parsed.tzinfo is None:
        raise SystemExit(f"emit_claim: --{name.replace('_', '-')} must include a timezone")
    return value


def valid_commit(value: str, name: str, *, allow_none: bool = False) -> str:
    single_line(value, name)
    if allow_none and value == "none":
        return value
    if not COMMIT_RE.fullmatch(value):
        raise SystemExit(f"emit_claim: --{name.replace('_', '-')} must be a full commit or none")
    return value


def valid_ref(value: str, name: str, *, allow_none: bool = False) -> str:
    single_line(value, name)
    if allow_none and value == "none":
        return value
    result = subprocess.run(
        ["git", "check-ref-format", "--branch", value],
        capture_output=True,
        text=True,
        check=False,
    )
    if result.returncode != 0 or result.stdout.strip() != value:
        raise SystemExit(f"emit_claim: --{name.replace('_', '-')} must be a safe ref or none")
    return value


def valid_remote_ref_locator(value: str, name: str, *, allow_none: bool = False) -> str:
    single_line(value, name)
    if allow_none and value == "none":
        return value
    if not value.isascii():
        raise SystemExit(
            f"emit_claim: --{name.replace('_', '-')} must be an ASCII remote-name:full-ref or none"
        )
    if ":" not in value:
        raise SystemExit(
            f"emit_claim: --{name.replace('_', '-')} must be remote-name:full-ref or none"
        )
    remote, ref = value.split(":", 1)
    if not REMOTE_RE.fullmatch(remote) or not ref.startswith("refs/"):
        raise SystemExit(
            f"emit_claim: --{name.replace('_', '-')} must be remote-name:full-ref or none"
        )
    result = subprocess.run(
        ["git", "check-ref-format", ref], capture_output=True, text=True, check=False
    )
    if result.returncode != 0:
        raise SystemExit(
            f"emit_claim: --{name.replace('_', '-')} must be remote-name:full-ref or none"
        )
    return value


def validate_common(args: argparse.Namespace) -> None:
    for name in (
        "issue", "worker", "actor", "branch", "worktree", "scope", "reason", "replacement_claim",
        "authority_comment", "observer", "disposition_owner",
    ):
        value = getattr(args, name, None)
        if value:
            single_line(value, name)
    if args.timestamp:
        valid_timestamp(args.timestamp)


def timestamp() -> str:
    return datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")


def claim_block(args: argparse.Namespace) -> str:
    claim_id = args.claim_id or str(uuid.uuid4())
    missing = [
        name
        for name in ("worker", "actor", "branch", "worktree", "base", "scope")
        if not getattr(args, name)
    ]
    if missing:
        raise SystemExit(f"emit_claim: claim requires --{' --'.join(missing)}")
    valid_uuid(claim_id, "claim_id")
    valid_ref(args.branch, "branch")
    valid_commit(args.base, "base")
    summary = (
        f"`{args.worker}` ({args.actor}) is claiming implementation of #{args.issue} "
        f"for {args.scope}."
    )
    return (
        f"{summary}\n\n"
        f"<!-- work-claim:v1\n"
        f"event: claim\n"
        f"claim-id: {claim_id}\n"
        f"worker: {args.worker}\n"
        f"github-actor: {args.actor}\n"
        f"branch: {args.branch}\n"
        f"worktree: {args.worktree}\n"
        f"base: {args.base}\n"
        f"scope: {args.scope}\n"
        f"timestamp: {args.timestamp or timestamp()}\n"
        f"-->\n"
    )


def terminal_block(args: argparse.Namespace) -> str:
    if not args.worker or not args.claim_id:
        raise SystemExit("emit_claim: terminal events require --worker and --claim-id")
    valid_uuid(args.claim_id, "claim_id")
    if args.replacement_claim and args.replacement_claim != "none":
        valid_uuid(args.replacement_claim, "replacement_claim")
    reason = args.reason or args.event
    actor = args.actor or "none"
    summary = (
        f"`{args.worker}` ({actor}) is releasing claim `{args.claim_id}`: {reason}."
    )
    return (
        f"{summary}\n\n"
        f"<!-- work-claim:v1\n"
        f"event: {args.event}\n"
        f"claim-id: {args.claim_id}\n"
        f"worker: {args.worker}\n"
        f"timestamp: {args.timestamp or timestamp()}\n"
        f"replacement-claim: {args.replacement_claim or 'none'}\n"
        f"authority-comment: {args.authority_comment or 'none'}\n"
        f"-->\n"
    )


def recovery_block(args: argparse.Namespace) -> str:
    missing = [
        name
        for name in (
            "worker",
            "claim_id",
            "observer",
            "workspace_reachable",
            "durable_ref",
            "durable_commit",
            "disposition_owner",
        )
        if not getattr(args, name)
    ]
    if missing:
        raise SystemExit(f"emit_claim: recovery requires --{' --'.join(missing)}")
    valid_uuid(args.claim_id, "claim_id")
    valid_remote_ref_locator(args.durable_ref, "durable_ref", allow_none=True)
    valid_commit(args.durable_commit, "durable_commit", allow_none=True)
    reason = args.reason or "worker or machine unreachable"
    return (
        f"Claim `{args.claim_id}` needs recovery: {reason}. Ownership remains active.\n\n"
        f"<!-- work-claim-recovery:v1\n"
        f"claim-id: {args.claim_id}\n"
        f"worker: {args.worker}\n"
        f"observer: {args.observer}\n"
        f"observed-at: {args.timestamp or timestamp()}\n"
        f"workspace-reachable: {args.workspace_reachable}\n"
        f"durable-ref: {args.durable_ref}\n"
        f"durable-commit: {args.durable_commit}\n"
        f"disposition-owner: {args.disposition_owner}\n"
        f"-->\n"
    )


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--event", required=True, choices=("claim", "recovery_needed", "release", "complete", "supersede"))
    parser.add_argument("--issue", required=True)
    parser.add_argument("--worker")
    parser.add_argument("--actor")
    parser.add_argument("--branch")
    parser.add_argument("--worktree")
    parser.add_argument("--base")
    parser.add_argument("--scope")
    parser.add_argument("--claim-id")
    parser.add_argument("--reason")
    parser.add_argument("--replacement-claim")
    parser.add_argument("--authority-comment")
    parser.add_argument("--observer")
    parser.add_argument("--workspace-reachable", choices=("true", "false", "unknown"))
    parser.add_argument("--durable-ref")
    parser.add_argument("--durable-commit")
    parser.add_argument("--disposition-owner")
    parser.add_argument("--timestamp", help="UTC RFC 3339; default now")
    args = parser.parse_args()
    try:
        validate_common(args)
        if args.event == "claim":
            text = claim_block(args)
        elif args.event == "recovery_needed":
            text = recovery_block(args)
        else:
            text = terminal_block(args)
    except SystemExit as error:
        print(error, file=sys.stderr)
        return 2
    sys.stdout.write(text)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
