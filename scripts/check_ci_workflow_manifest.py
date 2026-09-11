#!/usr/bin/env python3
"""Check the reviewed semantic shape of Finch pull-request workflows."""

from __future__ import annotations

import argparse
import itertools
import json
import re
import subprocess
import sys
from pathlib import Path
from typing import Any

ROOT = Path(__file__).resolve().parent.parent
WORKFLOWS = Path(".github/workflows")

EXPECTED_WORKFLOWS = (
    "ci.yml", "docs.yml", "issue-201-chatgpt-auth.yml",
    "issue-56-brain-isolation.yml", "release.yml", "repository-hygiene.yml",
)

# Exact triggers are reviewed separately from fixture activation so a path change cannot hide
# merely because none of the representative fixtures exercises it.
EXPECTED_PATHS: dict[str, tuple[str, ...] | None] = {
    "ci.yml": None,
    "docs.yml": (
        "**.md", "scripts/check_docs.py",
        ".agents/skills/finch-backlog/scripts/test-review-protocol",
        ".github/workflows/docs.yml",
    ),
    "issue-201-chatgpt-auth.yml": (
        "Cargo.toml", "src/lib.rs", "src/oauth/**", "src/config/**",
        "src/providers/chatgpt_oauth.rs", "src/providers/openai_jwks.rs",
        "src/providers/model_catalog.rs", "src/providers/mod.rs", "src/cli/chatgpt_auth.rs",
        "src/cli/setup_wizard.rs", "src/main.rs", "docs/OAUTH.md",
        ".github/issue-105-windows-probe/**", ".github/issue-201-windows-probe/**",
        ".github/workflows/issue-201-chatgpt-auth.yml",
    ),
    "issue-56-brain-isolation.yml": (
        ".github/workflows/issue-56-brain-isolation.yml", "Cargo.toml", "Cargo.lock",
        "build.rs", "schema/**", "src/**", "scripts/**", "tests/**",
    ),
    "repository-hygiene.yml": None,
}

EXPECTED_PULL_REQUEST_OPTIONS = {
    name: ({"branches": ("main",)} if name in {
        "ci.yml", "issue-201-chatgpt-auth.yml",
    } else {})
    for name in EXPECTED_PATHS
}

EXPECTED_CHECKS = {
    "ci.yml": (
        "Build Release (x86_64-unknown-linux-gnu)", "Runtime Authority (Ubuntu)",
        "Security Audit", "Test (macos-14, default)",
        "Test (ubuntu-24.04, default)", "Test (ubuntu-24.04, no-default-features)",
        "Toolchain and formatting contract", "Toolchain and formatting contract (Windows)",
    ),
    "docs.yml": ("Current docs links, claims, and shell syntax",),
    "issue-201-chatgpt-auth.yml": ("windows-verifier-compile",),
    "issue-56-brain-isolation.yml": ("Isolation boundaries (ubuntu-24.04)",),
    "repository-hygiene.yml": ("Tracked tree (ubuntu-24.04)",),
}

EXPECTED_FIXTURES = {
    "readme_only": (("README.md",), (
        "Build Release (x86_64-unknown-linux-gnu)",
        "Current docs links, claims, and shell syntax", "Runtime Authority (Ubuntu)",
        "Security Audit", "Test (macos-14, default)", "Test (ubuntu-24.04, default)",
        "Test (ubuntu-24.04, no-default-features)", "Toolchain and formatting contract",
        "Toolchain and formatting contract (Windows)", "Tracked tree (ubuntu-24.04)",
    )),
    "ordinary_source": (("src/models/mod.rs",), (
        "Build Release (x86_64-unknown-linux-gnu)", "Isolation boundaries (ubuntu-24.04)",
        "Runtime Authority (Ubuntu)", "Security Audit", "Test (macos-14, default)",
        "Test (ubuntu-24.04, default)", "Test (ubuntu-24.04, no-default-features)",
        "Toolchain and formatting contract", "Toolchain and formatting contract (Windows)",
        "Tracked tree (ubuntu-24.04)",
    )),
    "brain_effect": (("src/brain/store.rs", "src/server/handlers.rs"), (
        "Build Release (x86_64-unknown-linux-gnu)", "Isolation boundaries (ubuntu-24.04)",
        "Runtime Authority (Ubuntu)", "Security Audit", "Test (macos-14, default)",
        "Test (ubuntu-24.04, default)", "Test (ubuntu-24.04, no-default-features)",
        "Toolchain and formatting contract", "Toolchain and formatting contract (Windows)",
        "Tracked tree (ubuntu-24.04)",
    )),
    "manifest_dependency": (("Cargo.toml", "Cargo.lock"), (
        "Build Release (x86_64-unknown-linux-gnu)", "Isolation boundaries (ubuntu-24.04)",
        "Runtime Authority (Ubuntu)", "Security Audit", "Test (macos-14, default)",
        "Test (ubuntu-24.04, default)", "Test (ubuntu-24.04, no-default-features)",
        "Toolchain and formatting contract", "Toolchain and formatting contract (Windows)",
        "Tracked tree (ubuntu-24.04)", "windows-verifier-compile",
    )),
    "public_api": (("src/lib.rs",), (
        "Build Release (x86_64-unknown-linux-gnu)", "Isolation boundaries (ubuntu-24.04)",
        "Runtime Authority (Ubuntu)", "Security Audit", "Test (macos-14, default)",
        "Test (ubuntu-24.04, default)", "Test (ubuntu-24.04, no-default-features)",
        "Toolchain and formatting contract", "Toolchain and formatting contract (Windows)",
        "Tracked tree (ubuntu-24.04)", "windows-verifier-compile",
    )),
}

RUST_CACHE_ACTION = "Swatinem/rust-cache@6323deb102c322ba6fcbdcafc7e3dddab59af2b6"
GRAPH_HASH = "${{ hashFiles('Cargo.lock', '**/Cargo.toml', 'rust-toolchain.toml', '.cargo/config', '.cargo/config.toml') }}"
MATRIX_CACHE_KEY = (
    "finch-cargo-v5-${{ matrix.os }}-${{ runner.arch }}-${{ matrix.target }}-"
    "rust-1.98.0-${{ matrix.cache_family }}-" + GRAPH_HASH
)
MAIN_SAVE_IF = "${{ github.event_name == 'push' && github.ref == 'refs/heads/main' }}"
COMMON_CACHE_INPUTS = {
    "cache-provider": "github",
    "cache-targets": True,
    "cache-bin": False,
    "cache-workspace-crates": False,
    "cache-all-crates": False,
    "cache-on-failure": False,
}


def literal_cache_key(platform: str, target: str, family: str) -> str:
    return (
        f"finch-cargo-v5-{platform}-${{{{ runner.arch }}}}-{target}-"
        f"rust-1.98.0-{family}-{GRAPH_HASH}"
    )


CACHE_SPECS = {
    ("ci.yml", "test"): {
        "name": "Restore compatible Cargo dependencies and build artifacts",
        "shared-key": MATRIX_CACHE_KEY,
        "save-if": MAIN_SAVE_IF,
        "before": "Run clippy (binary only, warnings allowed for now)",
    },
    ("ci.yml", "runtime-authority"): {
        "name": "Restore Linux default-family Cargo state",
        "shared-key": literal_cache_key(
            "ubuntu-24.04", "x86_64-unknown-linux-gnu",
            "debug-default_all-features-clippy_release-default",
        ),
        "save-if": False,
        "before": "Run runtime authority regressions",
    },
    ("ci.yml", "build"): {
        "name": "Restore compatible Linux release Cargo state",
        "shared-key": literal_cache_key(
            "ubuntu-24.04", "x86_64-unknown-linux-gnu",
            "release-default-lto-false-codegen-units-16",
        ),
        "save-if": MAIN_SAVE_IF,
        "before": "Build release binary",
    },
    ("ci.yml", "security"): {
        "name": "Restore cargo-audit 0.22.2",
        "shared-key": literal_cache_key(
            "ubuntu-24.04", "x86_64-unknown-linux-gnu", "cargo-audit-0.22.2",
        ),
        "save-if": MAIN_SAVE_IF,
        "before": "Install cargo-audit 0.22.2 on cache miss",
        "cache-targets": False,
        "cache-bin": True,
        "id": "cargo-audit-cache",
    },
    ("issue-56-brain-isolation.yml", "isolation-boundaries"): {
        "name": "Restore compatible isolation Cargo state",
        "shared-key": literal_cache_key(
            "ubuntu-24.04", "x86_64-unknown-linux-gnu",
            "debug-default-test-debug-0_supervisor-release",
        ),
        "save-if": MAIN_SAVE_IF,
        "before": "Check bins and tests",
    },
    ("release.yml", "build-release"): {
        "name": "Restore compatible Cargo dependencies and build artifacts",
        "shared-key": MATRIX_CACHE_KEY,
        "save-if": False,
        "before": "Install Linux dependencies",
    },
}


class ContractError(Exception):
    pass


def load_yaml(path: Path) -> dict[str, Any]:
    """Use Ruby's stock Psych parser; Python's standard library has no YAML parser."""
    program = r"""
require 'yaml'; require 'json'
begin
  source = STDIN.read
  stream = Psych.parse_stream(source)
  raise 'workflow must contain exactly one YAML document' unless stream.children.length == 1
  root = stream.children[0].root
  raise 'workflow document must be a mapping' unless root.is_a?(Psych::Nodes::Mapping)
  visit = lambda do |node|
    if node.is_a?(Psych::Nodes::Mapping)
      keys = node.children.each_slice(2).map do |key, value|
        raise "mapping key at line #{key.start_line + 1} must be a scalar" unless key.is_a?(Psych::Nodes::Scalar)
        visit.call(value)
        key
      end
      duplicate = keys.group_by(&:value).find { |_, matches| matches.length > 1 }
      raise "duplicate YAML key #{duplicate[0].inspect} at line #{duplicate[1][1].start_line + 1}" if duplicate
    elsif node.respond_to?(:children) && node.children
      node.children.each { |child| visit.call(child) }
    end
  end
  visit.call(root)
  on_key = root.children.each_slice(2).map(&:first).find { |key| key.value == 'on' }
  raise "workflow root is missing the actual 'on' key" unless on_key
  document = YAML.safe_load(source, permitted_classes: [], aliases: false)
  raise 'YAML key conversion collision is unsupported' unless document.length == root.children.length / 2
  converted_on = on_key.plain ? true : 'on'
  document['on'] = document.delete(converted_on) unless converted_on == 'on'
  print JSON.generate(document)
rescue => error
  warn error.message
  exit 1
end
"""
    try:
        result = subprocess.run(
            ["ruby", "-e", program], input=path.read_text(), text=True,
            capture_output=True, check=False,
        )
    except (OSError, UnicodeError) as error:
        raise ContractError(f"{path}: cannot parse workflow YAML with Ruby Psych: {error}") from error
    if result.returncode:
        raise ContractError(f"{path}: invalid workflow YAML: {result.stderr.strip()}")
    try:
        document = json.loads(result.stdout)
    except json.JSONDecodeError as error:
        raise ContractError(f"{path}: YAML parser returned invalid JSON: {error}") from error
    if not isinstance(document, dict):
        raise ContractError(f"{path}: workflow document must be a mapping")
    return document


def event_contract(
    document: dict[str, Any], display: str, event: str,
) -> dict[str, tuple[str, ...]] | bool:
    triggers = document["on"]
    if not isinstance(triggers, dict):
        raise ContractError(f"{display}: on must be a mapping")
    if "pull_request_target" in triggers:
        raise ContractError(f"{display}: on.pull_request_target is unsupported by the reviewed PR allocation")
    if event not in triggers:
        return False
    options = triggers[event]
    if options is None:
        return {}
    if not isinstance(options, dict):
        raise ContractError(f"{display}: on.{event} must be a mapping or null")
    supported = {"branches", "branches-ignore", "types", "paths", "paths-ignore"}
    unsupported = set(options) - supported
    if unsupported:
        raise ContractError(f"{display}: unsupported {event} keys affecting activation: {sorted(unsupported)}")
    for alternatives in (("branches", "branches-ignore"), ("paths", "paths-ignore")):
        if set(alternatives) <= set(options):
            raise ContractError(f"{display}: on.{event} cannot combine {alternatives[0]} and {alternatives[1]}")
    contract: dict[str, tuple[str, ...]] = {}
    for key, values in options.items():
        if not isinstance(values, list) or not values or not all(isinstance(item, str) for item in values):
            raise ContractError(f"{display}: on.{event}.{key} must be a nonempty string list")
        if len(values) != len(set(values)):
            raise ContractError(f"{display}: on.{event}.{key} contains duplicates")
        contract[key] = tuple(values)
    return contract


def pull_request_contract(document: dict[str, Any], display: str) -> dict[str, tuple[str, ...]] | bool:
    return event_contract(document, display, "pull_request")


MATRIX_REFERENCE = re.compile(r"\$\{\{\s*matrix\.([A-Za-z_][A-Za-z0-9_-]*)\s*\}\}")


def expanded_checks(document: dict[str, Any], display: str) -> tuple[str, ...]:
    jobs = document.get("jobs")
    if not isinstance(jobs, dict) or not jobs:
        raise ContractError(f"{display}: jobs must be a nonempty mapping")
    names: list[str] = []
    for job_id, job in jobs.items():
        if not isinstance(job_id, str) or not isinstance(job, dict):
            raise ContractError(f"{display}: each job must have a string id and mapping body")
        explicit_name = job.get("name")
        if explicit_name is not None and not isinstance(explicit_name, str):
            raise ContractError(f"{display}: job {job_id!r} name must be a string")
        strategy = job.get("strategy", {})
        if not isinstance(strategy, dict):
            raise ContractError(f"{display}: job {job_id!r} strategy must be a mapping")
        matrix = strategy.get("matrix")
        if matrix is None:
            rows = [{}]
        else:
            if not isinstance(matrix, dict) or not matrix:
                raise ContractError(f"{display}: job {job_id!r} matrix must be a nonempty mapping")
            if "exclude" in matrix:
                raise ContractError(f"{display}: job {job_id!r} uses unsupported matrix.exclude allocation syntax")
            axes = [(key, value) for key, value in matrix.items() if key != "include"]
            includes = matrix.get("include")
            if axes and includes is not None:
                raise ContractError(f"{display}: job {job_id!r} mixes axes and matrix.include; allocation is unsupported")
            if includes is not None:
                if not isinstance(includes, list) or not includes or not all(isinstance(row, dict) for row in includes):
                    raise ContractError(f"{display}: job {job_id!r} matrix.include must be a nonempty mapping list")
                rows = includes
            else:
                if not all(isinstance(key, str) and isinstance(values, list) and values for key, values in axes):
                    raise ContractError(f"{display}: job {job_id!r} matrix axes must be nonempty lists")
                keys = [key for key, _ in axes]
                rows = [dict(zip(keys, values)) for values in itertools.product(*(values for _, values in axes))]
        for row in rows:
            if not all(isinstance(key, str) and isinstance(value, (str, int, float, bool)) for key, value in row.items()):
                raise ContractError(f"{display}: job {job_id!r} matrix rows must contain scalar values")
            if explicit_name is None:
                name = job_id if not row else f"{job_id} ({', '.join(map(str, row.values()))})"
            else:
                def replace(match: re.Match[str]) -> str:
                    key = match.group(1)
                    if key not in row:
                        raise ContractError(f"{display}: job {job_id!r} name references missing matrix key {key!r}")
                    return str(row[key]).lower() if isinstance(row[key], bool) else str(row[key])
                name = MATRIX_REFERENCE.sub(replace, explicit_name)
                if "${{" in name:
                    raise ContractError(f"{display}: job {job_id!r} name uses unsupported allocation expression {name!r}")
            names.append(name)
    duplicates = sorted({name for name in names if names.count(name) > 1})
    if duplicates:
        raise ContractError(f"{display}: duplicate expanded check names: {duplicates}")
    return tuple(sorted(names))


def glob_matches(pattern: str, path: str) -> bool:
    source = pattern[1:] if pattern.startswith("!") else pattern
    if not source or any(character in source for character in "[]{}\\"):
        raise ContractError(f"unsupported pull_request.paths pattern {pattern!r}")
    pieces: list[str] = []
    index = 0
    while index < len(source):
        if source[index:index + 3] == "**/":
            pieces.append("(?:.*/)?")
            index += 3
        elif source[index:index + 2] == "**":
            pieces.append(".*")
            index += 2
        elif source[index] == "*":
            pieces.append("[^/]*")
            index += 1
        elif source[index] == "?":
            pieces.append("[^/]")
            index += 1
        else:
            pieces.append(re.escape(source[index]))
            index += 1
    return re.fullmatch("".join(pieces), path) is not None


def workflow_activates(contract: dict[str, tuple[str, ...]], changed: tuple[str, ...]) -> bool:
    paths = contract.get("paths")
    ignored = contract.get("paths-ignore")
    if paths is None and ignored is None:
        return True
    if ignored is not None:
        return any(not any(glob_matches(pattern, path) for pattern in ignored) for path in changed)
    def path_matches(path: str) -> bool:
        active = False
        for pattern in paths:
            if glob_matches(pattern, path):
                active = not pattern.startswith("!")
        return active

    return any(path_matches(path) for path in changed)


def shell_commands(run: Any) -> tuple[str, ...]:
    if not isinstance(run, str):
        return ()
    joined = run.replace("\\\n", " ")
    return tuple(" ".join(line.split()) for line in joined.splitlines() if line.strip())


def active_owner_job_errors(
    documents: dict[str, dict[str, Any]], workflow: str, job_id: str, expected_runner: str,
) -> list[str]:
    job = documents.get(workflow, {}).get("jobs", {}).get(job_id)
    if not isinstance(job, dict):
        return [f"{workflow}: required owner job {job_id!r} is missing"]
    errors: list[str] = []
    if job.get("runs-on") != expected_runner or job.get("if") is not None:
        errors.append(
            f"{workflow}: owner job {job_id!r} must run actively on {expected_runner}"
        )
    if job.get("continue-on-error") not in (None, False):
        errors.append(f"{workflow}: owner job {job_id!r} must gate failure")
    return errors


def required_step_errors(
    documents: dict[str, dict[str, Any]], workflow: str, job_id: str, name: str,
    expected_if: str | None, expected_shell: str | None, commands: tuple[str, ...],
) -> list[str]:
    errors: list[str] = []
    matches: list[tuple[str, str, dict[str, Any]]] = []
    for owner_workflow, document in documents.items():
        jobs = document.get("jobs")
        if not isinstance(jobs, dict):
            continue
        for owner_job, job in jobs.items():
            if not isinstance(job, dict) or not isinstance(job.get("steps"), list):
                continue
            for step in job["steps"]:
                if isinstance(step, dict) and step.get("name") == name:
                    matches.append((owner_workflow, owner_job, step))
    owners = tuple((owner_workflow, owner_job) for owner_workflow, owner_job, _ in matches)
    if len(matches) != 1 or owners != ((workflow, job_id),):
        return [
            f"required step {name!r} must occur exactly once in {workflow}:{job_id}; "
            f"actual={owners!r}"
        ]
    step = matches[0][2]
    if step.get("if") != expected_if:
        errors.append(
            f"{workflow}: step {name!r} condition changed; "
            f"expected={expected_if!r} actual={step.get('if')!r}"
        )
    if step.get("shell") != expected_shell:
        errors.append(
            f"{workflow}: step {name!r} shell changed; "
            f"expected={expected_shell!r} actual={step.get('shell')!r}"
        )
    if step.get("continue-on-error") not in (None, False):
        errors.append(f"{workflow}: step {name!r} must gate failure")
    actual_commands = shell_commands(step.get("run"))
    if actual_commands != commands:
        errors.append(
            f"{workflow}: step {name!r} commands changed; "
            f"expected={commands!r} actual={actual_commands!r}"
        )
    return errors


def step_order_errors(
    documents: dict[str, dict[str, Any]], workflow: str, job_id: str,
    earlier_names: tuple[str, ...], before_name: str,
) -> list[str]:
    """Require unique preflight steps to precede the named expensive setup boundary."""
    job = documents.get(workflow, {}).get("jobs", {}).get(job_id)
    if not isinstance(job, dict) or not isinstance(job.get("steps"), list):
        return []
    positions: dict[str, list[int]] = {}
    for index, step in enumerate(job["steps"]):
        if isinstance(step, dict) and isinstance(step.get("name"), str):
            positions.setdefault(step["name"], []).append(index)
    required = (*earlier_names, before_name)
    if any(len(positions.get(name, ())) != 1 for name in required):
        return []  # Missing/duplicate steps already have more specific diagnostics.
    boundary = positions[before_name][0]
    late = tuple(name for name in earlier_names if positions[name][0] >= boundary)
    if not late:
        return []
    return [
        f"{workflow}: job {job_id!r} preflight steps must precede {before_name!r}; "
        f"late={late!r}"
    ]


def cache_action(uses: Any) -> bool:
    if not isinstance(uses, str):
        return False
    lowered = uses.lower()
    return (
        lowered.startswith("actions/cache@")
        or "rust-cache@" in lowered
        or "sccache-action@" in lowered
    )


def relevant_rust_env(value: Any) -> dict[str, Any]:
    if not isinstance(value, dict):
        return {}
    prefixes = ("CARGO", "CC", "CFLAGS", "CXX", "CMAKE", "RUST")
    return {key: item for key, item in value.items() if isinstance(key, str) and key.startswith(prefixes)}


def cache_contract_errors(documents: dict[str, dict[str, Any]]) -> list[str]:
    """Check the small, explicit cache allocation; do not interpret arbitrary Actions code."""
    errors: list[str] = []
    found: dict[tuple[str, str], list[tuple[int, dict[str, Any]]]] = {}
    for workflow, document in documents.items():
        jobs = document.get("jobs", {})
        if not isinstance(jobs, dict):
            continue
        for job_id, job in jobs.items():
            if not isinstance(job, dict) or not isinstance(job.get("steps"), list):
                continue
            for index, step in enumerate(job["steps"]):
                if isinstance(step, dict) and cache_action(step.get("uses")):
                    found.setdefault((workflow, job_id), []).append((index, step))

    expected_locations = set(CACHE_SPECS)
    actual_locations = set(found)
    if actual_locations != expected_locations:
        errors.append(
            "Cargo cache allocation changed; "
            f"missing={sorted(expected_locations - actual_locations)!r} "
            f"unexpected={sorted(actual_locations - expected_locations)!r}"
        )

    for location in sorted(expected_locations & actual_locations):
        workflow, job_id = location
        matches = found[location]
        if len(matches) != 1:
            errors.append(
                f"{workflow}: job {job_id!r} must contain exactly one Cargo cache step; "
                f"actual={len(matches)}"
            )
            continue
        cache_index, step = matches[0]
        spec = CACHE_SPECS[location]
        if step.get("name") != spec["name"]:
            errors.append(
                f"{workflow}: job {job_id!r} cache step name changed; "
                f"expected={spec['name']!r} actual={step.get('name')!r}"
            )
        if step.get("uses") != RUST_CACHE_ACTION:
            errors.append(
                f"{workflow}: job {job_id!r} must pin the reviewed rust-cache action; "
                f"actual={step.get('uses')!r}"
            )
        if step.get("continue-on-error") is not True:
            errors.append(f"{workflow}: job {job_id!r} cache failure must remain nonfatal")
        if step.get("if") is not None:
            errors.append(f"{workflow}: job {job_id!r} cache step must run actively")
        expected_inputs = dict(COMMON_CACHE_INPUTS)
        expected_inputs.update({
            "shared-key": spec["shared-key"],
            "save-if": spec["save-if"],
        })
        for optional in ("cache-targets", "cache-bin"):
            if optional in spec:
                expected_inputs[optional] = spec[optional]
        actual_inputs = step.get("with")
        if isinstance(actual_inputs, dict) and actual_inputs.get("add-job-id-key") is False:
            actual_inputs = {key: value for key, value in actual_inputs.items() if key != "add-job-id-key"}
        if actual_inputs != expected_inputs:
            errors.append(
                f"{workflow}: job {job_id!r} cache inputs changed; "
                f"expected={expected_inputs!r} actual={actual_inputs!r}"
            )
        if step.get("id") != spec.get("id"):
            errors.append(
                f"{workflow}: job {job_id!r} cache id changed; "
                f"expected={spec.get('id')!r} actual={step.get('id')!r}"
            )

        job = documents[workflow]["jobs"][job_id]
        steps = job["steps"]
        toolchains = [
            index for index, candidate in enumerate(steps)
            if isinstance(candidate, dict)
            and candidate.get("uses") == "dtolnay/rust-toolchain@1.98.0"
        ]
        boundaries = [
            index for index, candidate in enumerate(steps)
            if isinstance(candidate, dict) and candidate.get("name") == spec["before"]
        ]
        if len(toolchains) != 1 or len(boundaries) != 1:
            errors.append(
                f"{workflow}: job {job_id!r} cache ordering boundary is ambiguous; "
                f"toolchains={toolchains!r} before={boundaries!r}"
            )
        elif not toolchains[0] < cache_index < boundaries[0]:
            errors.append(
                f"{workflow}: job {job_id!r} cache must run after the pinned toolchain "
                f"and before {spec['before']!r}"
            )
        if relevant_rust_env(job.get("env")) or relevant_rust_env(step.get("env")):
            errors.append(
                f"{workflow}: job {job_id!r} must not override action-hashed Rust/Cargo "
                "environment at the cache boundary"
            )

    expected_modes = {
        ("ci.yml", "runtime-authority"): "read",
        ("release.yml", "build-release"): "read",
        ("release.yml", "create-release"): "none",
    }
    for workflow, document in documents.items():
        jobs = document.get("jobs", {})
        if not isinstance(jobs, dict):
            continue
        for job_id, job in jobs.items():
            if not isinstance(job, dict):
                continue
            expected_mode = expected_modes.get((workflow, job_id))
            actual_mode = job.get("cache-mode")
            if actual_mode != expected_mode:
                errors.append(
                    f"{workflow}: job {job_id!r} cache-mode changed; "
                    f"expected={expected_mode!r} actual={actual_mode!r}"
                )

    ci_env = relevant_rust_env(documents.get("ci.yml", {}).get("env"))
    release_env = relevant_rust_env(documents.get("release.yml", {}).get("env"))
    expected_shared_env = {
        "CARGO_TERM_COLOR": "always",
        "RUST_BACKTRACE": 1,
        "CARGO_BUILD_JOBS": 1,
        "CARGO_PROFILE_RELEASE_LTO": "false",
        "CARGO_PROFILE_RELEASE_CODEGEN_UNITS": "16",
    }
    if ci_env != expected_shared_env or release_env != expected_shared_env:
        errors.append(
            "ci.yml and release.yml must retain identical action-hashed Cargo/Rust "
            f"environment; expected={expected_shared_env!r} ci={ci_env!r} release={release_env!r}"
        )
    expected_isolation_env = {
        "CARGO_BUILD_JOBS": 1,
        "CARGO_PROFILE_TEST_DEBUG": 0,
        "CARGO_TERM_COLOR": "always",
        "RUST_BACKTRACE": 1,
    }
    isolation_env = relevant_rust_env(
        documents.get("issue-56-brain-isolation.yml", {}).get("env")
    )
    if isolation_env != expected_isolation_env:
        errors.append(
            "issue-56-brain-isolation.yml cache family environment changed; "
            f"expected={expected_isolation_env!r} actual={isolation_env!r}"
        )

    expected_test_matrix = [
        {
            "os": "ubuntu-24.04", "target": "x86_64-unknown-linux-gnu",
            "feature_name": "default",
            "cache_family": "debug-default_all-features-clippy_release-default",
            "cargo_args": "", "timeout_minutes": 45,
        },
        {
            "os": "ubuntu-24.04", "target": "x86_64-unknown-linux-gnu",
            "feature_name": "no-default-features",
            "cache_family": "debug-no-default-features",
            "cargo_args": "--no-default-features", "timeout_minutes": 45,
        },
        {
            "os": "macos-14", "target": "aarch64-apple-darwin",
            "feature_name": "default",
            "cache_family": "debug-default_supervisor-release_apple-release-default",
            "cargo_args": "", "timeout_minutes": 120,
        },
    ]
    actual_test_matrix = (
        documents.get("ci.yml", {}).get("jobs", {}).get("test", {})
        .get("strategy", {}).get("matrix", {}).get("include")
    )
    if actual_test_matrix != expected_test_matrix:
        errors.append(
            "ci.yml: test cache compatibility matrix changed; "
            f"expected={expected_test_matrix!r} actual={actual_test_matrix!r}"
        )

    expected_release_matrix = [
        {
            "os": "macos-14", "target": "aarch64-apple-darwin",
            "asset_name": "finch-macos-arm64",
            "cache_family": "debug-default_supervisor-release_apple-release-default",
        },
        {
            "os": "ubuntu-24.04", "target": "x86_64-unknown-linux-gnu",
            "asset_name": "finch-linux-x86_64",
            "cache_family": "release-default-lto-false-codegen-units-16",
        },
    ]
    actual_release_matrix = (
        documents.get("release.yml", {}).get("jobs", {}).get("build-release", {})
        .get("strategy", {}).get("matrix", {}).get("include")
    )
    if actual_release_matrix != expected_release_matrix:
        errors.append(
            "release.yml: build-release cache compatibility matrix changed; "
            f"expected={expected_release_matrix!r} actual={actual_release_matrix!r}"
        )

    release = documents.get("release.yml", {})
    if "permissions" in release:
        errors.append("release.yml: workflow-wide permissions must remain absent")
    release_jobs = release.get("jobs", {})
    for job_id, expected in (
        ("build-release", {"contents": "read"}),
        ("create-release", {"contents": "write"}),
    ):
        actual = release_jobs.get(job_id, {}).get("permissions")
        if actual != expected:
            errors.append(
                f"release.yml: job {job_id!r} permissions changed; "
                f"expected={expected!r} actual={actual!r}"
            )

    errors.extend(required_step_errors(
        documents, "ci.yml", "security", "Install cargo-audit 0.22.2 on cache miss",
        "steps.cargo-audit-cache.outputs.cache-hit != 'true'", None,
        ("cargo install cargo-audit --version 0.22.2 --locked",),
    ))
    errors.extend(required_step_errors(
        documents, "ci.yml", "security", "Verify cargo-audit 0.22.2",
        None, None,
        ('test "$(cargo audit --version)" = "cargo-audit 0.22.2"',),
    ))
    return errors


def migrated_boundary_errors(documents: dict[str, dict[str, Any]]) -> list[str]:
    errors: list[str] = []
    errors.extend(active_owner_job_errors(documents, "ci.yml", "test", "${{ matrix.os }}"))

    errors.extend(required_step_errors(
        documents, "ci.yml", "test", "Prove validated request tokens cannot be forged",
        "runner.os == 'Linux' && matrix.feature_name == 'default'", None,
        ("cargo test --doc -- ValidatedProviderRequest",),
    ))
    errors.extend(required_step_errors(
        documents, "ci.yml", "test", "Run release-mode atomic history regression",
        "runner.os == 'Linux' && matrix.feature_name == 'default'", None,
        ("cargo test --release --lib cli::conversation::tests -- --nocapture",),
    ))
    errors.extend(required_step_errors(
        documents, "ci.yml", "test", "Verify shared skill discovery",
        "matrix.feature_name == 'default'", "bash", (
            "test -L .claude/skills/finch-backlog",
            'test "$(cd .agents/skills/finch-backlog && pwd -P)" = "$(cd .claude/skills/finch-backlog && pwd -P)"',
        ),
    ))
    errors.extend(required_step_errors(
        documents, "ci.yml", "test", "Check and exercise the Cargo slot",
        "matrix.feature_name == 'default'", "bash", (
            "bash -n .agents/skills/finch-backlog/scripts/with-cargo-slot .agents/skills/finch-backlog/scripts/test-with-cargo-slot",
            ".agents/skills/finch-backlog/scripts/test-with-cargo-slot",
        ),
    ))
    errors.extend(step_order_errors(
        documents, "ci.yml", "test", (
            "Verify shared skill discovery", "Check and exercise the Cargo slot",
        ), "Install repository Rust toolchain",
    ))

    auth = documents.get("issue-201-chatgpt-auth.yml", {})
    jobs = auth.get("jobs")
    if not isinstance(jobs, dict) or tuple(jobs) != ("windows-verifier-compile",):
        errors.append("issue-201-chatgpt-auth.yml: exactly one Windows verifier job is required")
    else:
        job = jobs["windows-verifier-compile"]
        if not isinstance(job, dict):
            errors.append("issue-201-chatgpt-auth.yml: Windows verifier job must be a mapping")
    errors.extend(active_owner_job_errors(
        documents, "issue-201-chatgpt-auth.yml", "windows-verifier-compile", "windows-2022",
    ))
    errors.extend(required_step_errors(
        documents, "issue-201-chatgpt-auth.yml", "windows-verifier-compile",
        "Compile exact authentication sources on Windows", None, None, (
            "cargo check --manifest-path .github/issue-105-windows-probe/Cargo.toml",
            "cargo check --manifest-path .github/issue-201-windows-probe/Cargo.toml",
        ),
    ))

    try:
        push = event_contract(auth, ".github/workflows/issue-201-chatgpt-auth.yml", "push")
    except ContractError as error:
        errors.append(str(error))
    else:
        expected = {"branches": ("main",), "paths": EXPECTED_PATHS["issue-201-chatgpt-auth.yml"]}
        if push is False:
            errors.append("issue-201-chatgpt-auth.yml: path-filtered push to main is required")
        else:
            for key in sorted(set(push) | set(expected)):
                actual_value = push.get(key)
                expected_value = expected.get(key)
                equal = (
                    set(actual_value or ()) == set(expected_value or ())
                    if key == "paths" else actual_value == expected_value
                )
                if not equal:
                    errors.append(
                        f"issue-201-chatgpt-auth.yml: push.{key} changed; "
                        f"expected={expected_value!r} actual={actual_value!r}"
                    )
    return errors


def compare_contract(root: Path) -> list[str]:
    directory = root / WORKFLOWS
    actual_files = tuple(sorted(path.name for path in directory.glob("*.y*ml")))
    errors: list[str] = []
    if actual_files != EXPECTED_WORKFLOWS:
        errors.append(f"workflow inventory changed; expected={EXPECTED_WORKFLOWS!r} actual={actual_files!r}")
    parsed: dict[str, tuple[dict[str, tuple[str, ...]], tuple[str, ...]]] = {}
    documents: dict[str, dict[str, Any]] = {}
    for name in sorted(set(actual_files) & set(EXPECTED_WORKFLOWS)):
        display = (WORKFLOWS / name).as_posix()
        try:
            document = load_yaml(directory / name)
            documents[name] = document
            contract = pull_request_contract(document, display)
            if contract is False:
                continue
            checks = expanded_checks(document, display)
            parsed[name] = (contract, checks)
        except ContractError as error:
            errors.append(str(error))
    actual_pr = set(parsed)
    expected_pr = set(EXPECTED_PATHS)
    if actual_pr != expected_pr:
        errors.append(f"PR-active workflow inventory changed; expected={sorted(expected_pr)!r} actual={sorted(actual_pr)!r}")
    for name in sorted(actual_pr & expected_pr):
        contract, checks = parsed[name]
        expected_contract = dict(EXPECTED_PULL_REQUEST_OPTIONS[name])
        if EXPECTED_PATHS[name] is not None:
            expected_contract["paths"] = EXPECTED_PATHS[name]
        for key in sorted(set(contract) | set(expected_contract)):
            actual_value = contract.get(key)
            expected_value = expected_contract.get(key)
            equal = (
                set(actual_value or ()) == set(expected_value or ())
                if key == "paths" else actual_value == expected_value
            )
            if not equal:
                errors.append(
                    f"{WORKFLOWS / name}: pull_request.{key} changed; "
                    f"expected={expected_value!r} actual={actual_value!r}"
                )
        expected = tuple(sorted(EXPECTED_CHECKS[name]))
        if checks != expected:
            errors.append(f"{WORKFLOWS / name}: expanded check allocation changed; expected={expected!r} actual={checks!r}")
    all_checks = [check for _, checks in parsed.values() for check in checks]
    duplicates = sorted({name for name in all_checks if all_checks.count(name) > 1})
    if duplicates:
        errors.append(f"PR-active workflows have duplicate expanded check names: {duplicates}")
    for fixture, (changed, expected) in EXPECTED_FIXTURES.items():
        try:
            actual = tuple(sorted(
                check for contract, checks in parsed.values() if workflow_activates(contract, changed)
                for check in checks
            ))
        except ContractError as error:
            errors.append(f"fixture {fixture!r}: {error}")
            continue
        duplicates = sorted({name for name in actual if actual.count(name) > 1})
        if duplicates:
            errors.append(f"fixture {fixture!r}: duplicate check names: {duplicates}")
        wanted = tuple(sorted(expected))
        if actual != wanted:
            errors.append(
                f"fixture {fixture!r}: expected check names/count changed; "
                f"expected_count={len(wanted)} actual_count={len(actual)} "
                f"missing={sorted(set(wanted) - set(actual))!r} "
                f"unexpected={sorted(set(actual) - set(wanted))!r}"
            )
    errors.extend(migrated_boundary_errors(documents))
    errors.extend(cache_contract_errors(documents))
    return errors


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--root", type=Path, default=ROOT)
    arguments = parser.parse_args()
    errors = compare_contract(arguments.root.resolve())
    if errors:
        for error in errors:
            print(f"CI workflow contract: {error}", file=sys.stderr)
        return 1
    print("CI workflow contract: semantic inventory and check allocation passed")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
