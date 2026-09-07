#!/usr/bin/env python3
"""Validate Finch's bounded, trusted-producer Cargo download caches."""

from __future__ import annotations

import argparse
import re
import shlex
import sys
from dataclasses import dataclass
from pathlib import Path


WORKFLOWS = (".github/workflows/ci.yml", ".github/workflows/release.yml")
KNOWN_CONSUMERS = {
    ".github/workflows/ci.yml": {"test", "runtime-authority", "build", "security"},
    ".github/workflows/release.yml": {"build-release"},
}
PRODUCERS = {"cargo-download-cache", "cargo-audit-cache"}
TRUSTED_MAIN = "github.event_name == 'push' && github.ref == 'refs/heads/main'"

RESOLUTION_PATHS = {"~/.cargo/registry/index"}
RESOLUTION_KEY = (
    "cargo-resolution-v1-${{ runner.os }}-rust-1.98.0-config-"
    "${{ hashFiles('rust-toolchain.toml', '.cargo/config', '.cargo/config.toml') }}-"
    "manifests-${{ hashFiles('**/Cargo.lock', '**/Cargo.toml') }}"
)
RESOLUTION_RESTORE_KEY = (
    "cargo-resolution-v1-${{ runner.os }}-rust-1.98.0-config-"
    "${{ hashFiles('rust-toolchain.toml', '.cargo/config', '.cargo/config.toml') }}-"
    "manifests-"
)
DOWNLOAD_PATHS = {"~/.cargo/registry/cache", "~/.cargo/git/db"}
DOWNLOAD_KEY = (
    "cargo-downloads-v3-${{ runner.os }}-rust-1.98.0-config-"
    "${{ hashFiles('rust-toolchain.toml', '.cargo/config', '.cargo/config.toml') }}-"
    "lock-${{ hashFiles('**/Cargo.lock') }}"
)
DOWNLOAD_RESTORE_KEY = (
    "cargo-downloads-v3-${{ runner.os }}-rust-1.98.0-config-"
    "${{ hashFiles('rust-toolchain.toml', '.cargo/config', '.cargo/config.toml') }}-"
    "lock-"
)
AUDIT_PATHS = {
    "~/.cargo/bin/cargo-audit",
    "~/.cargo/.crates.toml",
    "~/.cargo/.crates2.json",
}
AUDIT_KEY = (
    "cargo-tool-v1-${{ runner.os }}-ubuntu-24.04-rust-1.98.0-cargo-audit-0.22.2"
)

EXPENSIVE_SUBCOMMANDS = {
    "bench",
    "build",
    "check",
    "clippy",
    "doc",
    "install",
    "run",
    "rustc",
    "test",
}
CARGO_OPTIONS_WITH_VALUES = {
    "--color",
    "--config",
    "--jobs",
    "--manifest-path",
    "--target-dir",
    "-j",
    "-Z",
}


@dataclass(frozen=True)
class Step:
    start: int
    uses: str
    condition: str
    values: dict[str, str]
    raw: str


def indentation(line: str) -> int:
    return len(line) - len(line.lstrip(" "))


def job_blocks(contents: str) -> dict[str, str]:
    """Parse top-level jobs from Finch's constrained workflow YAML shape."""
    lines = contents.splitlines()
    jobs_at = next((i for i, line in enumerate(lines) if line == "jobs:"), None)
    if jobs_at is None:
        return {}
    starts = [
        (match.group(1), i)
        for i, line in enumerate(lines[jobs_at + 1 :], jobs_at + 1)
        if (match := re.fullmatch(r"  ([A-Za-z0-9_-]+):", line))
    ]
    return {
        name: "\n".join(
            lines[start : starts[i + 1][1] if i + 1 < len(starts) else len(lines)]
        )
        + "\n"
        for i, (name, start) in enumerate(starts)
    }


def raw_step_blocks(job: str) -> list[tuple[int, str]]:
    lines = job.splitlines(keepends=True)
    offsets: list[int] = []
    offset = 0
    for line in lines:
        if re.match(r"^\s+-\s+(?:name|uses|run):", line):
            offsets.append(offset)
        offset += len(line)
    return [
        (start, job[start : offsets[i + 1] if i + 1 < len(offsets) else len(job)])
        for i, start in enumerate(offsets)
    ]


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
    for start, block in raw_step_blocks(job):
        uses = scalar(block, "uses")
        if not uses.startswith("actions/cache"):
            continue
        result.append(
            Step(
                start=start,
                uses=uses,
                condition=scalar(block, "if"),
                values={
                    name: with_value(block, name)
                    for name in ("path", "key", "restore-keys")
                },
                raw=block,
            )
        )
    return result


def paths(step: Step) -> set[str]:
    return {line for line in step.values["path"].splitlines() if line}


def masked_job(job: str) -> str:
    return "\n".join(
        " " * len(line) if line.lstrip().startswith("#") else line
        for line in job.splitlines()
    )


def expensive_position(job: str) -> int | None:
    offset = 0
    for line in masked_job(job).splitlines(keepends=True):
        for match in re.finditer(r"\bcargo\b", line):
            try:
                tokens = shlex.split(line[match.end() :], comments=False)
            except ValueError:
                continue
            index = 0
            if index < len(tokens) and tokens[index].startswith("+"):
                index += 1
            while index < len(tokens) and tokens[index].startswith("-"):
                option = tokens[index].split("=", 1)[0]
                has_inline_value = "=" in tokens[index]
                index += 1
                if option in CARGO_OPTIONS_WITH_VALUES and not has_inline_value:
                    index += 1
            if index < len(tokens) and tokens[index] in EXPENSIVE_SUBCOMMANDS:
                return offset + match.start()
        offset += len(line)
    return None


def exact_run_position(job: str, command: str) -> int | None:
    match = re.search(rf"^\s+run:\s*{re.escape(command)}\s*$", job, re.MULTILINE)
    return match.start() if match else None


def location(workflow: str, job: str) -> str:
    return f"{workflow} job '{job}'"


def validate_cache_step(step: Step, where: str, errors: list[str]) -> None:
    if step.uses not in ("actions/cache/restore@v4", "actions/cache/save@v4"):
        errors.append(
            f"{where} must use explicit actions/cache/restore@v4 or save@v4; found {step.uses!r}"
        )
    if not re.search(r"^\s+continue-on-error:\s*true\s*$", step.raw, re.MULTILINE):
        errors.append(f"{where} cache failures must not prevent the real Cargo command")
    if re.search(r"^\s+fail-on-cache-miss:\s*true\s*$", step.raw, re.MULTILINE):
        errors.append(f"{where} must treat a cache miss as an ordinary uncached build")
    cached = paths(step)
    forbidden = sorted(
        path
        for path in cached
        if re.search(r"(^|/)\.?target($|/)", path)
        or "registry/src" in path
        or "git/checkouts" in path
        or "ort.pyke.io" in path
        or ("/.cargo/bin" in path and cached != AUDIT_PATHS)
    )
    if forbidden:
        errors.append(
            f"{where} caches quota-heavy or executable/native output {forbidden!r}; only bounded download archives are authorized"
        )


def download_restores(job: str) -> list[Step]:
    return [
        step
        for step in cache_steps(job)
        if step.uses == "actions/cache/restore@v4" and paths(step) == DOWNLOAD_PATHS
    ]


def resolution_restores(job: str) -> list[Step]:
    return [
        step
        for step in cache_steps(job)
        if step.uses == "actions/cache/restore@v4" and paths(step) == RESOLUTION_PATHS
    ]


def audit_restores(job: str) -> list[Step]:
    return [
        step
        for step in cache_steps(job)
        if step.uses == "actions/cache/restore@v4" and paths(step) == AUDIT_PATHS
    ]


def validate_download_restore(step: Step, where: str, errors: list[str]) -> None:
    if step.values["key"] != DOWNLOAD_KEY:
        errors.append(
            f"{where} resolved-download key must bind OS, Rust, both Cargo config names, and the generated Cargo.lock; found {step.values['key']!r}"
        )
    if step.values["restore-keys"] != DOWNLOAD_RESTORE_KEY:
        errors.append(
            f"{where} resolved-download fallback may vary only the generated lock fingerprint; found {step.values['restore-keys']!r}"
        )


def validate_resolution_restore(step: Step, where: str, errors: list[str]) -> None:
    if step.values["key"] != RESOLUTION_KEY:
        errors.append(
            f"{where} resolution key must bind OS, Rust, both Cargo config names, Cargo.lock, and every Cargo.toml; found {step.values['key']!r}"
        )
    if step.values["restore-keys"] != RESOLUTION_RESTORE_KEY:
        errors.append(
            f"{where} resolution fallback may vary only the manifest/lock fingerprint; found {step.values['restore-keys']!r}"
        )


def validate_consumer(workflow: str, name: str, job: str, errors: list[str]) -> None:
    where = location(workflow, name)
    compile_at = expensive_position(job)
    if compile_at is None:
        errors.append(f"{where} is declared expensive but no compiling Cargo command was found")
        return

    resolutions = resolution_restores(job)
    downloads = download_restores(job)
    if len(resolutions) != 1:
        errors.append(
            f"{where} needs exactly one registry-index bootstrap before lock resolution; found {len(resolutions)}"
        )
        return
    if len(downloads) != 1:
        errors.append(
            f"{where} needs exactly one resolved Cargo download restore before compilation; found {len(downloads)}"
        )
        return
    resolution, download = resolutions[0], downloads[0]
    validate_resolution_restore(resolution, where, errors)
    validate_download_restore(download, where, errors)
    if download.start > compile_at:
        errors.append(f"{where} restores resolved Cargo downloads after compilation starts")

    resolve_at = exact_run_position(job, "cargo generate-lockfile")
    if resolve_at is None:
        errors.append(f"{where} must resolve the untracked Cargo.lock explicitly")
    elif not (resolution.start < resolve_at < download.start < compile_at):
        errors.append(
            f"{where} must restore the index, resolve Cargo.lock, restore that exact download layer, then compile"
        )

    if name != "security":
        if len(cache_steps(job)) != 2:
            errors.append(
                f"{where} may contain only index and resolved-download restores"
            )
        return
    tools = audit_restores(job)
    if len(tools) != 1:
        errors.append(
            f"{where} needs exactly one restore of pinned cargo-audit; found {len(tools)}"
        )
        return
    tool = tools[0]
    if tool.values["key"] != AUDIT_KEY:
        errors.append(f"{where} cargo-audit cache key must pin reviewed version 0.22.2")
    if tool.start > compile_at:
        errors.append(f"{where} restores cargo-audit after fallback compilation starts")
    required = (
        "if: steps.cargo-audit.outputs.cache-hit != 'true'",
        "cargo install cargo-audit --version 0.22.2 --locked",
        "actual=$(cargo-audit --version)",
        'if [[ "$actual" != "cargo-audit 0.22.2" ]]',
    )
    for fragment in required:
        if fragment not in job:
            errors.append(
                f"{where} is missing pinned cargo-audit fallback/verification {fragment!r}"
            )
    if len(cache_steps(job)) != 3:
        errors.append(
            f"{where} may contain only index, resolved-download, and pinned-tool restores"
        )


def validate_download_producer(job: str, errors: list[str]) -> None:
    where = location(".github/workflows/ci.yml", "cargo-download-cache")
    if scalar(job, "if") != TRUSTED_MAIN:
        errors.append(f"{where} must run only for a trusted main push")
    if "os: [ubuntu-24.04, macos-14]" not in job:
        errors.append(
            f"{where} must produce exactly one Linux and one macOS download key"
        )
    resolutions = resolution_restores(job)
    downloads = download_restores(job)
    saves = [step for step in cache_steps(job) if step.uses == "actions/cache/save@v4"]
    resolution_saves = [step for step in saves if paths(step) == RESOLUTION_PATHS]
    download_saves = [step for step in saves if paths(step) == DOWNLOAD_PATHS]
    resolve_at = exact_run_position(job, "cargo generate-lockfile")
    fetch_at = exact_run_position(job, "cargo fetch --verbose")
    if (
        len(resolutions) != 1
        or len(downloads) != 1
        or len(resolution_saves) != 1
        or len(download_saves) != 1
        or len(saves) != 2
        or len(cache_steps(job)) != 4
        or resolve_at is None
        or fetch_at is None
    ):
        errors.append(
            f"{where} needs index restore, lock resolution, exact-download restore, all-target fetch, and one trusted save per layer"
        )
        return
    resolution, download = resolutions[0], downloads[0]
    resolution_save, download_save = resolution_saves[0], download_saves[0]
    validate_resolution_restore(resolution, where, errors)
    validate_download_restore(download, where, errors)
    if resolution_save.values["key"] != "${{ steps.cargo-resolution.outputs.cache-primary-key }}":
        errors.append(f"{where} index save must reuse the bootstrap restore's primary key")
    if download_save.values["key"] != "${{ steps.cargo-downloads.outputs.cache-primary-key }}":
        errors.append(f"{where} save must reuse the restore action's immutable primary key")
    for save in (resolution_save, download_save):
        if TRUSTED_MAIN not in save.condition or "cache-hit != 'true'" not in save.condition:
            errors.append(f"{where} save must require a miss and an explicit trusted main push")
    if not (
        resolution.start
        < resolve_at
        < download.start
        < fetch_at
        < resolution_save.start
        < download_save.start
    ):
        errors.append(
            f"{where} must restore index, resolve, restore exact downloads, fetch completely, then save"
        )


def validate_audit_producer(job: str, errors: list[str]) -> None:
    where = location(".github/workflows/ci.yml", "cargo-audit-cache")
    if scalar(job, "if") != TRUSTED_MAIN:
        errors.append(f"{where} must run only for a trusted main push")
    tools = audit_restores(job)
    saves = [step for step in cache_steps(job) if step.uses == "actions/cache/save@v4"]
    install_at = exact_run_position(
        job, "cargo install cargo-audit --version 0.22.2 --locked"
    )
    if (
        len(tools) != 1
        or len(saves) != 1
        or len(cache_steps(job)) != 2
        or install_at is None
    ):
        errors.append(f"{where} needs one pinned-tool restore, fallback install, and one trusted save")
        return
    tool, save = tools[0], saves[0]
    if tool.values["key"] != AUDIT_KEY:
        errors.append(f"{where} must pin cargo-audit 0.22.2 in its cache key")
    if paths(save) != AUDIT_PATHS:
        errors.append(f"{where} may save only the pinned cargo-audit binary and Cargo metadata")
    if save.values["key"] != "${{ steps.cargo-audit.outputs.cache-primary-key }}":
        errors.append(f"{where} save must reuse the pinned restore action's primary key")
    if TRUSTED_MAIN not in save.condition or "cache-hit != 'true'" not in save.condition:
        errors.append(f"{where} save must require a miss and an explicit trusted main push")
    if not (tool.start < install_at < save.start):
        errors.append(f"{where} must restore, install/verify on miss, then save")
    if (
        "actual=$(cargo-audit --version)" not in job
        or 'if [[ "$actual" != "cargo-audit 0.22.2" ]]' not in job
    ):
        errors.append(f"{where} must verify the direct cached executable is version 0.22.2")


def validate(root: Path) -> list[str]:
    errors: list[str] = []
    parsed: dict[str, dict[str, str]] = {}
    for workflow in WORKFLOWS:
        path = root / workflow
        if not path.exists():
            errors.append(f"{workflow} is missing")
            continue
        parsed[workflow] = job_blocks(path.read_text(encoding="utf-8"))
        for name, job in parsed[workflow].items():
            where = location(workflow, name)
            for step in cache_steps(job):
                validate_cache_step(step, where, errors)
                if step.uses == "actions/cache/save@v4" and (
                    workflow != ".github/workflows/ci.yml" or name not in PRODUCERS
                ):
                    errors.append(f"{where} is a consumer and must be restore-only")

    for workflow, expected in KNOWN_CONSUMERS.items():
        jobs = parsed.get(workflow, {})
        for name in expected:
            if name not in jobs:
                errors.append(f"{location(workflow, name)} is missing")
        discovered = {
            name
            for name, job in jobs.items()
            if name not in PRODUCERS and expensive_position(job) is not None
        }
        for name in sorted(discovered - expected):
            errors.append(
                f"{location(workflow, name)} newly compiles with Cargo; add it to the explicit cache-consumer contract"
            )
        for name in sorted(expected & jobs.keys()):
            validate_consumer(workflow, name, jobs[name], errors)

    ci_jobs = parsed.get(".github/workflows/ci.yml", {})
    if "cargo-download-cache" in ci_jobs:
        validate_download_producer(ci_jobs["cargo-download-cache"], errors)
    else:
        errors.append(".github/workflows/ci.yml job 'cargo-download-cache' is missing")
    if "cargo-audit-cache" in ci_jobs:
        validate_audit_producer(ci_jobs["cargo-audit-cache"], errors)
    else:
        errors.append(".github/workflows/ci.yml job 'cargo-audit-cache' is missing")
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
    print(
        "CI cache contract passed: 5 bounded keys per dependency generation; older generations are quota-LRU; PRs restore-only"
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
