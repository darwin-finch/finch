#!/usr/bin/env python3
"""Semantically expand pull-request workflow fan-out for representative changes."""

from __future__ import annotations

import argparse
import itertools
import re
import sys
from dataclasses import dataclass
from pathlib import Path
from typing import Any

try:
    import yaml
except ImportError as error:  # pragma: no cover - exercised by the workflow bootstrap
    raise SystemExit(
        "workflow fan-out: PyYAML is required (python3 -m pip install PyYAML==6.0.3)"
    ) from error


ROOT = Path(__file__).resolve().parent.parent
WORKFLOWS = Path(".github/workflows")
FALSE_EXPRESSIONS = {"false", "${{ false }}", "${{false}}"}


@dataclass(frozen=True)
class Fixture:
    name: str
    paths: tuple[str, ...]
    expected: frozenset[str]


def job(workflow: str, name: str, **matrix: str) -> str:
    suffix = ""
    if matrix:
        suffix = "[" + ",".join(f"{key}={matrix[key]}" for key in sorted(matrix)) + "]"
    return f"{workflow}::{name}{suffix}"


CI = frozenset(
    {
        job("ci.yml", "toolchain-contract"),
        job("ci.yml", "windows-format-contract"),
        job("ci.yml", "runtime-authority"),
        job("ci.yml", "security"),
        *(job("ci.yml", "test", os=os, feature_name=feature) for os in ("ubuntu-24.04", "macos-14") for feature in ("default", "no-default-features")),
        job("ci.yml", "build", os="ubuntu-24.04", target="x86_64-unknown-linux-gnu"),
        job("ci.yml", "build", os="macos-14", target="aarch64-apple-darwin"),
    }
)
BRAIN = frozenset(
    job("issue-56-brain-isolation.yml", "isolation-boundaries", os=os)
    for os in ("ubuntu-24.04", "macos-14")
)
HYGIENE = frozenset(
    job("repository-hygiene.yml", "tracked-tree", os=os)
    for os in ("ubuntu-24.04", "macos-14")
)
EFFECT = frozenset({job("issue-163-effect-audit.yml", "effect-audit")})
DOCS = frozenset({job("docs.yml", "current-docs")})
QUICK_XML = frozenset(
    {job("issue-185-spreadsheet-advisories.yml", "quick-xml-dependency-contract")}
)
SSH_DEPENDENCY = frozenset(
    job(
        "issue-186-ssh-removal.yml",
        "dependency-graph",
        target=target,
        feature_name=feature,
    )
    for target in (
        "x86_64-unknown-linux-gnu",
        "aarch64-apple-darwin",
        "x86_64-pc-windows-msvc",
    )
    for feature in ("default", "no-default-features")
)
SSH_API = frozenset(
    job("issue-186-ssh-removal.yml", "public-api-absence", feature_name=feature)
    for feature in ("default", "no-default-features")
)
SSH = SSH_DEPENDENCY | SSH_API
OAUTH_105 = frozenset(
    job("issue-105-oauth.yml", name) for name in ("oauth", "windows-compile", "macos-oauth")
)
AUTH_201 = frozenset(
    {
        job("issue-201-chatgpt-auth.yml", "windows-verifier-compile"),
        *(job("issue-201-chatgpt-auth.yml", "focused-auth", os=os) for os in ("ubuntu-24.04", "macos-14")),
    }
)

FIXTURES = (
    Fixture("README", ("README.md",), CI | HYGIENE | DOCS),
    Fixture("ordinary source", ("src/models/mod.rs",), CI | BRAIN | HYGIENE),
    Fixture(
        "representative Brain/effect",
        ("src/brain/store.rs", "src/server/handlers.rs"),
        CI | BRAIN | HYGIENE | EFFECT,
    ),
    Fixture("Cargo.toml", ("Cargo.toml",), CI | BRAIN | HYGIENE | QUICK_XML | SSH | OAUTH_105 | AUTH_201),
    Fixture("Cargo.lock", ("Cargo.lock",), CI | BRAIN | HYGIENE | QUICK_XML | SSH),
    Fixture("public API", ("src/lib.rs",), CI | BRAIN | HYGIENE | SSH | OAUTH_105),
    Fixture(
        "SSH source guard",
        ("scripts/check_no_ssh_surface.py",),
        CI | BRAIN | HYGIENE | SSH,
    ),
    Fixture(
        "SSH API guard",
        ("scripts/check_removed_ssh_api.py",),
        CI | BRAIN | HYGIENE | SSH,
    ),
    Fixture(
        "spreadsheet workflow definition",
        (".github/workflows/issue-185-spreadsheet-advisories.yml",),
        CI | HYGIENE | QUICK_XML,
    ),
    Fixture(
        "SSH workflow definition",
        (".github/workflows/issue-186-ssh-removal.yml",),
        CI | HYGIENE | SSH,
    ),
    Fixture(
        "fan-out guard",
        ("scripts/check_workflow_fanout.py", "scripts/test_workflow_fanout.py"),
        CI | BRAIN | HYGIENE,
    ),
    Fixture(
        "hygiene workflow definition",
        (".github/workflows/repository-hygiene.yml",),
        CI | HYGIENE,
    ),
)


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", type=Path, default=ROOT, help="Git worktree to inspect")
    return parser.parse_args()


def load_workflow(path: Path) -> dict[str, Any]:
    try:
        parsed = yaml.load(path.read_text(encoding="utf-8"), Loader=yaml.BaseLoader)
    except (OSError, UnicodeDecodeError, yaml.YAMLError) as error:
        raise ValueError(f"{path}: cannot parse workflow YAML: {error}") from error
    if not isinstance(parsed, dict):
        raise ValueError(f"{path}: workflow root must be a mapping")
    return parsed


def inert(value: Any) -> bool:
    return isinstance(value, str) and value.strip().lower() in FALSE_EXPRESSIONS


def glob_regex(pattern: str) -> re.Pattern[str]:
    output = ""
    index = 0
    while index < len(pattern):
        character = pattern[index]
        if character == "*":
            if index + 1 < len(pattern) and pattern[index + 1] == "*":
                output += ".*"
                index += 2
                continue
            output += "[^/]*"
        elif character == "?":
            output += "[^/]"
        else:
            output += re.escape(character)
        index += 1
    return re.compile(f"^{output}$")


def matches_patterns(path: str, patterns: list[str]) -> bool:
    matched = False
    for raw_pattern in patterns:
        negative = raw_pattern.startswith("!")
        pattern = raw_pattern[1:] if negative else raw_pattern
        if glob_regex(pattern).match(path):
            matched = not negative
    return matched


def string_list(value: Any) -> list[str]:
    if value is None:
        return []
    if isinstance(value, list):
        return [str(item) for item in value]
    return [str(value)]


def pull_request_trigger(workflow: dict[str, Any]) -> Any:
    trigger = workflow.get("on")
    if isinstance(trigger, str):
        return {} if trigger == "pull_request" else None
    if isinstance(trigger, list):
        return {} if "pull_request" in trigger else None
    if isinstance(trigger, dict) and "pull_request" in trigger:
        return trigger["pull_request"] or {}
    return None


def workflow_activates(workflow: dict[str, Any], changed: tuple[str, ...]) -> bool:
    trigger = pull_request_trigger(workflow)
    if trigger is None:
        return False
    if not isinstance(trigger, dict):
        raise ValueError("pull_request trigger must be a mapping or null")
    branches = string_list(trigger.get("branches"))
    if branches and not matches_patterns("main", branches):
        return False
    ignored_branches = string_list(trigger.get("branches-ignore"))
    if ignored_branches and matches_patterns("main", ignored_branches):
        return False
    paths = string_list(trigger.get("paths"))
    if paths and not any(matches_patterns(path, paths) for path in changed):
        return False
    paths_ignore = string_list(trigger.get("paths-ignore"))
    if paths_ignore and all(matches_patterns(path, paths_ignore) for path in changed):
        return False
    return True


def matrix_rows(job_definition: dict[str, Any]) -> list[dict[str, str]]:
    strategy = job_definition.get("strategy") or {}
    matrix = strategy.get("matrix") if isinstance(strategy, dict) else None
    if not isinstance(matrix, dict):
        return [{}]
    axes = {key: string_list(value) for key, value in matrix.items() if key not in {"include", "exclude"}}
    if axes:
        keys = list(axes)
        original = [dict(zip(keys, values)) for values in itertools.product(*(axes[key] for key in keys))]
        rows = [dict(row) for row in original]
    else:
        original = []
        rows = []
    includes = matrix.get("include") or []
    if isinstance(includes, dict):
        includes = [includes]
    for include in includes:
        if not isinstance(include, dict):
            raise ValueError("matrix include entries must be mappings")
        include = {str(key): str(value) for key, value in include.items()}
        compatible = [index for index, row in enumerate(original) if all(key not in row or row[key] == value for key, value in include.items())]
        if compatible:
            for index in compatible:
                rows[index].update(include)
        else:
            rows.append(include)
    excludes = matrix.get("exclude") or []
    if isinstance(excludes, dict):
        excludes = [excludes]
    for exclude in excludes:
        if not isinstance(exclude, dict):
            raise ValueError("matrix exclude entries must be mappings")
        excluded = {str(key): str(value) for key, value in exclude.items()}
        rows = [row for row in rows if not all(row.get(key) == value for key, value in excluded.items())]
    return rows or [{}]


def active_jobs(workflow_name: str, workflow: dict[str, Any]) -> set[str]:
    jobs = workflow.get("jobs")
    if not isinstance(jobs, dict):
        raise ValueError(f"{workflow_name}: jobs must be a mapping")
    expanded: set[str] = set()
    for job_name, definition in jobs.items():
        if not isinstance(definition, dict):
            raise ValueError(f"{workflow_name}::{job_name}: job must be a mapping")
        if inert(definition.get("if")):
            continue
        for row in matrix_rows(definition):
            visible = {key: value for key, value in row.items() if not key.endswith("_args") and key not in {"probe_args"}}
            expanded.add(job(workflow_name, str(job_name), **visible))
    return expanded


def active_steps(definition: dict[str, Any]) -> list[dict[str, Any]]:
    steps = definition.get("steps") or []
    return [step for step in steps if isinstance(step, dict) and not inert(step.get("if"))]


def all_run_text(definition: dict[str, Any]) -> str:
    return "\n".join(str(step.get("run", "")) for step in active_steps(definition))


def full_audit_jobs(workflows: dict[str, dict[str, Any]], active: set[str]) -> list[str]:
    audits: list[str] = []
    for expanded in active:
        workflow_name, remainder = expanded.split("::", 1)
        job_name = remainder.split("[", 1)[0]
        definition = workflows[workflow_name]["jobs"][job_name]
        if re.search(r"(?m)(?:^|[;&|]\s*)cargo\s+audit(?:\s|$)", all_run_text(definition)):
            audits.append(expanded)
    return sorted(audits)


def validate_contract(root: Path) -> list[str]:
    errors: list[str] = []
    workflow_dir = root / WORKFLOWS
    workflows: dict[str, dict[str, Any]] = {}
    try:
        for path in sorted(workflow_dir.glob("*.yml")):
            workflows[path.name] = load_workflow(path)
    except ValueError as error:
        return [str(error)]

    for name in ("issue-185-spreadsheet-advisories.yml", "issue-186-ssh-removal.yml"):
        trigger = pull_request_trigger(workflows.get(name, {}))
        if isinstance(trigger, dict) and not trigger.get("paths") and not trigger.get("paths-ignore"):
            errors.append(f"{name}: closed-issue pull_request trigger is unfiltered (paths/paths-ignore missing)")

    activations: dict[str, set[str]] = {}
    for fixture in FIXTURES:
        actual: set[str] = set()
        try:
            for name, workflow in workflows.items():
                if workflow_activates(workflow, fixture.paths):
                    actual |= active_jobs(name, workflow)
        except ValueError as error:
            errors.append(f"fixture {fixture.name}: {error}")
            continue
        activations[fixture.name] = actual
        if actual != fixture.expected:
            missing = sorted(fixture.expected - actual)
            unexpected = sorted(actual - fixture.expected)
            errors.append(
                f"fixture {fixture.name} ({', '.join(fixture.paths)}): expected {len(fixture.expected)} activated jobs, found {len(actual)}; "
                f"missing={missing}; unexpected={unexpected}; active={sorted(actual)}"
            )

    ordinary = activations.get("ordinary source", set())
    audits = full_audit_jobs(workflows, ordinary)
    if len(audits) != 1:
        errors.append(f"ordinary source must activate exactly one full cargo audit; found {len(audits)} in {audits}")

    canonical_tests = {
        (os, feature)
        for os in ("ubuntu-24.04", "macos-14")
        for feature in ("default", "no-default-features")
    }
    canonical_builds = {
        ("ubuntu-24.04", "x86_64-unknown-linux-gnu"),
        ("macos-14", "aarch64-apple-darwin"),
    }
    for name, workflow in workflows.items():
        if name == "ci.yml" or pull_request_trigger(workflow) is None:
            continue
        for job_name, definition in workflow.get("jobs", {}).items():
            if not isinstance(definition, dict) or inert(definition.get("if")):
                continue
            try:
                rows = matrix_rows(definition)
            except ValueError as error:
                errors.append(f"{name}::{job_name}: {error}")
                continue
            test_rows = {
                (row.get("os"), row.get("feature_name"))
                for row in rows
                if "os" in row and "feature_name" in row
            }
            build_rows = {
                (row.get("os"), row.get("target"))
                for row in rows
                if "os" in row and "target" in row
            }
            commands = all_run_text(definition)
            if canonical_tests <= test_rows and "cargo test --all-targets" in commands:
                errors.append(
                    f"{name}::{job_name}: duplicates the canonical four-job all-target "
                    f"OS x feature matrix: {sorted(canonical_tests)}"
                )
            if canonical_builds <= build_rows and "cargo build --release" in commands:
                errors.append(
                    f"{name}::{job_name}: duplicates the canonical two-job release "
                    f"OS x target matrix: {sorted(canonical_builds)}"
                )

    hygiene = workflows.get("repository-hygiene.yml", {})
    hygiene_jobs = hygiene.get("jobs", {}) if isinstance(hygiene, dict) else {}
    tracked = hygiene_jobs.get("tracked-tree", {}) if isinstance(hygiene_jobs, dict) else {}
    hygiene_run = all_run_text(tracked) if isinstance(tracked, dict) else ""
    required_hygiene = (
        "scripts/check_repository_hygiene.py",
        "scripts/test_repository_hygiene.py",
        "scripts/check_no_ssh_surface.py",
        "scripts/test_no_ssh_surface.py",
        "scripts/check_workflow_fanout.py",
        "scripts/test_workflow_fanout.py",
    )
    for command in required_hygiene:
        if command not in hygiene_run:
            errors.append(f"repository-hygiene.yml::tracked-tree: active steps must run {command}; comments or if:false steps do not satisfy the guard")

    required_text = {
        "issue-185-spreadsheet-advisories.yml": ("0.41.0", "RUSTSEC-2026-0194", "RUSTSEC-2026-0195", "quick-xml"),
        "issue-186-ssh-removal.yml": (
            "rsa",
            "russh",
            "russh-keys",
            "russh-cryptovec",
            "RUSTSEC-2026-0154",
            "RUSTSEC-2026-0153",
            "RUSTSEC-2023-0071",
            "scripts/check_removed_ssh_api.py",
        ),
    }
    for name, needles in required_text.items():
        workflow = workflows.get(name, {})
        jobs = workflow.get("jobs", {}) if isinstance(workflow, dict) else {}
        active_text = "\n".join(all_run_text(definition) for definition in jobs.values() if isinstance(definition, dict) and not inert(definition.get("if")))
        for needle in needles:
            if needle not in active_text:
                errors.append(f"{name}: active jobs/steps do not enforce required security marker {needle!r}")

    return errors


def main() -> int:
    args = parse_args()
    errors = validate_contract(args.root.resolve())
    if errors:
        for error in errors:
            print(f"workflow fan-out: {error}", file=sys.stderr)
        return 1
    print("workflow fan-out: representative PR fixtures satisfy the pinned semantic job contract")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
