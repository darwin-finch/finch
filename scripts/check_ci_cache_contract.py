#!/usr/bin/env python3
"""Validate cache coverage and compatibility keys in Finch's canonical CI."""

from __future__ import annotations

import argparse
import re
import sys
from dataclasses import dataclass
from pathlib import Path


CANONICAL_JOBS = {
    ".github/workflows/ci.yml": {
        "test": (
            "${{ matrix.target }}",
            "profile-dev-test",
            "features-${{ matrix.cache_feature_set }}",
        ),
        "runtime-authority": (
            "x86_64-unknown-linux-gnu",
            "profile-test",
            "features-default",
        ),
        "build": ("${{ matrix.target }}", "profile-release", "features-default"),
    },
    ".github/workflows/release.yml": {
        "build-release": (
            "${{ matrix.target }}",
            "profile-release",
            "features-default",
        ),
    },
}

DEPENDENCY_PATHS = {"~/.cargo/registry", "~/.cargo/git"}
TARGET_PATHS = {"target/**/.fingerprint", "target/**/build", "target/**/deps"}
DEPENDENCY_KEY_PARTS = (
    "cargo-deps-v1",
    "${{ runner.os }}",
    "rust-1.98.0",
    "${{ hashFiles('**/Cargo.lock', '**/Cargo.toml') }}",
)
DEPENDENCY_HASH = "${{ hashFiles('**/Cargo.lock', '**/Cargo.toml') }}"
TARGET_CARGO = re.compile(
    r"\bcargo(?:\s+\+\S+)?(?:\s+--\S+)*\s+(?:bench|build|check|clippy|doc|run|rustc|test)\b"
)
INSTALL_CARGO = re.compile(r"\bcargo(?:\s+\+\S+)?(?:\s+--\S+)*\s+install\b")


@dataclass(frozen=True)
class Step:
    name: str
    uses: str
    values: dict[str, str]
    raw: str


def indentation(line: str) -> int:
    return len(line) - len(line.lstrip(" "))


def job_blocks(contents: str) -> dict[str, str]:
    """Return top-level jobs from the constrained GitHub workflow YAML shape."""
    lines = contents.splitlines()
    jobs_line = next(
        (index for index, line in enumerate(lines) if line == "jobs:"), None
    )
    if jobs_line is None:
        return {}

    starts: list[tuple[str, int]] = []
    for index in range(jobs_line + 1, len(lines)):
        line = lines[index]
        if line and indentation(line) == 0 and not line.startswith("#"):
            break
        match = re.fullmatch(r"  ([A-Za-z0-9_-]+):", line)
        if match:
            starts.append((match.group(1), index))

    result: dict[str, str] = {}
    for position, (name, start) in enumerate(starts):
        end = starts[position + 1][1] if position + 1 < len(starts) else len(lines)
        result[name] = "\n".join(lines[start:end]) + "\n"
    return result


def step_blocks(job: str) -> list[str]:
    lines = job.splitlines()
    starts: list[tuple[int, int]] = []
    for index, line in enumerate(lines):
        match = re.match(r"^(\s*)-\s+(?:name|uses|run):", line)
        if match:
            starts.append((index, len(match.group(1))))

    result: list[str] = []
    for position, (start, step_indent) in enumerate(starts):
        end = len(lines)
        for candidate, candidate_indent in starts[position + 1 :]:
            if candidate_indent == step_indent:
                end = candidate
                break
        result.append("\n".join(lines[start:end]) + "\n")
    return result


def scalar(block: str, name: str) -> str:
    match = re.search(rf"^\s+{re.escape(name)}:\s*(.*?)\s*$", block, re.MULTILINE)
    return match.group(1) if match else ""


def with_value(block: str, name: str) -> str:
    lines = block.splitlines()
    for index, line in enumerate(lines):
        match = re.match(rf"^(\s*){re.escape(name)}:\s*(.*?)\s*$", line)
        if not match:
            continue
        value = match.group(2)
        if value not in ("|", ">", "|-"):
            return value
        field_indent = len(match.group(1))
        values: list[str] = []
        for continuation in lines[index + 1 :]:
            if continuation.strip() and indentation(continuation) <= field_indent:
                break
            if continuation.strip():
                values.append(continuation.strip())
        return "\n".join(values)
    return ""


def cache_steps(job: str) -> list[Step]:
    result: list[Step] = []
    for block in step_blocks(job):
        uses = scalar(block, "uses")
        if not uses.startswith("actions/cache@"):
            continue
        result.append(
            Step(
                name=scalar(block, "name") or "unnamed cache step",
                uses=uses,
                values={
                    "path": with_value(block, "path"),
                    "key": with_value(block, "key"),
                    "restore-keys": with_value(block, "restore-keys"),
                },
                raw=block,
            )
        )
    return result


def path_set(step: Step) -> set[str]:
    return {line.strip() for line in step.values["path"].splitlines() if line.strip()}


def describe(path: str, job: str) -> str:
    return f"{path} job '{job}'"


def cargo_position(job: str, pattern: re.Pattern[str]) -> int | None:
    masked = "\n".join(
        " " * len(line) if line.lstrip().startswith("#") else line
        for line in job.splitlines()
    )
    match = pattern.search(masked)
    return match.start() if match else None


def validate_lockfile_before_cache(job: str, location: str, errors: list[str]) -> None:
    resolve = re.search(
        r"^\s+run:\s*cargo generate-lockfile\s*$", job, re.MULTILINE
    )
    resolve_at = resolve.start() if resolve else -1
    cache_at = job.find("uses: actions/cache@v4")
    if resolve_at == -1:
        errors.append(
            f"{location} must run 'cargo generate-lockfile' because Cargo.lock is untracked and cache keys hash the resolved graph"
        )
    elif cache_at != -1 and resolve_at > cache_at:
        errors.append(
            f"{location} must resolve Cargo.lock before its first cache key is evaluated"
        )


def validate_cache_step(step: Step, location: str, errors: list[str]) -> None:
    if step.uses != "actions/cache@v4":
        errors.append(f"{location} must use actions/cache@v4; found {step.uses!r}")
    if not re.search(r"^\s+continue-on-error:\s*true\s*$", step.raw, re.MULTILINE):
        errors.append(
            f"{location} must set continue-on-error: true so a cache outage cannot hide Cargo results"
        )
    if re.search(r"^\s+fail-on-cache-miss:\s*true\s*$", step.raw, re.MULTILINE):
        errors.append(f"{location} must not make an ordinary cache miss fail the job")


def validate_dependency_cache(step: Step, location: str, errors: list[str]) -> None:
    validate_cache_step(step, location, errors)
    paths = path_set(step)
    if paths != DEPENDENCY_PATHS:
        errors.append(
            f"{location} dependency cache paths must be {sorted(DEPENDENCY_PATHS)!r}; found {sorted(paths)!r}"
        )
    key = step.values["key"]
    for part in DEPENDENCY_KEY_PARTS:
        if part not in key:
            errors.append(
                f"{location} dependency key is missing compatibility part {part!r}: {key!r}"
            )
    expected_restore = "cargo-deps-v1-${{ runner.os }}-rust-1.98.0-"
    if step.values["restore-keys"] != expected_restore:
        errors.append(
            f"{location} dependency restore key must be {expected_restore!r}; found {step.values['restore-keys']!r}"
        )


def validate_target_cache(
    step: Step,
    location: str,
    workflow: str,
    dimensions: tuple[str, str, str],
    errors: list[str],
) -> None:
    validate_cache_step(step, location, errors)
    paths = path_set(step)
    if paths != TARGET_PATHS:
        errors.append(
            f"{location} target cache must contain only reusable fingerprints, build outputs, and deps; found {sorted(paths)!r}"
        )

    target, profile, features = dimensions
    config_hash = (
        "${{ hashFiles('rust-toolchain.toml', '.cargo/config.toml', "
        f"'Cargo.toml', '{workflow}') }}}}"
    )
    key = step.values["key"]
    required = (
        "cargo-target-v1",
        "${{ runner.os }}",
        target,
        "rust-1.98.0",
        profile,
        features,
        config_hash,
        DEPENDENCY_HASH,
    )
    for part in required:
        if part not in key:
            errors.append(
                f"{location} target key is missing compatibility part {part!r}: {key!r}"
            )

    restore = step.values["restore-keys"]
    for part in required[:-1]:
        if part not in restore:
            errors.append(
                f"{location} target restore key is missing compatibility part {part!r}: {restore!r}"
            )
    if not restore.endswith("-deps-"):
        errors.append(
            f"{location} target restore key must end in '-deps-' so only the dependency fingerprint may fall back: {restore!r}"
        )


def validate(root: Path) -> list[str]:
    errors: list[str] = []
    ci_path = root / ".github/workflows/ci.yml"
    ci_contents = ci_path.read_text(encoding="utf-8") if ci_path.exists() else ""
    if re.search(r"^\s*pull_request_target\s*:", ci_contents, re.MULTILINE):
        errors.append(
            ".github/workflows/ci.yml must not use pull_request_target with restored build outputs; ordinary pull_request cache scope protects main from fork writes"
        )

    for relative, required_jobs in CANONICAL_JOBS.items():
        path = root / relative
        if not path.exists():
            errors.append(f"{relative} is missing")
            continue
        contents = path.read_text(encoding="utf-8")
        if not re.search(r"^  CARGO_INCREMENTAL:\s*0\s*$", contents, re.MULTILINE):
            errors.append(
                f"{relative} must set CARGO_INCREMENTAL: 0 so invocation-local incremental state is not cached"
            )
        jobs = job_blocks(contents)
        for job_name in required_jobs:
            location = describe(relative, job_name)
            if job_name not in jobs:
                errors.append(f"{location} is missing")

        for job_name, job in jobs.items():
            target_at = cargo_position(job, TARGET_CARGO)
            install_at = cargo_position(job, INSTALL_CARGO)
            if target_at is None and install_at is None:
                continue

            location = describe(relative, job_name)
            if target_at is not None and job_name not in required_jobs:
                errors.append(
                    f"{location} compiles Finch with Cargo but has no declared target/profile/feature compatibility dimensions"
                )
            if relative == ".github/workflows/ci.yml" and job_name == "test":
                for value in (
                    "cache_feature_set: default-and-all-features-clippy",
                    "cache_feature_set: default",
                    "cache_feature_set: no-default-features",
                ):
                    if value not in job:
                        errors.append(
                            f"{location} matrix is missing actual compiled feature-set identity {value!r}"
                        )
            validate_lockfile_before_cache(job, location, errors)
            caches = cache_steps(job)
            dependency = [step for step in caches if path_set(step) & DEPENDENCY_PATHS]
            targets = [step for step in caches if path_set(step) & TARGET_PATHS]
            if len(dependency) != 1:
                errors.append(
                    f"{location} needs exactly one Cargo dependency cache; found {len(dependency)}"
                )
            else:
                validate_dependency_cache(dependency[0], location, errors)

            first_cargo_at = min(
                position for position in (target_at, install_at) if position is not None
            )
            first_cache_at = job.find("uses: actions/cache@v4")
            if first_cache_at == -1 or first_cache_at > first_cargo_at:
                errors.append(
                    f"{location} must restore dependency caches before its first expensive Cargo command"
                )

            if target_at is None:
                continue
            if len(targets) != 1:
                errors.append(
                    f"{location} needs exactly one compatible target-artifact cache; found {len(targets)}"
                )
            else:
                dimensions = required_jobs.get(job_name)
                if dimensions is not None:
                    validate_target_cache(
                        targets[0], location, relative, dimensions, errors
                    )

    return errors


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--root", type=Path, default=Path(__file__).resolve().parent.parent
    )
    args = parser.parse_args()
    errors = validate(args.root)
    if errors:
        print("CI cache contract failed:", file=sys.stderr)
        for error in errors:
            print(f"- {error}", file=sys.stderr)
        return 1
    print("CI cache contract passed for canonical Cargo jobs")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
