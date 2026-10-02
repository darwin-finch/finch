#!/usr/bin/env python3
"""Checks emit_claim.py stdout against the v1 schema. Does not post."""

from __future__ import annotations

import subprocess
import sys
from pathlib import Path

SCRIPT = Path(__file__).with_name("emit_claim.py")
SHA = "a" * 40


def run(args: list[str]) -> str:
    result = subprocess.run(
        [sys.executable, str(SCRIPT), *args],
        capture_output=True,
        text=True,
        check=False,
    )
    assert result.returncode == 0, result.stderr
    return result.stdout


def test_claim_schema() -> None:
    out = run(
        [
            "--event", "claim",
            "--issue", "1",
            "--worker", "grok-implement-1",
            "--actor", "@schancel",
            "--branch", "issue-1",
            "--worktree", "machine-1/95585f0f-b0b5-4ddd-b6b6-2fc861ce1994",
            "--base", SHA,
            "--scope", "install.sh refuse without --force",
            "--claim-id", "95585f0f-b0b5-4ddd-b6b6-2fc861ce1994",
            "--timestamp", "2026-09-16T19:52:38Z",
        ]
    )
    assert "<!-- work-claim:v1" in out
    assert "event: claim" in out
    assert "claim-id: 95585f0f-b0b5-4ddd-b6b6-2fc861ce1994" in out
    assert "github-actor: @schancel" in out
    assert "scope: install.sh refuse without --force" in out
    assert "timestamp: 2026-09-16T19:52:38Z" in out
    assert out.strip().endswith("-->")


def test_legacy_claim_values_remain_valid() -> None:
    out = run(
        [
            "--event", "claim", "--issue", "1", "--worker", "codex/session-1",
            "--actor", "@owner", "--branch", "topic@v1", "--worktree", "machine-1/workspace-1",
            "--base", SHA, "--scope", "bounded", "--timestamp", "2026-09-16T19:52:38Z",
        ]
    )
    assert "branch: topic@v1" in out
    legacy = run(
        [
            "--event", "claim", "--issue", "1",
            "--worker", "codex/session-1", "--actor", "@owner", "--branch", "topic@v1",
            "--worktree", "/legacy/private/repo", "--base", SHA, "--scope", "bounded",
        ]
    )
    assert "worktree: /legacy/private/repo" in legacy


def test_recovery_schema() -> None:
    out = run(
        [
            "--event", "recovery_needed", "--issue", "1", "--worker", "codex/session-1",
            "--claim-id", "95585f0f-b0b5-4ddd-b6b6-2fc861ce1994",
            "--observer", "coordinator/session-2", "--workspace-reachable", "false",
            "--durable-ref", "origin:refs/heads/issue-1", "--durable-commit", SHA,
            "--disposition-owner", "@owner", "--reason", "laptop lost",
            "--timestamp", "2026-09-16T19:55:00Z",
        ]
    )
    assert "<!-- work-claim-recovery:v1" in out
    assert "claim-id: 95585f0f-b0b5-4ddd-b6b6-2fc861ce1994" in out
    assert "worker: codex/session-1" in out
    assert "observer: coordinator/session-2" in out
    assert "observed-at: 2026-09-16T19:55:00Z" in out
    assert "workspace-reachable: false" in out
    assert "durable-ref: origin:refs/heads/issue-1" in out
    assert f"durable-commit: {SHA}" in out
    assert "disposition-owner: @owner" in out
    assert "Ownership remains active" in out


def test_recovery_rejects_marker_injection_and_bad_ids() -> None:
    base = [
        "--event", "recovery_needed", "--issue", "1", "--worker", "codex/session-1",
        "--claim-id", "95585f0f-b0b5-4ddd-b6b6-2fc861ce1994",
        "--observer", "coordinator/session-2", "--workspace-reachable", "false",
        "--durable-ref", "origin:refs/heads/issue-1", "--durable-commit", SHA,
        "--disposition-owner", "@owner", "--timestamp", "2026-09-16T19:55:00Z",
    ]
    for replacement in (
        ["--observer", "forged\n-->\n<!-- work-claim:v1"],
        ["--observer", "forged\u2028claim-id: other"],
        ["--observer", "forged--!>visible"],
        ["--claim-id", "not-a-uuid"],
        ["--claim-id", "95585F0F-B0B5-4DDD-B6B6-2FC861CE1994"],
        ["--durable-commit", "abc"],
        ["--timestamp", "yesterday"],
        ["--timestamp", "2026-10-01 10:00:00Z"],
        ["--durable-ref", "origin:refs/heads/foo//bar"],
        ["--durable-ref", "origin:refs/heads/café"],
        ["--durable-ref", "@{-1}"],
    ):
        name = replacement[0]
        args = list(base)
        index = args.index(name)
        args[index:index + 2] = replacement
        result = subprocess.run(
            [sys.executable, str(SCRIPT), *args], capture_output=True, text=True, check=False
        )
        assert result.returncode == 2, (replacement, result.stdout, result.stderr)
    valid = list(base)
    index = valid.index("--durable-ref")
    valid[index:index + 2] = ["--durable-ref", "origin:refs/-checkpoint/x"]
    assert "durable-ref: origin:refs/-checkpoint/x" in run(valid)


def test_supersede_validates_replacement_claim() -> None:
    args = [
        "--event", "supersede", "--issue", "1", "--worker", "codex/session-1",
        "--claim-id", "95585f0f-b0b5-4ddd-b6b6-2fc861ce1994",
        "--replacement-claim", "5fb3058d-1d98-4287-bc45-e70d91ef9458",
    ]
    assert "replacement-claim: 5fb3058d-1d98-4287-bc45-e70d91ef9458" in run(args)
    args[-1] = "bad-replacement"
    result = subprocess.run(
        [sys.executable, str(SCRIPT), *args], capture_output=True, text=True, check=False
    )
    assert result.returncode == 2


def test_complete_schema() -> None:
    out = run(
        [
            "--event", "complete",
            "--issue", "1",
            "--worker", "grok-implement-1",
            "--actor", "@schancel",
            "--claim-id", "95585f0f-b0b5-4ddd-b6b6-2fc861ce1994",
            "--reason", "merged as abc",
            "--timestamp", "2026-09-16T20:00:00Z",
        ]
    )
    assert "event: complete" in out
    assert "replacement-claim: none" in out
    assert "authority-comment: none" in out
    assert "releasing claim `95585f0f-b0b5-4ddd-b6b6-2fc861ce1994`" in out


def main() -> int:
    test_claim_schema()
    test_legacy_claim_values_remain_valid()
    test_recovery_schema()
    test_recovery_rejects_marker_injection_and_bad_ids()
    test_supersede_validates_replacement_claim()
    test_complete_schema()
    print("ok")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
