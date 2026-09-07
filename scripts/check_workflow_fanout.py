#!/usr/bin/env python3
"""Fail closed when pull-request workflows exceed Finch's pinned CI contract."""

from __future__ import annotations

import argparse
import itertools
import re
import sys
from collections import Counter
from dataclasses import dataclass
from pathlib import Path
from textwrap import dedent
from typing import Any

try:
    import yaml
except ImportError as error:  # pragma: no cover - exercised by the workflow bootstrap
    raise SystemExit(
        "workflow fan-out: PyYAML is required (python3 -m pip install PyYAML==6.0.3)"
    ) from error


ROOT = Path(__file__).resolve().parent.parent
WORKFLOWS = Path(".github/workflows")
FALSE_CONDITIONS = {"false", "${{ false }}", "${{false}}"}


class UniqueBaseLoader(yaml.BaseLoader):
    """BaseLoader with fail-closed duplicate mapping-key detection."""


def construct_unique_mapping(
    loader: UniqueBaseLoader, node: yaml.MappingNode, deep: bool = False
) -> dict[Any, Any]:
    mapping: dict[Any, Any] = {}
    for key_node, value_node in node.value:
        key = loader.construct_object(key_node, deep=deep)
        if key in mapping:
            raise yaml.constructor.ConstructorError(
                "while constructing a mapping",
                node.start_mark,
                f"duplicate mapping key {key!r}",
                key_node.start_mark,
            )
        mapping[key] = loader.construct_object(value_node, deep=deep)
    return mapping


UniqueBaseLoader.add_constructor(
    yaml.resolver.BaseResolver.DEFAULT_MAPPING_TAG, construct_unique_mapping
)


@dataclass(frozen=True)
class Fixture:
    name: str
    paths: tuple[str, ...]
    expected: Counter[str]


def identity(
    workflow: str,
    job_name: str,
    row: int | None = None,
    *,
    condition: str = "<none>",
    **matrix: str,
) -> str:
    fields = [f"if={condition}"]
    if row is not None:
        fields.insert(0, f"row={row}")
    if row is not None:
        fields.extend(f"{key}={matrix[key]}" for key in sorted(matrix))
    return f"{workflow}::{job_name}[{','.join(fields)}]"


def counted(*items: str) -> Counter[str]:
    return Counter(items)


CI = counted(
    identity("ci.yml", "toolchain-contract"),
    identity("ci.yml", "windows-format-contract"),
    identity("ci.yml", "runtime-authority"),
    identity("ci.yml", "security"),
    identity("ci.yml", "test", 0, os="ubuntu-24.04", feature_name="default", cargo_args=""),
    identity("ci.yml", "test", 1, os="ubuntu-24.04", feature_name="no-default-features", cargo_args="--no-default-features"),
    identity("ci.yml", "test", 2, os="macos-14", feature_name="default", cargo_args=""),
    identity("ci.yml", "test", 3, os="macos-14", feature_name="no-default-features", cargo_args="--no-default-features"),
    identity("ci.yml", "build", 0, os="ubuntu-24.04", target="x86_64-unknown-linux-gnu"),
    identity("ci.yml", "build", 1, os="macos-14", target="aarch64-apple-darwin"),
)
BRAIN = counted(
    *(identity("issue-56-brain-isolation.yml", "isolation-boundaries", row, os=os)
      for row, os in enumerate(("ubuntu-24.04", "macos-14")))
)
HYGIENE = counted(
    *(identity("repository-hygiene.yml", "tracked-tree", row, os=os)
      for row, os in enumerate(("ubuntu-24.04", "macos-14")))
)
EFFECT = counted(identity("issue-163-effect-audit.yml", "effect-audit"))
DOCS = counted(identity("docs.yml", "current-docs"))
OAUTH_105 = counted(
    *(identity("issue-105-oauth.yml", name)
      for name in ("oauth", "windows-compile", "macos-oauth"))
)
AUTH_201 = counted(
    identity("issue-201-chatgpt-auth.yml", "windows-verifier-compile"),
    *(identity("issue-201-chatgpt-auth.yml", "focused-auth", row, os=os)
      for row, os in enumerate(("ubuntu-24.04", "macos-14"))),
)


FIXTURES = (
    Fixture("README", ("README.md",), CI + HYGIENE + DOCS),
    Fixture("ordinary source", ("src/models/mod.rs",), CI + BRAIN + HYGIENE),
    Fixture(
        "representative Brain/effect",
        ("src/brain/store.rs", "src/server/handlers.rs"),
        CI + BRAIN + HYGIENE + EFFECT,
    ),
    Fixture("Cargo.toml", ("Cargo.toml",), CI + BRAIN + HYGIENE + OAUTH_105 + AUTH_201),
    Fixture("Cargo.lock", ("Cargo.lock",), CI + BRAIN + HYGIENE),
    Fixture("public API", ("src/lib.rs",), CI + BRAIN + HYGIENE + OAUTH_105),
    Fixture("SSH source guard", ("scripts/check_no_ssh_surface.py",), CI + BRAIN + HYGIENE),
    Fixture("SSH API guard", ("scripts/check_removed_ssh_api.py",), CI + BRAIN + HYGIENE),
    Fixture(
        "fan-out guard",
        ("scripts/check_workflow_fanout.py", "scripts/test_workflow_fanout.py"),
        CI + BRAIN + HYGIENE,
    ),
    Fixture("canonical CI workflow definition", (".github/workflows/ci.yml",), CI + HYGIENE),
    Fixture(
        "hygiene workflow definition",
        (".github/workflows/repository-hygiene.yml",),
        CI + HYGIENE,
    ),
)


def block(value: str) -> str:
    return dedent(value).strip()


SECURITY_STEPS: tuple[dict[str, Any], ...] = (
    {"name": "Checkout code", "uses": "actions/checkout@v4"},
    {"name": "Install repository Rust toolchain", "uses": "dtolnay/rust-toolchain@1.98.0"},
    {"name": "Install cargo-audit", "run": "cargo install cargo-audit --locked"},
    {
        "name": "Verify resolved HTTP dependency contract",
        "run": block("""
            cargo generate-lockfile
            python3 scripts/test_http_dependency_contract.py
            python3 scripts/check_http_dependency_contract.py
        """),
    },
    {
        "name": "Prove removed SSH and RSA packages stay out of the resolved graph",
        "shell": "bash",
        "run": block(r"""
            printf '%s\n' \
              'RUSTSEC-2026-0154 remains unreachable because russh is absent' \
              'RUSTSEC-2026-0153 remains unreachable because russh-cryptovec is absent' \
              'RUSTSEC-2023-0071 remains unreachable because rsa is absent'
            if grep -Eq '^name = "(rsa|russh|russh-cryptovec|russh-keys)"$' Cargo.lock; then
              echo 'A removed SSH or RSA package returned to Cargo.lock' >&2
              grep -nE '^name = "(rsa|russh|russh-cryptovec|russh-keys)"$' Cargo.lock >&2
              exit 1
            fi
        """),
    },
    {
        "name": "Prove the quick-xml floor excludes both historical advisories",
        "shell": "bash",
        "run": block(r"""
            cargo tree --invert quick-xml --edges normal,build,dev --prefix none --format '{p}' \
              | sort -u | tee quick-xml-tree.txt
            printf '%s\n' \
              'RUSTSEC-2026-0194 is excluded by quick-xml >= 0.41.0' \
              'RUSTSEC-2026-0195 is excluded by quick-xml >= 0.41.0'
            minimum=0.41.0
            found=$(sed -n 's/^quick-xml v\([0-9][0-9.]*\).*/\1/p' quick-xml-tree.txt | sort -u)
            if [[ -z "$found" ]]; then
              echo 'no quick-xml in the graph; the advisory assertion would be vacuous' >&2
              exit 1
            fi
            for version in $found; do
              oldest=$(printf '%s\n%s\n' "$version" "$minimum" | sort -V | head -n1)
              if [[ "$oldest" == "$version" && "$version" != "$minimum" ]]; then
                echo "quick-xml $version is below the fixed $minimum and is still reachable" >&2
                exit 1
              fi
              echo "quick-xml $version is at or above the fixed $minimum"
            done
        """),
    },
    {
        "name": "Run the one full Cargo audit and retain named advisory diagnostics",
        "shell": "bash",
        "run": block(r"""
            set +e
            cargo audit --json > audit.json
            audit_status=$?
            set -e
            if (( audit_status != 0 && audit_status != 1 )); then
              echo "cargo-audit exited unexpectedly with status ${audit_status}" >&2
              exit 1
            fi
            test -s audit.json
            jq -e 'type == "object" and (.vulnerabilities.list | type == "array")' audit.json >/dev/null
            jq -r '.vulnerabilities.list[]? | [.advisory.id, .package.name, .package.version] | @tsv' audit.json
            jq -e '
              [.vulnerabilities.list[]?]
              | all(
                  .advisory.id != "RUSTSEC-2026-0194"
                  and .advisory.id != "RUSTSEC-2026-0195"
                  and .advisory.id != "RUSTSEC-2026-0154"
                  and .advisory.id != "RUSTSEC-2026-0153"
                  and .advisory.id != "RUSTSEC-2023-0071"
                  and .package.name != "rsa"
                  and .package.name != "russh"
                  and .package.name != "russh-cryptovec"
                  and .package.name != "russh-keys"
                )
            ' audit.json
            if (( audit_status != 0 )); then
              echo "cargo audit found advisories outside the five permanent focused guards" >&2
              exit "$audit_status"
            fi
        """),
    },
    {
        "name": "Detect manifest and public-API changes",
        "id": "security-paths",
        "shell": "bash",
        "run": block(r"""
            base='${{ github.event.pull_request.base.sha || github.event.before }}'
            if [[ -z "$base" || "$base" == "0000000000000000000000000000000000000000" ]]; then
              base="$(git rev-parse HEAD^)"
            fi
            git fetch --no-tags --depth=1 origin "$base"
            set +e
            git diff --quiet "$base" '${{ github.sha }}' -- \
              Cargo.toml Cargo.lock src/lib.rs scripts/check_removed_ssh_api.py \
              .github/workflows/ci.yml
            diff_status=$?
            set -e
            case "$diff_status" in
              0) echo 'removed_ssh_api=false' >> "$GITHUB_OUTPUT" ;;
              1) echo 'removed_ssh_api=true' >> "$GITHUB_OUTPUT" ;;
              *) echo "cannot inspect security-relevant changes: git diff exited $diff_status" >&2; exit "$diff_status" ;;
            esac
        """),
    },
    {
        "name": "Install Cap'n Proto for the downstream removed-API probe",
        "if": "steps.security-paths.outputs.removed_ssh_api == 'true'",
        "run": "sudo apt-get update && sudo apt-get install -y capnproto",
    },
    {
        "name": "Compile downstream positive controls and require removed-API diagnostics",
        "if": "steps.security-paths.outputs.removed_ssh_api == 'true'",
        "run": block("""
            python3 scripts/check_removed_ssh_api.py
            python3 scripts/check_removed_ssh_api.py --no-default-features
        """),
    },
)

HYGIENE_STEPS: tuple[dict[str, Any], ...] = (
    {"uses": "actions/checkout@v4"},
    {"uses": "actions/setup-python@v5", "with": {"python-version": "3.12"}},
    {"name": "Install workflow parser", "run": "python3 -m pip install --disable-pip-version-check PyYAML==6.0.3"},
    {"name": "Check the current tracked tree", "run": "python3 scripts/check_repository_hygiene.py"},
    {"name": "Run repository hygiene regressions", "run": "python3 scripts/test_repository_hygiene.py"},
    {"name": "Reject restoration of the removed SSH surface", "run": "python3 scripts/check_no_ssh_surface.py"},
    {"name": "Run removed SSH surface regressions", "run": "python3 scripts/test_no_ssh_surface.py"},
    {"name": "Check pull-request workflow fan-out", "run": "python3 scripts/check_workflow_fanout.py"},
    {"name": "Run workflow fan-out mutation regressions", "run": "python3 scripts/test_workflow_fanout.py"},
)


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", type=Path, default=ROOT, help="Git worktree to inspect")
    return parser.parse_args()


def load_workflow(path: Path) -> dict[str, Any]:
    try:
        value = yaml.load(path.read_text(encoding="utf-8"), Loader=UniqueBaseLoader)
    except (OSError, UnicodeDecodeError, yaml.YAMLError) as error:
        raise ValueError(f"{path}: cannot parse workflow YAML: {error}") from error
    if not isinstance(value, dict):
        raise ValueError(f"{path}: workflow root must be a mapping, found {value!r}")
    return value


def string_list(value: Any, context: str) -> list[str]:
    if isinstance(value, str):
        return [value]
    if isinstance(value, list) and all(isinstance(item, str) for item in value):
        return value
    raise ValueError(f"{context}: expected a string or list of strings, found {value!r}")


def pull_request_trigger(workflow: dict[str, Any], context: str) -> dict[str, Any] | None:
    trigger = workflow.get("on")
    if isinstance(trigger, str):
        return {} if trigger == "pull_request" else None
    if isinstance(trigger, list):
        return {} if "pull_request" in trigger else None
    if not isinstance(trigger, dict):
        raise ValueError(f"{context}: on must be a string, list, or mapping, found {trigger!r}")
    if "pull_request" not in trigger:
        return None
    value = trigger["pull_request"]
    if value in (None, ""):
        return {}
    if not isinstance(value, dict):
        raise ValueError(f"{context}: pull_request must be a mapping or empty, found {value!r}")
    return value


def glob_regex(pattern: str) -> re.Pattern[str]:
    output = ""
    index = 0
    while index < len(pattern):
        char = pattern[index]
        if char == "*":
            if index + 1 < len(pattern) and pattern[index + 1] == "*":
                output += ".*"
                index += 2
                continue
            output += "[^/]*"
        elif char == "?":
            output += "[^/]"
        else:
            output += re.escape(char)
        index += 1
    return re.compile(f"^{output}$")


def matches(path: str, patterns: list[str]) -> bool:
    matched = False
    for raw in patterns:
        negative = raw.startswith("!")
        pattern = raw[1:] if negative else raw
        if glob_regex(pattern).match(path):
            matched = not negative
    return matched


def activates_on_synchronize(
    workflow: dict[str, Any], changed: tuple[str, ...], context: str
) -> bool:
    trigger = pull_request_trigger(workflow, context)
    if trigger is None:
        return False
    if "types" in trigger and "synchronize" not in string_list(trigger["types"], f"{context}: pull_request.types"):
        return False
    if "branches" in trigger and not matches("main", string_list(trigger["branches"], f"{context}: pull_request.branches")):
        return False
    if "branches-ignore" in trigger and matches("main", string_list(trigger["branches-ignore"], f"{context}: pull_request.branches-ignore")):
        return False
    if "paths" in trigger and not any(matches(path, string_list(trigger["paths"], f"{context}: pull_request.paths")) for path in changed):
        return False
    if "paths-ignore" in trigger and all(matches(path, string_list(trigger["paths-ignore"], f"{context}: pull_request.paths-ignore")) for path in changed):
        return False
    return True


def matrix_rows(definition: dict[str, Any], context: str) -> tuple[bool, list[dict[str, str]]]:
    strategy = definition.get("strategy")
    if strategy is None:
        return False, [{}]
    if not isinstance(strategy, dict):
        raise ValueError(f"{context}: strategy must be a mapping, found {strategy!r}")
    matrix = strategy.get("matrix")
    if matrix is None:
        return False, [{}]
    if not isinstance(matrix, dict):
        raise ValueError(f"{context}: dynamic or non-mapping matrix is unsupported: {matrix!r}")
    axes: dict[str, list[str]] = {}
    for key, value in matrix.items():
        if key in {"include", "exclude"}:
            continue
        if (
            not isinstance(value, list)
            or not value
            or not all(isinstance(item, str) and "${{" not in item for item in value)
        ):
            raise ValueError(f"{context}: matrix axis {key!r} must be a non-empty literal string list, found {value!r}")
        axes[str(key)] = value
    keys = list(axes)
    original = [dict(zip(keys, values)) for values in itertools.product(*(axes[key] for key in keys))] if axes else []
    rows = [dict(row) for row in original]
    includes = matrix.get("include", [])
    if isinstance(includes, dict):
        includes = [includes]
    if not isinstance(includes, list):
        raise ValueError(f"{context}: matrix.include must be a list of mappings, found {includes!r}")
    for ordinal, include in enumerate(includes):
        if not isinstance(include, dict) or not all(
            isinstance(value, str) and "${{" not in value for value in include.values()
        ):
            raise ValueError(f"{context}: matrix.include[{ordinal}] must contain literal scalar values, found {include!r}")
        row = {str(key): value for key, value in include.items()}
        compatible = [index for index, base in enumerate(original) if all(key not in base or base[key] == value for key, value in row.items())]
        if compatible:
            for index in compatible:
                rows[index].update(row)
        else:
            rows.append(row)
    excludes = matrix.get("exclude", [])
    if isinstance(excludes, dict):
        excludes = [excludes]
    if not isinstance(excludes, list):
        raise ValueError(f"{context}: matrix.exclude must be a list of mappings, found {excludes!r}")
    for ordinal, exclude in enumerate(excludes):
        if not isinstance(exclude, dict) or not all(
            isinstance(value, str) and "${{" not in value for value in exclude.values()
        ):
            raise ValueError(f"{context}: matrix.exclude[{ordinal}] must contain literal scalar values, found {exclude!r}")
        rows = [row for row in rows if not all(row.get(str(key)) == value for key, value in exclude.items())]
    if not rows:
        raise ValueError(f"{context}: matrix expands to no jobs")
    return True, rows


def expanded_jobs(workflow_name: str, workflow: dict[str, Any]) -> Counter[str]:
    jobs = workflow.get("jobs")
    if not isinstance(jobs, dict):
        raise ValueError(f"{workflow_name}: jobs must be a mapping, found {jobs!r}")
    result: Counter[str] = Counter()
    for job_name, definition in jobs.items():
        context = f"{workflow_name}::{job_name}"
        if not isinstance(definition, dict):
            raise ValueError(f"{context}: job must be a mapping, found {definition!r}")
        condition = definition.get("if")
        if isinstance(condition, str) and condition.strip().lower() in FALSE_CONDITIONS:
            continue
        has_matrix, rows = matrix_rows(definition, context)
        for ordinal, row in enumerate(rows):
            rendered_condition = str(condition).strip() if condition is not None else "<none>"
            result[identity(workflow_name, str(job_name), ordinal if has_matrix else None, condition=rendered_condition, **row)] += 1
    return result


def canonical_trigger_errors(name: str, workflow: dict[str, Any]) -> list[str]:
    expected_pr: dict[str, Any] = {} if name == "repository-hygiene.yml" else {"branches": ["main"]}
    trigger = workflow.get("on")
    expected = {"pull_request": expected_pr, "push": {"branches": ["main"]}}
    normalized = trigger
    if isinstance(trigger, dict):
        normalized = {key: ({} if value in (None, "") else value) for key, value in trigger.items()}
    if normalized == expected:
        return []
    return [f"{name}: trigger must exactly model normal pull_request synchronization and main pushes; expected {expected!r}, found {normalized!r}"]


def exact_job_errors(
    workflow_name: str,
    workflow: dict[str, Any],
    job_name: str,
    expected_job: dict[str, Any],
    expected_steps: tuple[dict[str, Any], ...],
) -> list[str]:
    errors: list[str] = []
    jobs = workflow.get("jobs", {})
    actual = jobs.get(job_name) if isinstance(jobs, dict) else None
    context = f"{workflow_name}::{job_name}"
    if not isinstance(actual, dict):
        return [f"{context}: required critical job is missing or not a mapping: {actual!r}"]
    expected_keys = set(expected_job) | {"steps"}
    actual_keys = set(actual)
    if actual_keys != expected_keys:
        errors.append(f"{context}: critical job keys changed; expected {sorted(expected_keys)!r}, found {sorted(actual_keys)!r}, unsupported={sorted(actual_keys - expected_keys)!r}, missing={sorted(expected_keys - actual_keys)!r}, actual_job={actual!r}")
    for key, value in expected_job.items():
        if actual.get(key) != value:
            errors.append(f"{context}: {key!r} changed; expected {value!r}, found {actual.get(key)!r}")
    steps = actual.get("steps")
    if not isinstance(steps, list):
        return errors + [f"{context}: steps must be an ordered list, found {steps!r}"]
    actual_names = [step.get("name", f"uses:{step.get('uses')}") if isinstance(step, dict) else repr(step) for step in steps]
    expected_names = [step.get("name", f"uses:{step.get('uses')}") for step in expected_steps]
    if actual_names != expected_names:
        errors.append(f"{context}: critical step order changed; expected {expected_names!r}, found {actual_names!r}")
    for index in range(max(len(steps), len(expected_steps))):
        if index >= len(expected_steps):
            errors.append(f"{context}: unexpected critical step[{index}]={steps[index]!r}")
            continue
        if index >= len(steps):
            errors.append(f"{context}: missing critical step[{index}]={expected_steps[index]!r}")
            continue
        expected = expected_steps[index]
        step = steps[index]
        if not isinstance(step, dict):
            errors.append(f"{context}: step[{index}] must be a mapping, found {step!r}")
            continue
        if set(step) != set(expected):
            errors.append(f"{context}: step[{index}] {expected.get('name', expected.get('uses'))!r} keys changed; expected {sorted(expected)!r}, found {sorted(step)!r}, unsupported={sorted(set(step) - set(expected))!r}, missing={sorted(set(expected) - set(step))!r}, actual_step={step!r}")
        for key, value in expected.items():
            actual_value = step.get(key)
            if key == "run" and isinstance(actual_value, str):
                actual_value = actual_value.strip()
            if actual_value != value:
                errors.append(f"{context}: step[{index}] {expected.get('name', expected.get('uses'))!r} key {key!r} changed; expected {value!r}, found {actual_value!r}")
    return errors


def run_steps(workflow: dict[str, Any]) -> list[tuple[str, str, dict[str, Any]]]:
    output: list[tuple[str, str, dict[str, Any]]] = []
    jobs = workflow.get("jobs", {})
    if not isinstance(jobs, dict):
        return output
    for job_name, definition in jobs.items():
        if not isinstance(definition, dict):
            continue
        steps = definition.get("steps", [])
        if not isinstance(steps, list):
            continue
        for index, step in enumerate(steps):
            if isinstance(step, dict):
                output.append((str(job_name), f"step[{index}] {step.get('name', step.get('uses', '<unnamed>'))}", step))
    return output


def audit_errors(workflows: dict[str, dict[str, Any]], ordinary_active: Counter[str]) -> list[str]:
    installs: list[str] = []
    audits: list[str] = []
    active_jobs = {item.split("[", 1)[0] for item in ordinary_active}
    for workflow_name, workflow in workflows.items():
        for job_name, label, step in run_steps(workflow):
            if f"{workflow_name}::{job_name}" not in active_jobs:
                continue
            location = f"{workflow_name}::{job_name}::{label}"
            uses = str(step.get("uses", ""))
            if re.search(r"(?:^|/)audit-check(?:@|$)", uses, re.IGNORECASE):
                audits.append(f"{location} uses={uses!r}")
            run = str(step.get("run", ""))
            install_matches = list(re.finditer(r"\bcargo\s+install\s+cargo-audit\b", run))
            installs.extend(f"{location} run={run!r}" for _ in install_matches)
            without_installs = re.sub(r"\bcargo\s+install\s+cargo-audit\b[^\n;|&]*", "", run)
            invocations = re.findall(
                r"(?:^[ \t]*|[;&|][ \t]*|^[ \t]*(?:bash|sh)[ \t]+-c[ \t]+['\"][ \t]*)"
                r"(?:sudo[ \t]+)?cargo(?:[ \t]+audit|-audit)\b",
                without_installs,
                re.MULTILINE,
            )
            audits.extend(f"{location} run={run!r}" for _ in invocations)
    errors: list[str] = []
    if len(installs) != 1 or not installs[0].startswith("ci.yml::security::step[2]"):
        errors.append(f"ordinary source must activate exactly the canonical cargo-audit install; found {len(installs)} occurrences: {installs!r}")
    if len(audits) != 1 or not audits[0].startswith("ci.yml::security::step[6]"):
        errors.append(f"ordinary source must activate exactly one full cargo audit in canonical security; found {len(audits)} occurrences: {audits!r}")
    return errors


def duplicate_build_errors(workflows: dict[str, dict[str, Any]]) -> list[str]:
    errors: list[str] = []
    for workflow_name, workflow in workflows.items():
        if workflow_name == "ci.yml":
            continue
        try:
            if pull_request_trigger(workflow, workflow_name) is None:
                continue
        except ValueError as error:
            errors.append(str(error))
            continue
        jobs = workflow.get("jobs", {})
        for job_name, label, step in run_steps(workflow):
            run = str(step.get("run", "")).replace("\\\n", " ")
            command_lines = [line for line in run.splitlines() if re.search(r"\bcargo\s+(?:test|build)\b", line)]
            definition = jobs.get(job_name, {}) if isinstance(jobs, dict) else {}
            try:
                has_matrix, rows = matrix_rows(definition, f"{workflow_name}::{job_name}")
                expanded = [
                    identity(workflow_name, job_name, ordinal if has_matrix else None, **row)
                    for ordinal, row in enumerate(rows)
                ]
            except ValueError as error:
                expanded = [f"unexpandable matrix: {error}"]
            for line in command_lines:
                if re.search(r"\bcargo\s+test\b", line) and "--all-targets" in line:
                    errors.append(f"{workflow_name}::{job_name}::{label}: pull-request workflow outside ci.yml runs forbidden duplicate cargo test --all-targets; expanded job/matrix identities={expanded!r}; run={step.get('run')!r}")
                if (
                    workflow_name != "issue-56-brain-isolation.yml"
                    and re.search(r"\bcargo\s+build\b", line)
                    and "--release" in line
                    and re.search(r"--target(?:\s|=)", line)
                ):
                    errors.append(f"{workflow_name}::{job_name}::{label}: pull-request workflow outside ci.yml runs forbidden duplicate cargo build --release --target; expanded job/matrix identities={expanded!r}; run={step.get('run')!r}")
    return errors


def format_counter(counter: Counter[str]) -> list[str]:
    return [f"{item} x{count}" if count != 1 else item for item, count in sorted(counter.items())]


def validate_contract(root: Path) -> list[str]:
    errors: list[str] = []
    workflow_dir = root / WORKFLOWS
    paths = sorted((*workflow_dir.glob("*.yml"), *workflow_dir.glob("*.yaml")))
    workflows: dict[str, dict[str, Any]] = {}
    for path in paths:
        try:
            workflows[path.name] = load_workflow(path)
        except ValueError as error:
            errors.append(str(error))
    for retired in ("issue-185-spreadsheet-advisories", "issue-186-ssh-removal"):
        returned = [name for name in workflows if Path(name).stem == retired]
        if returned:
            counts = []
            for name in returned:
                try:
                    counts.append(f"{name}={expanded_jobs(name, workflows[name]).total()} jobs")
                except ValueError as error:
                    counts.append(f"{name}=unexpandable ({error})")
            errors.append(f"retired closed-issue workflow returned: {returned!r} ({', '.join(counts)}); its permanent checks belong in canonical CI/hygiene")
    for canonical in ("ci.yml", "repository-hygiene.yml"):
        if canonical not in workflows:
            errors.append(f"{canonical}: canonical workflow is missing")
        else:
            errors.extend(canonical_trigger_errors(canonical, workflows[canonical]))

    activations: dict[str, Counter[str]] = {}
    for fixture in FIXTURES:
        actual: Counter[str] = Counter()
        for name, workflow in workflows.items():
            try:
                if activates_on_synchronize(workflow, fixture.paths, name):
                    actual += expanded_jobs(name, workflow)
            except ValueError as error:
                errors.append(f"fixture {fixture.name}: {error}")
        activations[fixture.name] = actual
        if actual != fixture.expected:
            missing = fixture.expected - actual
            unexpected = actual - fixture.expected
            errors.append(
                f"fixture {fixture.name} ({', '.join(fixture.paths)}): expected {fixture.expected.total()} activated jobs, found {actual.total()}; "
                f"missing={format_counter(missing)!r}; unexpected={format_counter(unexpected)!r}; active={format_counter(actual)!r}"
            )

    ci = workflows.get("ci.yml", {})
    hygiene = workflows.get("repository-hygiene.yml", {})
    errors.extend(exact_job_errors(
        "ci.yml", ci, "security",
        {"name": "Security Audit", "runs-on": "ubuntu-24.04"}, SECURITY_STEPS,
    ))
    errors.extend(exact_job_errors(
        "repository-hygiene.yml", hygiene, "tracked-tree",
        {
            "name": "Tracked tree (${{ matrix.os }})",
            "strategy": {"fail-fast": "false", "matrix": {"os": ["ubuntu-24.04", "macos-14"]}},
            "runs-on": "${{ matrix.os }}",
            "timeout-minutes": "5",
        },
        HYGIENE_STEPS,
    ))
    errors.extend(audit_errors(workflows, activations.get("ordinary source", Counter())))
    errors.extend(duplicate_build_errors(workflows))
    return errors


def main() -> int:
    args = parse_args()
    errors = validate_contract(args.root.resolve())
    if errors:
        for error in errors:
            print(f"workflow fan-out: {error}", file=sys.stderr)
        return 1
    print("workflow fan-out: representative PR fixtures satisfy the pinned semantic job contract (ordinary=14, Brain/effect=15)")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
