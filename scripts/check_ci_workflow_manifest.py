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
    "ci.yml", "docs.yml", "issue-104-chooser-catalog.yml", "issue-105-oauth.yml",
    "issue-163-effect-audit.yml", "issue-187-subagent-fanout.yml",
    "issue-201-chatgpt-auth.yml", "issue-227-setup-preservation.yml",
    "issue-245-cargo-slot.yml", "issue-46-atomic-conversation.yml",
    "issue-56-brain-isolation.yml", "issue-72-capability-contract.yml", "release.yml",
    "repository-hygiene.yml",
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
    "issue-105-oauth.yml": (
        "Cargo.toml", "src/lib.rs", "src/oauth/**", "src/providers/chatgpt_oauth.rs",
        "src/providers/mod.rs", ".github/issue-105-windows-probe/**",
        ".github/workflows/issue-105-oauth.yml",
    ),
    "issue-163-effect-audit.yml": (
        ".github/workflows/issue-163-effect-audit.yml", "schema/finch_ipc.capnp",
        "src/brain/effect_audit_archive.rs", "src/brain/mod.rs", "src/brain/remote.rs",
        "src/brain/store.rs", "src/cli/repl_event/**", "src/ipc/**",
        "src/runtime/effect_log.rs", "src/runtime/mod.rs", "src/server/brain_runner.rs",
        "src/server/brain_service.rs", "src/server/handlers.rs", "src/server/mod.rs",
        "src/tools/executor.rs", "src/tools/types.rs",
        "src/tools/implementations/program.rs",
    ),
    "issue-187-subagent-fanout.yml": (
        "src/tools/implementations/spawn.rs",
        ".github/workflows/issue-187-subagent-fanout.yml",
    ),
    "issue-201-chatgpt-auth.yml": (
        "Cargo.toml", "src/oauth/**", "src/config/**", "src/providers/chatgpt_oauth.rs",
        "src/providers/model_catalog.rs", "src/providers/openai_jwks.rs",
        "src/cli/chatgpt_auth.rs", "src/cli/setup_wizard.rs", "src/main.rs", "docs/OAUTH.md",
        ".github/issue-201-windows-probe/**",
        ".github/workflows/issue-201-chatgpt-auth.yml",
    ),
    "issue-227-setup-preservation.yml": (
        "src/config/**", "src/cli/setup_wizard.rs", "src/main.rs",
        ".github/workflows/issue-227-setup-preservation.yml",
    ),
    "issue-245-cargo-slot.yml": (
        ".agents/skills/finch-backlog/**", ".claude/skills/finch-backlog",
        ".github/workflows/issue-245-cargo-slot.yml",
    ),
    "issue-46-atomic-conversation.yml": (
        ".github/workflows/issue-46-atomic-conversation.yml", "src/cli/conversation.rs",
        "src/cli/memtree_console/event_handler.rs", "src/cli/repl_event/**",
        "src/providers/claude.rs",
    ),
    "issue-56-brain-isolation.yml": (
        ".github/workflows/issue-56-brain-isolation.yml", "Cargo.toml", "Cargo.lock",
        "build.rs", "schema/**", "src/**", "scripts/**", "tests/**",
    ),
    "repository-hygiene.yml": None,
}

EXPECTED_CHECKS = {
    "ci.yml": (
        "Build Release (x86_64-unknown-linux-gnu)", "Runtime Authority (Ubuntu)",
        "Security Audit", "Test (macos-14, default)",
        "Test (ubuntu-24.04, default)", "Test (ubuntu-24.04, no-default-features)",
        "Toolchain and formatting contract", "Toolchain and formatting contract (Windows)",
    ),
    "docs.yml": ("Current docs links, claims, and shell syntax",),
    "issue-105-oauth.yml": ("macos-oauth", "oauth", "windows-compile"),
    "issue-163-effect-audit.yml": ("effect-audit",),
    "issue-187-subagent-fanout.yml": (
        "Focused spawn tests (macos-14)", "Focused spawn tests (ubuntu-24.04)",
    ),
    "issue-201-chatgpt-auth.yml": (
        "focused-auth (macos-14)", "focused-auth (ubuntu-24.04)",
        "windows-verifier-compile",
    ),
    "issue-227-setup-preservation.yml": (
        "setup-preservation (macos-14)", "setup-preservation (ubuntu-24.04)",
        "windows-auth-contract",
    ),
    "issue-245-cargo-slot.yml": (
        "macos-latest repository-wide lock", "ubuntu-latest repository-wide lock",
    ),
    "issue-46-atomic-conversation.yml": (
        "atomic-rounds (macos-14)", "atomic-rounds (ubuntu-24.04)",
    ),
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
        "Tracked tree (ubuntu-24.04)", "effect-audit",
    )),
    "manifest_dependency": (("Cargo.toml", "Cargo.lock"), (
        "Build Release (x86_64-unknown-linux-gnu)", "Isolation boundaries (ubuntu-24.04)",
        "Runtime Authority (Ubuntu)", "Security Audit", "Test (macos-14, default)",
        "Test (ubuntu-24.04, default)", "Test (ubuntu-24.04, no-default-features)",
        "Toolchain and formatting contract", "Toolchain and formatting contract (Windows)",
        "Tracked tree (ubuntu-24.04)", "focused-auth (macos-14)",
        "focused-auth (ubuntu-24.04)", "macos-oauth", "oauth", "windows-compile",
        "windows-verifier-compile",
    )),
    "public_api": (("src/lib.rs",), (
        "Build Release (x86_64-unknown-linux-gnu)", "Isolation boundaries (ubuntu-24.04)",
        "Runtime Authority (Ubuntu)", "Security Audit", "Test (macos-14, default)",
        "Test (ubuntu-24.04, default)", "Test (ubuntu-24.04, no-default-features)",
        "Toolchain and formatting contract", "Toolchain and formatting contract (Windows)",
        "Tracked tree (ubuntu-24.04)", "macos-oauth", "oauth", "windows-compile",
    )),
}


class ContractError(Exception):
    pass


def load_yaml(path: Path) -> dict[str, Any]:
    """Use Ruby's stock Psych parser; Python's standard library has no YAML parser."""
    program = (
        "require 'yaml'; require 'json'; "
        "print JSON.generate(YAML.safe_load(STDIN.read, permitted_classes: [], aliases: false))"
    )
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


def pull_request_paths(document: dict[str, Any], display: str) -> tuple[str, ...] | None | bool:
    # Psych implements YAML 1.1 and therefore decodes the plain key `on` as true.
    triggers = document.get("on", document.get("true"))
    if not isinstance(triggers, dict) or "pull_request" not in triggers:
        return False
    pull_request = triggers["pull_request"]
    if pull_request is None:
        return None
    if not isinstance(pull_request, dict):
        raise ContractError(f"{display}: on.pull_request must be a mapping or null")
    unsupported = set(pull_request) - {"branches", "branches-ignore", "types", "paths"}
    if unsupported:
        raise ContractError(f"{display}: unsupported pull_request keys affecting activation: {sorted(unsupported)}")
    paths = pull_request.get("paths")
    if paths is None:
        return None
    if not isinstance(paths, list) or not paths or not all(isinstance(item, str) for item in paths):
        raise ContractError(f"{display}: on.pull_request.paths must be a nonempty string list")
    return tuple(paths)


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


def workflow_activates(paths: tuple[str, ...] | None, changed: tuple[str, ...]) -> bool:
    if paths is None:
        return True
    def path_matches(path: str) -> bool:
        active = False
        for pattern in paths:
            if glob_matches(pattern, path):
                active = not pattern.startswith("!")
        return active

    return any(path_matches(path) for path in changed)


def compare_contract(root: Path) -> list[str]:
    directory = root / WORKFLOWS
    actual_files = tuple(sorted(path.name for path in directory.glob("*.y*ml")))
    errors: list[str] = []
    if actual_files != EXPECTED_WORKFLOWS:
        errors.append(f"workflow inventory changed; expected={EXPECTED_WORKFLOWS!r} actual={actual_files!r}")
    parsed: dict[str, tuple[tuple[str, ...] | None, tuple[str, ...]]] = {}
    for name in sorted(set(actual_files) & set(EXPECTED_WORKFLOWS)):
        display = (WORKFLOWS / name).as_posix()
        try:
            document = load_yaml(directory / name)
            paths = pull_request_paths(document, display)
            if paths is False:
                continue
            checks = expanded_checks(document, display)
            parsed[name] = (paths, checks)
        except ContractError as error:
            errors.append(str(error))
    actual_pr = set(parsed)
    expected_pr = set(EXPECTED_PATHS)
    if actual_pr != expected_pr:
        errors.append(f"PR-active workflow inventory changed; expected={sorted(expected_pr)!r} actual={sorted(actual_pr)!r}")
    for name in sorted(actual_pr & expected_pr):
        paths, checks = parsed[name]
        if paths != EXPECTED_PATHS[name]:
            errors.append(f"{WORKFLOWS / name}: pull_request.paths changed; expected={EXPECTED_PATHS[name]!r} actual={paths!r}")
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
                check for paths, checks in parsed.values() if workflow_activates(paths, changed)
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
