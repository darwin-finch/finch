#!/usr/bin/env python3
"""Semantically expand pull-request workflow fan-out for representative changes."""

from __future__ import annotations

import argparse
import hashlib
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


def job(workflow: str, name: str, row: int | None = None, **matrix: str) -> str:
    suffix = ""
    if row is not None:
        fields = [f"row={row}"]
        fields.extend(f"{key}={matrix[key]}" for key in sorted(matrix))
        suffix = "[" + ",".join(fields) + "]"
    return f"{workflow}::{name}{suffix}"


CI = frozenset(
    {
        job("ci.yml", "toolchain-contract"),
        job("ci.yml", "windows-format-contract"),
        job("ci.yml", "runtime-authority"),
        job("ci.yml", "security"),
        job("ci.yml", "test", row=0, os="ubuntu-24.04", feature_name="default", cargo_args=""),
        job("ci.yml", "test", row=1, os="ubuntu-24.04", feature_name="no-default-features", cargo_args="--no-default-features"),
        job("ci.yml", "test", row=2, os="macos-14", feature_name="default", cargo_args=""),
        job("ci.yml", "test", row=3, os="macos-14", feature_name="no-default-features", cargo_args="--no-default-features"),
        job("ci.yml", "build", row=0, os="ubuntu-24.04", target="x86_64-unknown-linux-gnu"),
        job("ci.yml", "build", row=1, os="macos-14", target="aarch64-apple-darwin"),
    }
)
BRAIN = frozenset(
    job("issue-56-brain-isolation.yml", "isolation-boundaries", row=row, os=os)
    for row, os in enumerate(("ubuntu-24.04", "macos-14"))
)
HYGIENE = frozenset(
    job("repository-hygiene.yml", "tracked-tree", row=row, os=os)
    for row, os in enumerate(("ubuntu-24.04", "macos-14"))
)
EFFECT = frozenset({job("issue-163-effect-audit.yml", "effect-audit")})
DOCS = frozenset({job("docs.yml", "current-docs")})
OAUTH_105 = frozenset(
    job("issue-105-oauth.yml", name) for name in ("oauth", "windows-compile", "macos-oauth")
)
AUTH_201 = frozenset(
    {
        job("issue-201-chatgpt-auth.yml", "windows-verifier-compile"),
        *(job("issue-201-chatgpt-auth.yml", "focused-auth", row=row, os=os) for row, os in enumerate(("ubuntu-24.04", "macos-14"))),
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
    Fixture("Cargo.toml", ("Cargo.toml",), CI | BRAIN | HYGIENE | OAUTH_105 | AUTH_201),
    Fixture("Cargo.lock", ("Cargo.lock",), CI | BRAIN | HYGIENE),
    Fixture("public API", ("src/lib.rs",), CI | BRAIN | HYGIENE | OAUTH_105),
    Fixture(
        "SSH source guard",
        ("scripts/check_no_ssh_surface.py",),
        CI | BRAIN | HYGIENE,
    ),
    Fixture(
        "SSH API guard",
        ("scripts/check_removed_ssh_api.py",),
        CI | BRAIN | HYGIENE,
    ),
    Fixture(
        "fan-out guard",
        ("scripts/check_workflow_fanout.py", "scripts/test_workflow_fanout.py"),
        CI | BRAIN | HYGIENE,
    ),
    Fixture(
        "canonical CI workflow definition",
        (".github/workflows/ci.yml",),
        CI | HYGIENE,
    ),
    Fixture(
        "hygiene workflow definition",
        (".github/workflows/repository-hygiene.yml",),
        CI | HYGIENE,
    ),
)

REQUIRED_PR_EVENT_TYPES = {"opened", "reopened", "synchronize"}
CANONICAL_JOB_WORKFLOWS = {
    "ci.yml",
    "issue-56-brain-isolation.yml",
    "issue-163-effect-audit.yml",
    "repository-hygiene.yml",
}
STEP_CONTRACTS = {
    ("ci.yml", "security", "Install cargo-audit"): (
        None,
        None,
        "188b5f08be3448fb097738a6932e476d72c2be5ad765135c1bfdcdf2f65e397b",
    ),
    (
        "ci.yml",
        "security",
        "Prove removed SSH and RSA packages stay out of the resolved graph",
    ): (
        None,
        None,
        "02dbc02d5f567d0c871979ef5c7db47888d6313e0e13ffdf13d4cb43a00ecc7a",
    ),
    ("ci.yml", "security", "Prove the quick-xml floor excludes both historical advisories"): (
        None,
        None,
        "0fcfd2876aa0f81253a1375ee794e0109b80022821f228abd959567f80cf930d",
    ),
    (
        "ci.yml",
        "security",
        "Run the one full Cargo audit and retain named advisory diagnostics",
    ): (
        None,
        None,
        "ab79352de7d6a3b55bb297401ad15bb5f0958d58038174f3cf9505d3110578f8",
    ),
    ("ci.yml", "security", "Detect manifest and public-API changes"): (
        "security-paths",
        None,
        "41bb9c34ef3419caa50bfa498cb7728f1ac9bf68a6b146334dcc99e1ba1bfdeb",
    ),
    (
        "ci.yml",
        "security",
        "Install Cap'n Proto for the downstream removed-API probe",
    ): (
        None,
        "steps.security-paths.outputs.removed_ssh_api == 'true'",
        "e28080565f920b77a6cd1496403ceabe68e8b69de9e8c7d0ca479253ff76d7a8",
    ),
    (
        "ci.yml",
        "security",
        "Compile downstream positive controls and require removed-API diagnostics",
    ): (
        None,
        "steps.security-paths.outputs.removed_ssh_api == 'true'",
        "bb32ab2e21e5d905480b04b3c8ec58f9c930d7493ecc091b84a9f2f099d75b65",
    ),
    ("repository-hygiene.yml", "tracked-tree", "Install workflow parser"): (
        None,
        None,
        "98c37f93dd1883bfd9a5b1503c6dfdbaf6426a03200f484df880f2552d9c7ca3",
    ),
    ("repository-hygiene.yml", "tracked-tree", "Check the current tracked tree"): (
        None,
        None,
        "de4214bf756c6e08d0e4afd810f29a49df948fb283c5116ff63960dbee838f74",
    ),
    ("repository-hygiene.yml", "tracked-tree", "Run repository hygiene regressions"): (
        None,
        None,
        "0c275c736f4b45ff7bb6ab20e37b0b2568808bb3479329ccb4cbcfb4f8959067",
    ),
    (
        "repository-hygiene.yml",
        "tracked-tree",
        "Reject restoration of the removed SSH surface",
    ): (
        None,
        None,
        "5e0b3034d9604333239df0a694f1c5f7b3dd461b6ee261a8c9935103720448a5",
    ),
    ("repository-hygiene.yml", "tracked-tree", "Run removed SSH surface regressions"): (
        None,
        None,
        "68bb4534a4a312eaa3bcbc737f01f59cad1e81933546a6d7f9e5cf91473ca4fb",
    ),
    ("repository-hygiene.yml", "tracked-tree", "Check pull-request workflow fan-out"): (
        None,
        None,
        "ee20773994b002b6bc4f51fbee99963d555b56e1004e940567fa30e61ce9f66a",
    ),
    (
        "repository-hygiene.yml",
        "tracked-tree",
        "Run workflow fan-out mutation regressions",
    ): (
        None,
        None,
        "2a4393c20469ccce3439568a72b3806077ff52dde1edd1c6cd64929dacd1688a",
    ),
}
EXPLICIT_BASH_STEPS = {
    ("ci.yml", "security", "Prove removed SSH and RSA packages stay out of the resolved graph"),
    ("ci.yml", "security", "Prove the quick-xml floor excludes both historical advisories"),
    (
        "ci.yml",
        "security",
        "Run the one full Cargo audit and retain named advisory diagnostics",
    ),
    ("ci.yml", "security", "Detect manifest and public-API changes"),
}


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
    event_types = string_list(trigger.get("types"))
    if "types" in trigger and "synchronize" not in event_types:
        return False
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


def has_matrix(job_definition: dict[str, Any]) -> bool:
    strategy = job_definition.get("strategy")
    return isinstance(strategy, dict) and isinstance(strategy.get("matrix"), dict)


def active_jobs(workflow_name: str, workflow: dict[str, Any]) -> set[str]:
    jobs = workflow.get("jobs")
    if not isinstance(jobs, dict):
        raise ValueError(f"{workflow_name}: jobs must be a mapping")
    expanded: set[str] = set()
    for job_name, definition in jobs.items():
        if not isinstance(definition, dict):
            raise ValueError(f"{workflow_name}::{job_name}: job must be a mapping")
        if definition.get("if") is not None:
            continue
        rows = matrix_rows(definition)
        for row_number, matrix in enumerate(rows):
            expanded.add(
                job(
                    workflow_name,
                    str(job_name),
                    row=row_number if has_matrix(definition) else None,
                    **matrix,
                )
            )
    return expanded


def active_steps(definition: dict[str, Any]) -> list[dict[str, Any]]:
    steps = definition.get("steps") or []
    return [step for step in steps if isinstance(step, dict) and not inert(step.get("if"))]


def all_run_text(definition: dict[str, Any]) -> str:
    lines = "\n".join(str(step.get("run", "")) for step in active_steps(definition))
    return "\n".join(line for line in lines.splitlines() if not line.lstrip().startswith("#"))


def normalized_run(step: dict[str, Any]) -> str:
    run = str(step.get("run", ""))
    return "\n".join(line.rstrip() for line in run.strip().splitlines())


def run_digest(step: dict[str, Any]) -> str:
    return hashlib.sha256(normalized_run(step).encode("utf-8")).hexdigest()


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
        paths = sorted((*workflow_dir.glob("*.yml"), *workflow_dir.glob("*.yaml")))
        for path in paths:
            workflows[path.name] = load_workflow(path)
    except ValueError as error:
        return [str(error)]

    for stem in ("issue-185-spreadsheet-advisories", "issue-186-ssh-removal"):
        for suffix in (".yml", ".yaml"):
            name = stem + suffix
            if name not in workflows:
                continue
            trigger = pull_request_trigger(workflows[name])
            trigger_detail = "no pull_request trigger" if trigger is None else repr(trigger)
            errors.append(
                f"{name}: retired closed-issue workflow returned; permanent guards belong in "
                f"canonical CI/repository hygiene (pull_request={trigger_detail})"
            )

    for name, workflow in workflows.items():
        trigger = pull_request_trigger(workflow)
        if not isinstance(trigger, dict):
            continue
        event_types = set(string_list(trigger.get("types")))
        if "types" in trigger and not REQUIRED_PR_EVENT_TYPES <= event_types:
            missing = sorted(REQUIRED_PR_EVENT_TYPES - event_types)
            errors.append(
                f"{name}: pull_request.types={sorted(event_types)} omits required PR "
                f"activation types {missing}"
            )
        if name in CANONICAL_JOB_WORKFLOWS and "types" in trigger:
            errors.append(
                f"{name}: canonical pull_request trigger must omit types so opened, "
                "reopened, and synchronize remain enabled"
            )

    expected_triggers = {
        "ci.yml": {"branches": ["main"]},
        "repository-hygiene.yml": {},
    }
    for name, expected_trigger in expected_triggers.items():
        actual_trigger = pull_request_trigger(workflows.get(name, {}))
        if actual_trigger != expected_trigger:
            errors.append(
                f"{name}: canonical pull_request trigger changed; "
                f"expected={expected_trigger!r}, actual={actual_trigger!r}"
            )

    for name in CANONICAL_JOB_WORKFLOWS:
        workflow = workflows.get(name, {})
        jobs = workflow.get("jobs", {}) if isinstance(workflow, dict) else {}
        if not isinstance(jobs, dict):
            continue
        for job_name, definition in jobs.items():
            if isinstance(definition, dict) and definition.get("if") is not None:
                errors.append(
                    f"{name}::{job_name}: canonical job-level if is unsupported; "
                    f"found {definition.get('if')!r}, expected no condition"
                )

    for (workflow_name, job_name, step_name), contract in STEP_CONTRACTS.items():
        expected_id, expected_if, expected_digest = contract
        workflow = workflows.get(workflow_name, {})
        jobs = workflow.get("jobs", {}) if isinstance(workflow, dict) else {}
        definition = jobs.get(job_name, {}) if isinstance(jobs, dict) else {}
        steps = definition.get("steps", []) if isinstance(definition, dict) else []
        matches = [
            step
            for step in steps
            if isinstance(step, dict) and step.get("name") == step_name
        ]
        location = f"{workflow_name}::{job_name}::{step_name}"
        if len(matches) != 1:
            errors.append(
                f"{location}: expected exactly one critical step, found {len(matches)}"
            )
            continue
        step = matches[0]
        actual_id = step.get("id")
        if actual_id != expected_id:
            errors.append(
                f"{location}: id changed; expected={expected_id!r}, actual={actual_id!r}"
            )
        actual_if = step.get("if")
        if actual_if != expected_if:
            errors.append(
                f"{location}: condition changed; expected={expected_if!r}, "
                f"actual={actual_if!r}; inverted, constant-false, and push-only "
                "conditions are unsupported"
            )
        expected_shell = "bash" if (workflow_name, job_name, step_name) in EXPLICIT_BASH_STEPS else None
        actual_shell = step.get("shell")
        if actual_shell != expected_shell:
            errors.append(
                f"{location}: shell changed; expected={expected_shell!r}, "
                f"actual={actual_shell!r}"
            )
        if step.get("continue-on-error") is not None:
            errors.append(
                f"{location}: continue-on-error is unsupported for a required guard; "
                f"found {step.get('continue-on-error')!r}"
            )
        actual_digest = run_digest(step)
        if actual_digest != expected_digest:
            errors.append(
                f"{location}: normalized run block changed; expected sha256="
                f"{expected_digest}, actual sha256={actual_digest}; restore the reviewed "
                "command block or update this contract with matching mutation coverage"
            )

    allowed_audit_steps = {
        ("ci.yml", "security", "Install cargo-audit"),
        (
            "ci.yml",
            "security",
            "Run the one full Cargo audit and retain named advisory diagnostics",
        ),
    }
    audit_pattern = re.compile(r"\bcargo(?:-audit|\s+audit)\b")
    for workflow_name, workflow in workflows.items():
        jobs = workflow.get("jobs", {}) if isinstance(workflow, dict) else {}
        if not isinstance(jobs, dict):
            continue
        for job_name, definition in jobs.items():
            steps = definition.get("steps", []) if isinstance(definition, dict) else []
            for index, step in enumerate(steps):
                if not isinstance(step, dict) or not audit_pattern.search(normalized_run(step)):
                    continue
                identity = (workflow_name, str(job_name), str(step.get("name", "")))
                if identity not in allowed_audit_steps:
                    errors.append(
                        f"{workflow_name}::{job_name}::step[{index}]: cargo-audit invocation "
                        f"is outside the two exact canonical install/audit steps: "
                        f"{normalized_run(step)!r}"
                    )

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
            if canonical_tests <= test_rows and re.search(
                r"(?m)cargo\s+test[^\n]*--all-targets", commands
            ):
                errors.append(
                    f"{name}::{job_name}: duplicates the canonical four-job all-target "
                    f"OS x feature matrix: {sorted(canonical_tests)}"
                )
            if canonical_builds <= build_rows and re.search(
                r"(?m)cargo\s+build[^\n]*--release", commands
            ):
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

    ci = workflows.get("ci.yml", {})
    ci_jobs = ci.get("jobs", {}) if isinstance(ci, dict) else {}
    security = ci_jobs.get("security", {}) if isinstance(ci_jobs, dict) else {}
    security_run = all_run_text(security) if isinstance(security, dict) else ""
    security_steps = active_steps(security) if isinstance(security, dict) else []
    detector_run = "\n".join(
        str(step.get("run", ""))
        for step in security_steps
        if step.get("id") == "security-paths"
    )
    security_markers = (
        "cargo audit --json",
        "grep -Eq '^name = \"(rsa|russh|russh-cryptovec|russh-keys)\"$' Cargo.lock",
        "cargo tree --invert quick-xml",
        "minimum=0.41.0",
        "quick-xml",
        "0.41.0",
        "RUSTSEC-2026-0194",
        "RUSTSEC-2026-0195",
        "rsa",
        "russh",
        "russh-keys",
        "russh-cryptovec",
        "RUSTSEC-2026-0154",
        "RUSTSEC-2026-0153",
        "RUSTSEC-2023-0071",
        "scripts/check_removed_ssh_api.py",
        "--no-default-features",
        "removed_ssh_api=true",
    )
    for marker in security_markers:
        if marker not in security_run:
            errors.append(
                "ci.yml::security: active jobs/steps do not enforce required security "
                f"marker {marker!r}; if:false steps and comments are inert"
            )
    if security_run.count("python3 scripts/check_removed_ssh_api.py") != 2:
        errors.append(
            "ci.yml::security: active downstream removed-API step must run exactly two "
            "positive/negative probes (default and no-default-features)"
        )
    for path in (
        "Cargo.toml",
        "Cargo.lock",
        "src/lib.rs",
        "scripts/check_removed_ssh_api.py",
        ".github/workflows/ci.yml",
    ):
        if path not in detector_run:
            errors.append(
                "ci.yml::security: downstream removed-API change detector does not include "
                f"required path {path!r}"
            )

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
