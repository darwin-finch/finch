#!/usr/bin/env python3
"""Record and report provider-neutral boundary-load evidence.

The script reports evidence. It does not post comments, create tickets, change
readiness, or authorize architectural work.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import re
import subprocess
import sys
from collections import Counter
from datetime import datetime, timezone
from pathlib import Path, PurePosixPath

MANIFEST_SCHEMA = "software-factory/subsystem-manifest/v1"
EVIDENCE_SCHEMA = "software-factory/boundary-load-evidence/v1"
SURFACES = ("contract", "implementation")
SUBSYSTEM_ID = re.compile(r"^[a-z][a-z0-9]*(?:-[a-z0-9]+)*$")
CHECKPOINT_PREFIX = "boundary-load-audit/v1/"


class EvidenceError(ValueError):
    pass


def read_json(path: Path) -> object:
    try:
        return json.loads(path.read_text())
    except (OSError, json.JSONDecodeError) as error:
        raise EvidenceError(f"{path}: {error}") from error


def normalized_repo_path(value: object, field: str) -> str:
    if not isinstance(value, str) or not value:
        raise EvidenceError(f"{field} must be a non-empty string")
    if "\\" in value:
        raise EvidenceError(f"{field} must use POSIX separators: {value!r}")
    path = PurePosixPath(value)
    if (
        path.is_absolute()
        or not path.parts
        or path.as_posix() != value
        or any(part in ("", ".", "..") for part in path.parts)
    ):
        raise EvidenceError(f"{field} must be a normalized repository-relative path: {value!r}")
    return path.as_posix()


def roots_overlap(left: str, right: str) -> bool:
    return left == right or left.startswith(right + "/") or right.startswith(left + "/")


def load_manifest(path: Path) -> dict:
    payload = read_json(path)
    if not isinstance(payload, dict) or payload.get("schema") != MANIFEST_SCHEMA:
        raise EvidenceError(f"{path}: expected schema {MANIFEST_SCHEMA}")
    allowed_manifest = {"schema", "subsystems"}
    if set(payload) != allowed_manifest or not isinstance(payload.get("subsystems"), list):
        raise EvidenceError(f"{path}: manifest fields must be schema and subsystems")

    ids: set[str] = set()
    roots: list[tuple[str, str, str]] = []
    normalized_subsystems = []
    for index, subsystem in enumerate(payload["subsystems"]):
        where = f"subsystems[{index}]"
        required = {"id", "contract_roots", "implementation_roots", "dependencies"}
        if not isinstance(subsystem, dict) or set(subsystem) != required:
            raise EvidenceError(f"{where} must contain exactly {', '.join(sorted(required))}")
        subsystem_id = subsystem["id"]
        if not isinstance(subsystem_id, str) or not SUBSYSTEM_ID.fullmatch(subsystem_id) or subsystem_id in ids:
            raise EvidenceError(f"{where}.id must be a unique lowercase kebab-case identifier")
        ids.add(subsystem_id)
        normalized = {"id": subsystem_id}
        for field, surface in (("contract_roots", "contract"), ("implementation_roots", "implementation")):
            values = subsystem[field]
            if not isinstance(values, list):
                raise EvidenceError(f"{where}.{field} must be a list")
            current = [normalized_repo_path(value, f"{where}.{field}") for value in values]
            if len(set(current)) != len(current):
                raise EvidenceError(f"{where}.{field} contains duplicate roots")
            normalized[field] = current
            roots.extend((root, subsystem_id, surface) for root in current)
        dependencies = subsystem["dependencies"]
        if not isinstance(dependencies, list) or not all(isinstance(value, str) for value in dependencies):
            raise EvidenceError(f"{where}.dependencies must be a string list")
        if len(set(dependencies)) != len(dependencies):
            raise EvidenceError(f"{where}.dependencies contains duplicates")
        normalized["dependencies"] = dependencies
        normalized_subsystems.append(normalized)

    for subsystem in normalized_subsystems:
        unknown = sorted(set(subsystem["dependencies"]) - ids)
        if unknown:
            raise EvidenceError(f"{subsystem['id']} has unknown dependencies: {', '.join(unknown)}")
        if subsystem["id"] in subsystem["dependencies"]:
            raise EvidenceError(f"{subsystem['id']} cannot depend on itself")
    for index, (left, left_id, left_surface) in enumerate(roots):
        for right, right_id, right_surface in roots[index + 1 :]:
            if roots_overlap(left, right):
                raise EvidenceError(
                    "ambiguous path roots: "
                    f"{left_id}.{left_surface} {left!r} overlaps "
                    f"{right_id}.{right_surface} {right!r}"
                )
    return {"schema": MANIFEST_SCHEMA, "subsystems": normalized_subsystems}


def classify_paths(manifest: dict, paths: list[str]) -> list[dict]:
    roots = []
    for subsystem in manifest["subsystems"]:
        for surface in SURFACES:
            for root in subsystem[f"{surface}_roots"]:
                roots.append((root, subsystem["id"], surface))
    classified = []
    for path in sorted(set(paths)):
        matches = [item for item in roots if path == item[0] or path.startswith(item[0] + "/")]
        if matches:
            root, subsystem, surface = matches[0]
            classified.append({"path": path, "subsystem": subsystem, "surface": surface})
    return classified


def resolve_commit(value: str, cwd: Path) -> str:
    if not value or value.startswith("-") or any(character.isspace() for character in value):
        raise EvidenceError(f"invalid Git revision: {value!r}")
    result = subprocess.run(
        ["git", "rev-parse", "--verify", f"{value}^{{commit}}"],
        cwd=cwd,
        capture_output=True,
        text=True,
        check=False,
    )
    if result.returncode != 0:
        raise EvidenceError(f"cannot resolve Git revision {value!r}: {result.stderr.strip() or result.stdout.strip()}")
    commit = result.stdout.strip()
    if not re.fullmatch(r"[0-9a-f]{40,64}", commit):
        raise EvidenceError(f"Git returned an invalid commit id for {value!r}")
    return commit


def parse_timestamp(value: str, field: str) -> datetime:
    try:
        timestamp = datetime.fromisoformat(value.replace("Z", "+00:00"))
    except ValueError as error:
        raise EvidenceError(f"{field} must be RFC 3339") from error
    if "T" not in value or timestamp.utcoffset() is None:
        raise EvidenceError(f"{field} must be an RFC 3339 timestamp with an offset")
    return timestamp


def latest_checkpoint(repo: Path) -> dict | None:
    result = subprocess.run(
        [
            "git",
            "for-each-ref",
            "--sort=-creatordate",
            "--format=%(refname:short)%09%(objecttype)%09%(creatordate:iso-strict)",
            f"refs/tags/{CHECKPOINT_PREFIX}",
        ],
        cwd=repo,
        capture_output=True,
        text=True,
        check=False,
    )
    if result.returncode != 0:
        raise EvidenceError(f"cannot enumerate boundary-load checkpoints: {result.stderr.strip()}")
    for line in result.stdout.splitlines():
        tag, object_type, created_at = line.split("\t", 2)
        if object_type != "tag":
            continue
        reachable = subprocess.run(
            ["git", "merge-base", "--is-ancestor", f"{tag}^{{commit}}", "HEAD"],
            cwd=repo,
            capture_output=True,
            check=False,
        )
        if reachable.returncode == 0:
            return {
                "schema": "software-factory/boundary-load-checkpoint/v1",
                "tag": tag,
                "commit": resolve_commit(tag, repo),
                "created_at": parse_timestamp(created_at, "checkpoint creation time").isoformat(),
            }
        if reachable.returncode not in (0, 1):
            raise EvidenceError(f"cannot test checkpoint reachability: {tag}")
    return None


def git_modified_paths(base: str, candidate: str, cwd: Path) -> list[str]:
    result = subprocess.run(
        ["git", "diff", "--name-only", "-z", base, candidate, "--"],
        cwd=cwd,
        capture_output=True,
        check=False,
    )
    if result.returncode != 0:
        detail = result.stderr.decode(errors="replace").strip()
        raise EvidenceError(f"git diff failed: {detail or result.returncode}")
    return sorted(
        {
            normalized_repo_path(raw.decode(errors="strict"), "git modified path")
            for raw in result.stdout.split(b"\0")
            if raw
        }
    )


def read_path_lines(path: Path) -> list[str]:
    try:
        lines = path.read_text().splitlines()
    except OSError as error:
        raise EvidenceError(f"{path}: {error}") from error
    return sorted(
        {
            normalized_repo_path(line.strip(), f"{path} inspected path")
            for line in lines
            if line.strip()
        }
    )


def provenance(collector: str, version: str, harness: str, source: str) -> dict:
    values = (collector, version, harness, source)
    if not all(isinstance(value, str) and value for value in values):
        raise EvidenceError("observation provenance fields must be non-empty")
    return {
        "collector": collector,
        "collector_version": version,
        "harness": harness,
        "source": source,
    }


def observation(coverage: str, source: dict, paths: list[str] | None = None) -> dict:
    if coverage not in ("not_observed", "partial", "complete"):
        raise EvidenceError(f"invalid observation coverage: {coverage}")
    result = {"coverage": coverage, "provenance": source}
    if coverage == "not_observed":
        if paths is not None:
            raise EvidenceError("not_observed observations cannot contain paths")
    else:
        result["paths"] = sorted(set(paths or []))
    return result


def manifest_digest(path: Path) -> str:
    try:
        return hashlib.sha256(path.read_bytes()).hexdigest()
    except OSError as error:
        raise EvidenceError(f"{path}: {error}") from error


def create_record(args: argparse.Namespace) -> dict:
    repo = Path(args.repo)
    manifest_path = Path(args.manifest) if args.manifest else repo / ".agents/subsystems.json"
    manifest = load_manifest(manifest_path)
    by_id = {item["id"]: item for item in manifest["subsystems"]}
    if args.primary_subsystem not in by_id:
        raise EvidenceError(f"unknown primary subsystem: {args.primary_subsystem}")

    base = resolve_commit(args.base, repo)
    candidate = resolve_commit(args.candidate, repo)
    modified = git_modified_paths(base, candidate, repo)
    modified_observation = observation(
        "complete",
        provenance("git-diff", "1", "git", f"git diff --name-only {base} {candidate}"),
        modified,
    )
    inspection_provenance = provenance(
        args.inspection_collector,
        args.inspection_collector_version,
        args.inspection_harness,
        args.inspection_source
        or (f"path-list:{Path(args.inspected_paths).name}" if args.inspected_paths else "no inspected-path trace"),
    )
    if args.inspection_coverage == "not_observed":
        if args.inspected_paths:
            raise EvidenceError("--inspected-paths cannot be used with not_observed coverage")
        inspected = []
        inspected_observation = observation("not_observed", inspection_provenance)
    else:
        if not args.inspected_paths:
            raise EvidenceError("--inspected-paths is required for partial or complete coverage")
        inspected = read_path_lines(Path(args.inspected_paths))
        inspected_observation = observation(args.inspection_coverage, inspection_provenance, inspected)

    modified_classifications = classify_paths(manifest, modified)
    inspected_classifications = classify_paths(manifest, inspected)
    dependencies = set(by_id[args.primary_subsystem]["dependencies"])
    escapes = [
        item
        for item in inspected_classifications
        if item["subsystem"] in dependencies and item["surface"] == "implementation"
    ]
    recorded_at = args.recorded_at or datetime.now(timezone.utc).isoformat().replace("+00:00", "Z")
    return {
        "schema": EVIDENCE_SCHEMA,
        "claim": {
            "id": args.claim_id,
            "item": args.item,
            "base": base,
            "candidate": candidate,
        },
        "primary_subsystem": args.primary_subsystem,
        "scope_kind": args.scope_kind,
        "recorded_at": recorded_at,
        "manifest": {"schema": MANIFEST_SCHEMA, "sha256": manifest_digest(manifest_path)},
        "observations": {
            "modified_paths": modified_observation,
            "inspected_paths": inspected_observation,
        },
        "classifications": {
            "modified_paths": modified_classifications,
            "inspected_paths": inspected_classifications,
            "dependency_implementation_escapes": escapes,
        },
    }


def load_records(path: Path) -> list[dict]:
    try:
        text = path.read_text()
    except OSError as error:
        raise EvidenceError(f"{path}: {error}") from error
    try:
        payload = json.loads(text)
        records = payload if isinstance(payload, list) else [payload]
    except json.JSONDecodeError:
        try:
            records = [json.loads(line) for line in text.splitlines() if line.strip()]
        except json.JSONDecodeError as error:
            raise EvidenceError(f"{path}: {error}") from error
    for record in records:
        validate_record(record)
    return records


def validate_provenance(value: object, where: str) -> None:
    required = {"collector", "collector_version", "harness", "source"}
    if not isinstance(value, dict) or set(value) != required:
        raise EvidenceError(f"{where}.provenance has invalid fields")
    if not all(isinstance(value[field], str) and value[field] for field in required):
        raise EvidenceError(f"{where}.provenance fields must be non-empty strings")


def validate_observation(value: object, where: str) -> None:
    if not isinstance(value, dict) or value.get("coverage") not in ("not_observed", "partial", "complete"):
        raise EvidenceError(f"{where} has invalid coverage")
    coverage = value["coverage"]
    expected = {"coverage", "provenance"} if coverage == "not_observed" else {"coverage", "provenance", "paths"}
    if set(value) != expected:
        raise EvidenceError(f"{where} fields do not match {coverage} coverage")
    validate_provenance(value["provenance"], where)
    if coverage != "not_observed":
        paths = value["paths"]
        if not isinstance(paths, list):
            raise EvidenceError(f"{where}.paths must be a list")
        normalized = [normalized_repo_path(path, f"{where}.paths") for path in paths]
        if len(set(normalized)) != len(normalized):
            raise EvidenceError(f"{where}.paths contains duplicates")


def validate_classifications(value: object, where: str) -> None:
    if not isinstance(value, list):
        raise EvidenceError(f"{where} must be a list")
    for index, item in enumerate(value):
        if not isinstance(item, dict) or set(item) != {"path", "subsystem", "surface"}:
            raise EvidenceError(f"{where}[{index}] has invalid fields")
        normalized_repo_path(item["path"], f"{where}[{index}].path")
        if not isinstance(item["subsystem"], str) or not item["subsystem"]:
            raise EvidenceError(f"{where}[{index}].subsystem must be non-empty")
        if item["surface"] not in SURFACES:
            raise EvidenceError(f"{where}[{index}].surface is invalid")


def validate_record(record: object) -> None:
    required = {
        "schema",
        "claim",
        "primary_subsystem",
        "scope_kind",
        "recorded_at",
        "manifest",
        "observations",
        "classifications",
    }
    if not isinstance(record, dict) or set(record) != required or record.get("schema") != EVIDENCE_SCHEMA:
        raise EvidenceError(f"record must contain exactly the {EVIDENCE_SCHEMA} fields")
    claim = record["claim"]
    if not isinstance(claim, dict) or set(claim) != {"id", "item", "base", "candidate"}:
        raise EvidenceError("record.claim has invalid fields")
    if not all(isinstance(value, str) and value for value in claim.values()):
        raise EvidenceError("record.claim fields must be non-empty strings")
    if not isinstance(record["primary_subsystem"], str) or not record["primary_subsystem"]:
        raise EvidenceError("record.primary_subsystem must be non-empty")
    if record["scope_kind"] not in ("single_subsystem", "cross_subsystem"):
        raise EvidenceError("record.scope_kind is invalid")
    if not isinstance(record["recorded_at"], str):
        raise EvidenceError("record.recorded_at must be a string")
    parse_timestamp(record["recorded_at"], "record.recorded_at")
    manifest = record["manifest"]
    if (
        not isinstance(manifest, dict)
        or set(manifest) != {"schema", "sha256"}
        or manifest.get("schema") != MANIFEST_SCHEMA
        or not isinstance(manifest.get("sha256"), str)
        or not re.fullmatch(r"[0-9a-f]{64}", manifest["sha256"])
    ):
        raise EvidenceError("record.manifest is invalid")
    observations = record["observations"]
    if not isinstance(observations, dict) or set(observations) != {"modified_paths", "inspected_paths"}:
        raise EvidenceError("record.observations has invalid fields")
    for name, value in observations.items():
        validate_observation(value, f"record.observations.{name}")
    classifications = record["classifications"]
    expected_classifications = {"modified_paths", "inspected_paths", "dependency_implementation_escapes"}
    if not isinstance(classifications, dict) or set(classifications) != expected_classifications:
        raise EvidenceError("record.classifications has invalid fields")
    for name, value in classifications.items():
        validate_classifications(value, f"record.classifications.{name}")
    for name in ("modified_paths", "inspected_paths"):
        observed_paths = set(observations[name].get("paths", []))
        classified_paths = [item["path"] for item in classifications[name]]
        if len(set(classified_paths)) != len(classified_paths):
            raise EvidenceError(f"record.classifications.{name} classifies a path more than once")
        if not set(classified_paths).issubset(observed_paths):
            raise EvidenceError(f"record.classifications.{name} contains an unobserved path")
    if observations["inspected_paths"]["coverage"] == "not_observed" and (
        classifications["inspected_paths"] or classifications["dependency_implementation_escapes"]
    ):
        raise EvidenceError("not_observed inspection cannot have inspected classifications or escapes")
    inspected = {tuple(sorted(item.items())) for item in classifications["inspected_paths"]}
    if any(tuple(sorted(item.items())) not in inspected for item in classifications["dependency_implementation_escapes"]):
        raise EvidenceError("dependency implementation escapes must be inspected-path classifications")


def summarize(records: list[dict], window: int, since: str | None = None) -> dict:
    if window < 1:
        raise EvidenceError("window must be positive")
    since_timestamp = parse_timestamp(since, "since") if since else None
    grouped: dict[str, list[dict]] = {}
    coverage = Counter()
    cross_subsystem = Counter()
    new_records = Counter()
    new_eligible = Counter()
    claim_ids: set[str] = set()
    for record in records:
        claim_id = record["claim"]["id"]
        if claim_id in claim_ids:
            raise EvidenceError(f"duplicate claim evidence: {claim_id}")
        claim_ids.add(claim_id)
        subsystem = record.get("primary_subsystem")
        observation = record.get("observations", {}).get("inspected_paths", {})
        current_coverage = observation.get("coverage")
        if not isinstance(subsystem, str) or current_coverage not in ("not_observed", "partial", "complete"):
            raise EvidenceError("record has invalid primary_subsystem or inspected_paths coverage")
        coverage[(subsystem, current_coverage)] += 1
        is_new = since_timestamp is None or parse_timestamp(record["recorded_at"], "record.recorded_at") > since_timestamp
        if is_new:
            new_records[subsystem] += 1
        if record["scope_kind"] == "cross_subsystem":
            cross_subsystem[subsystem] += 1
        elif current_coverage == "complete":
            grouped.setdefault(subsystem, []).append(record)
            if is_new:
                new_eligible[subsystem] += 1

    subsystem_ids = sorted({key[0] for key in coverage})
    rows = []
    for subsystem in subsystem_ids:
        complete = sorted(
            grouped.get(subsystem, []),
            key=lambda item: parse_timestamp(item["recorded_at"], "record.recorded_at"),
        )
        row = {
            "subsystem": subsystem,
            "coverage": {
                state: coverage[(subsystem, state)]
                for state in ("complete", "partial", "not_observed")
            },
            "eligible_claims": len(complete),
            "excluded_cross_subsystem": cross_subsystem[subsystem],
            "new_records": new_records[subsystem],
            "new_eligible_claims": new_eligible[subsystem],
            "window": window,
            "baseline": None,
            "recent": None,
            "direction": "insufficient_history",
            "audit_candidate": False,
        }
        if len(complete) >= 2 * window:
            baseline_records = complete[-2 * window : -window]
            recent_records = complete[-window:]

            def window_stats(items: list[dict]) -> dict:
                escaped = sum(
                    bool(item.get("classifications", {}).get("dependency_implementation_escapes"))
                    for item in items
                )
                return {"claims": len(items), "escape_claims": escaped, "escape_rate": escaped / len(items)}

            baseline = window_stats(baseline_records)
            recent = window_stats(recent_records)
            if recent["escape_rate"] > baseline["escape_rate"]:
                direction = "increased"
            elif recent["escape_rate"] < baseline["escape_rate"]:
                direction = "decreased"
            else:
                direction = "level"
            row.update(
                baseline=baseline,
                recent=recent,
                direction=direction,
                audit_candidate=direction == "increased" and new_eligible[subsystem] > 0,
            )
        rows.append(row)
    return {"schema": "software-factory/boundary-load-report/v1", "since": since, "subsystems": rows}


def print_text_report(report: dict) -> None:
    print("subsystem\tcomplete\tpartial\tnot_observed\tcross_scope\tnew_eligible\tbaseline\trecent\tdirection\taudit_candidate")
    for row in report["subsystems"]:
        coverage = row["coverage"]
        baseline = row["baseline"]
        recent = row["recent"]
        baseline_text = "" if baseline is None else f"{baseline['escape_claims']}/{baseline['claims']}"
        recent_text = "" if recent is None else f"{recent['escape_claims']}/{recent['claims']}"
        print(
            f"{row['subsystem']}\t{coverage['complete']}\t{coverage['partial']}\t"
            f"{coverage['not_observed']}\t{row['excluded_cross_subsystem']}\t{row['new_eligible_claims']}\t"
            f"{baseline_text}\t{recent_text}\t"
            f"{row['direction']}\t{'yes' if row['audit_candidate'] else 'no'}"
        )


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(description=__doc__)
    subparsers = parser.add_subparsers(dest="command", required=True)

    validate = subparsers.add_parser("validate-manifest")
    validate.add_argument("manifest")

    validate_evidence = subparsers.add_parser("validate-evidence")
    validate_evidence.add_argument("records", help="JSON record, JSON list, or JSONL")

    checkpoint = subparsers.add_parser("checkpoint")
    checkpoint.add_argument("--repo", default=".")

    record = subparsers.add_parser("record")
    record.add_argument("--manifest", help="defaults to REPO/.agents/subsystems.json")
    record.add_argument("--repo", default=".")
    record.add_argument("--claim-id", required=True)
    record.add_argument("--item", required=True)
    record.add_argument("--primary-subsystem", required=True)
    record.add_argument("--scope-kind", choices=("single_subsystem", "cross_subsystem"), required=True)
    record.add_argument("--base", required=True)
    record.add_argument("--candidate", required=True)
    record.add_argument(
        "--inspection-coverage",
        choices=("not_observed", "partial", "complete"),
        default="not_observed",
    )
    record.add_argument("--inspected-paths", help="newline-delimited repository-relative paths")
    record.add_argument("--inspection-collector", default="none")
    record.add_argument("--inspection-collector-version", default="1")
    record.add_argument("--inspection-harness", default="unknown")
    record.add_argument("--inspection-source")
    record.add_argument("--recorded-at", help="RFC 3339 timestamp; defaults to now")

    report = subparsers.add_parser("report")
    report.add_argument("records", help="JSON record, JSON list, or JSONL")
    report.add_argument("--window", type=int, default=5)
    report.add_argument("--format", choices=("text", "json"), default="text")
    report.add_argument("--since", help="only flag candidates with eligible evidence after this RFC 3339 time")
    report.add_argument("--since-checkpoint", action="store_true")
    report.add_argument("--repo", default=".", help="repository used by --since-checkpoint")
    return parser


def main() -> int:
    args = build_parser().parse_args()
    try:
        if args.command == "validate-manifest":
            load_manifest(Path(args.manifest))
            print(f"ok {args.manifest}")
        elif args.command == "validate-evidence":
            records = load_records(Path(args.records))
            print(f"ok {args.records}: {len(records)} record(s)")
        elif args.command == "checkpoint":
            print(json.dumps({"checkpoint": latest_checkpoint(Path(args.repo))}, indent=2))
        elif args.command == "record":
            record = create_record(args)
            validate_record(record)
            print(json.dumps(record, indent=2))
        else:
            if args.since and args.since_checkpoint:
                raise EvidenceError("use only one of --since or --since-checkpoint")
            checkpoint_record = latest_checkpoint(Path(args.repo)) if args.since_checkpoint else None
            since = checkpoint_record["created_at"] if checkpoint_record else args.since
            report = summarize(load_records(Path(args.records)), args.window, since)
            report["checkpoint"] = checkpoint_record
            if args.format == "json":
                print(json.dumps(report, indent=2))
            else:
                print_text_report(report)
    except EvidenceError as error:
        print(f"boundary_load: {error}", file=sys.stderr)
        return 2
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
