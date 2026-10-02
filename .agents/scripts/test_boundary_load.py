#!/usr/bin/env python3
"""Tests for boundary-load manifest, evidence, and historical reporting."""

from __future__ import annotations

import argparse
import json
import subprocess
import tempfile
from pathlib import Path

from boundary_load import EvidenceError, create_record, latest_checkpoint, load_manifest, summarize, validate_record


def write_manifest(path: Path, overlap: bool = False) -> None:
    implementation = ["src/rpc"] if overlap else ["src/rpc/private"]
    path.write_text(
        json.dumps(
            {
                "schema": "software-factory/subsystem-manifest/v1",
                "subsystems": [
                    {
                        "id": "transactions",
                        "contract_roots": ["src/transactions/api.py"],
                        "implementation_roots": ["src/transactions/private"],
                        "dependencies": ["rpc"],
                    },
                    {
                        "id": "rpc",
                        "contract_roots": ["src/rpc/api.py"],
                        "implementation_roots": implementation,
                        "dependencies": [],
                    },
                ],
            }
        )
    )


def git(repo: Path, *args: str) -> str:
    result = subprocess.run(["git", *args], cwd=repo, capture_output=True, text=True, check=True)
    return result.stdout.strip()


def record_args(repo: Path, manifest: Path, base: str, candidate: str, trace: Path | None, coverage: str) -> argparse.Namespace:
    return argparse.Namespace(
        manifest=str(manifest),
        repo=str(repo),
        claim_id=f"claim-{candidate[:7]}",
        item="#7",
        primary_subsystem="transactions",
        scope_kind="single_subsystem",
        base=base,
        candidate=candidate,
        inspection_coverage=coverage,
        inspected_paths=str(trace) if trace else None,
        inspection_collector="test-collector" if trace else "none",
        inspection_collector_version="1",
        inspection_harness="test",
        inspection_source=str(trace) if trace else None,
        recorded_at="2026-09-27T12:00:00Z",
    )


def test_manifest_rejects_ambiguous_roots(root: Path) -> None:
    manifest = root / "overlap.json"
    write_manifest(manifest, overlap=True)
    try:
        load_manifest(manifest)
    except EvidenceError as error:
        assert "ambiguous path roots" in str(error)
    else:
        raise AssertionError("overlapping roots should fail")


def test_manifest_rejects_non_normalized_roots(root: Path) -> None:
    for invalid_root in (".", "src//rpc", "src/./rpc", "src/rpc/"):
        manifest = root / "invalid-root.json"
        write_manifest(manifest)
        payload = json.loads(manifest.read_text())
        payload["subsystems"][0]["implementation_roots"] = [invalid_root]
        manifest.write_text(json.dumps(payload))
        try:
            load_manifest(manifest)
        except EvidenceError as error:
            assert "normalized repository-relative path" in str(error)
        else:
            raise AssertionError(f"non-normalized root should fail: {invalid_root}")


def test_record_and_report(root: Path) -> None:
    repo = root / "repo"
    repo.mkdir()
    git(repo, "init", "-q")
    git(repo, "config", "user.email", "test@example.com")
    git(repo, "config", "user.name", "Test")
    for relative, contents in (
        ("src/transactions/private/worker.py", "before\n"),
        ("src/rpc/api.py", "api\n"),
        ("src/rpc/private/provider.py", "private\n"),
    ):
        path = repo / relative
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(contents)
    git(repo, "add", ".")
    git(repo, "commit", "-qm", "base")
    base = git(repo, "rev-parse", "HEAD")
    (repo / "src/transactions/private/worker.py").write_text("after\n")
    git(repo, "commit", "-qam", "candidate")
    candidate = git(repo, "rev-parse", "HEAD")

    manifest = root / "subsystems.json"
    write_manifest(manifest)
    trace = root / "trace.txt"
    trace.write_text("src/rpc/api.py\nsrc/rpc/private/provider.py\n")
    complete = create_record(record_args(repo, manifest, base, candidate, trace, "complete"))
    assert complete["claim"]["base"] == base
    assert complete["claim"]["candidate"] == candidate
    assert complete["observations"]["modified_paths"]["coverage"] == "complete"
    assert complete["classifications"]["modified_paths"] == [
        {
            "path": "src/transactions/private/worker.py",
            "subsystem": "transactions",
            "surface": "implementation",
        }
    ]
    assert complete["classifications"]["dependency_implementation_escapes"] == [
        {"path": "src/rpc/private/provider.py", "subsystem": "rpc", "surface": "implementation"}
    ]

    unobserved = create_record(record_args(repo, manifest, base, candidate, None, "not_observed"))
    assert "paths" not in unobserved["observations"]["inspected_paths"]

    empty_trace = root / "empty-trace.txt"
    empty_trace.write_text("")
    observed_none = create_record(record_args(repo, manifest, base, candidate, empty_trace, "complete"))
    assert observed_none["observations"]["inspected_paths"]["paths"] == []
    assert observed_none["classifications"]["dependency_implementation_escapes"] == []

    records = []
    for index in range(5):
        record = json.loads(json.dumps(complete))
        record["claim"]["id"] = f"baseline-{index}"
        record["recorded_at"] = f"2026-09-{index + 1:02d}T12:00:00Z"
        record["classifications"]["dependency_implementation_escapes"] = []
        records.append(record)
    partial = json.loads(json.dumps(complete))
    partial["claim"]["id"] = "partial"
    partial["recorded_at"] = "2026-09-06T12:00:00Z"
    partial["observations"]["inspected_paths"]["coverage"] = "partial"
    records.append(partial)
    cross_scope = json.loads(json.dumps(complete))
    cross_scope["claim"]["id"] = "cross-scope"
    cross_scope["recorded_at"] = "2026-09-07T12:00:00Z"
    cross_scope["scope_kind"] = "cross_subsystem"
    records.append(cross_scope)
    for index in range(5):
        record = json.loads(json.dumps(complete))
        record["claim"]["id"] = f"recent-{index}"
        record["recorded_at"] = f"2026-09-{index + 10:02d}T12:00:00Z"
        if index >= 2:
            record["classifications"]["dependency_implementation_escapes"] = []
        records.append(record)
    report = summarize(records, 5)["subsystems"][0]
    assert report["coverage"] == {"complete": 11, "partial": 1, "not_observed": 0}
    assert report["eligible_claims"] == 10
    assert report["excluded_cross_subsystem"] == 1
    assert report["baseline"]["escape_claims"] == 0
    assert report["recent"]["escape_claims"] == 2
    assert report["direction"] == "increased"
    assert report["audit_candidate"] is True
    assert report["new_eligible_claims"] == 10

    after_all_records = summarize(records, 5, "2026-10-01T00:00:00Z")["subsystems"][0]
    assert after_all_records["direction"] == "increased"
    assert after_all_records["new_eligible_claims"] == 0
    assert after_all_records["audit_candidate"] is False

    offset_records = []
    for claim_id, recorded_at, escaped in (
        ("first", "2025-12-31T00:00:00Z", False),
        ("offset-earlier", "2026-01-01T01:00:00+02:00", True),
        ("utc-later", "2026-01-01T00:30:00Z", False),
        ("last", "2026-01-02T00:00:00Z", True),
    ):
        record = json.loads(json.dumps(complete))
        record["claim"]["id"] = claim_id
        record["recorded_at"] = recorded_at
        if not escaped:
            record["classifications"]["dependency_implementation_escapes"] = []
        offset_records.append(record)
    offset_report = summarize(offset_records, 2)["subsystems"][0]
    assert offset_report["baseline"]["escape_claims"] == 1
    assert offset_report["recent"]["escape_claims"] == 1
    assert offset_report["direction"] == "level"

    invalid_timestamp = json.loads(json.dumps(complete))
    invalid_timestamp["recorded_at"] = "2026-09-27T12:00:00"
    try:
        validate_record(invalid_timestamp)
    except EvidenceError as error:
        assert "with an offset" in str(error)
    else:
        raise AssertionError("timezone-less evidence should fail")

    unobserved_classification = json.loads(json.dumps(complete))
    unobserved_classification["observations"]["inspected_paths"]["paths"] = ["src/rpc/api.py"]
    try:
        validate_record(unobserved_classification)
    except EvidenceError as error:
        assert "contains an unobserved path" in str(error)
    else:
        raise AssertionError("classifications must refer to observed paths")

    try:
        summarize([complete, complete], 1)
    except EvidenceError as error:
        assert "duplicate claim evidence" in str(error)
    else:
        raise AssertionError("duplicate claim evidence should fail")

    unsafe = record_args(repo, manifest, "--output=elsewhere", candidate, None, "not_observed")
    try:
        create_record(unsafe)
    except EvidenceError as error:
        assert "invalid Git revision" in str(error)
    else:
        raise AssertionError("option-shaped Git revision should fail")

    git(repo, "tag", "-a", "boundary-load-audit/v1/20260927T120000Z", "-m", "completed pass")
    git(repo, "tag", "boundary-load-audit/v1/lightweight")
    checkpoint = latest_checkpoint(repo)
    assert checkpoint is not None
    assert checkpoint["tag"] == "boundary-load-audit/v1/20260927T120000Z"
    assert checkpoint["commit"] == candidate


def main() -> int:
    with tempfile.TemporaryDirectory() as directory:
        root = Path(directory)
        test_manifest_rejects_ambiguous_roots(root)
        test_manifest_rejects_non_normalized_roots(root)
        test_record_and_report(root)
    print("ok")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
