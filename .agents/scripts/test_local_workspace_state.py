#!/usr/bin/env python3
"""Validate representative local-workspace-state v1 records."""

from __future__ import annotations

import copy
import json
from pathlib import Path

from jsonschema import Draft202012Validator, FormatChecker


SCHEMA = Path(__file__).parents[1] / "schemas" / "local-workspace-state.v1.schema.json"
SHA = "a" * 40


def record() -> dict[str, object]:
    return {
        "schema": "software-factory/local-workspace-state/v1",
        "workspace_id": "95585f0f-b0b5-4ddd-b6b6-2fc861ce1994",
        "machine_id": "machine-1",
        "repository": "/repo",
        "worktree": "/repo/.worktrees/probe",
        "branch": None,
        "base": SHA,
        "item": None,
        "claim_id": None,
        "claim_url": None,
        "harness": "codex",
        "worker": "session-1",
        "coordinator": "coordinator-1",
        "state": "active",
        "created_at": "2026-10-01T10:00:00Z",
        "updated_at": "2026-10-01T10:00:00Z",
        "remote_recovery_ref": None,
        "checkpoint_push_authorized": False,
        "recovery_ref_delete_authorized": False,
        "recovery_ref_disposition_owner": "owner",
        "recovery_ref_removal_trigger": "workspace retired",
        "durable_commit": None,
        "cleanup_blocker": None,
        "cleanup_owner": None,
        "task": {"outcome": "probe", "scope": "docs", "gates": ["docs"]},
        "processes": [],
        "resources": [],
        "resume": {"last_gate": None, "dirty_paths": ["docs/readme.md"], "next_action": "inspect"},
    }


def main() -> int:
    schema = json.loads(SCHEMA.read_text())
    Draft202012Validator.check_schema(schema)
    validator = Draft202012Validator(schema, format_checker=FormatChecker())
    assert not list(validator.iter_errors(record()))

    mutations = (
        ("base", "b" * 41),
        ("durable_commit", "b" * 63),
        ("created_at", "2026-10-01 10:00:00Z"),
        ("remote_recovery_ref", "none"),
        ("repository", "relative/repo"),
        ("remote_recovery_ref", "bad ref with spaces"),
        ("remote_recovery_ref", "origin/issue-1"),
        ("workspace_id", "95585F0F-B0B5-4DDD-B6B6-2FC861CE1994"),
    )
    for field, value in mutations:
        candidate = copy.deepcopy(record())
        candidate[field] = value
        assert list(validator.iter_errors(candidate)), (field, value)

    for path in ("../outside", "dir/../../outside", "C:outside.txt", "\\outside.txt", "foo\n/../../outside"):
        candidate = copy.deepcopy(record())
        candidate["resume"]["dirty_paths"] = [path]  # type: ignore[index]
        assert list(validator.iter_errors(candidate)), path

    candidate = copy.deepcopy(record())
    candidate["checkpoint_push_authorized"] = True
    assert list(validator.iter_errors(candidate))
    candidate["remote_recovery_ref"] = "origin:refs/-checkpoint/x"
    assert not list(validator.iter_errors(candidate))
    candidate["recovery_ref_delete_authorized"] = True
    candidate["durable_commit"] = SHA
    assert not list(validator.iter_errors(candidate))
    candidate = copy.deepcopy(record())
    candidate["recovery_ref_delete_authorized"] = True
    assert list(validator.iter_errors(candidate))

    candidate = copy.deepcopy(record())
    candidate["state"] = "cleanup_blocked"
    assert list(validator.iter_errors(candidate))
    candidate["cleanup_blocker"] = "container still running"
    candidate["cleanup_owner"] = "owner"
    assert not list(validator.iter_errors(candidate))

    candidate = copy.deepcopy(record())
    candidate["repository"] = "/"
    assert list(validator.iter_errors(candidate))
    candidate["repository"] = "\\not-fully-qualified"
    assert list(validator.iter_errors(candidate))
    candidate["repository"] = "/repo/a/../b"
    assert list(validator.iter_errors(candidate))
    for path in ("/../outside", "/./outside"):
        candidate["repository"] = path
        assert list(validator.iter_errors(candidate)), path
    for path in (r"\\server\share\dir\\child", r"\\server\share\dir//child"):
        candidate["repository"] = path
        assert list(validator.iter_errors(candidate)), path
    print("ok")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
