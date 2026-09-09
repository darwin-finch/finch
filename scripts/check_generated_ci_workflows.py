#!/usr/bin/env python3
"""Render and verify Finch's bounded generated CI workflow catalog."""

from __future__ import annotations

import argparse
import ast
import json
import os
import re
import stat
import subprocess
import sys
from pathlib import Path
from typing import Any


CATALOG = (
    (
        "tests/fixtures/ci-generated-workflows/superseded-run-envelope.json",
        "tests/fixtures/ci-generated-workflows/superseded-run-envelope.yml",
    ),
)
FIXTURE_DIRECTORY = "tests/fixtures/ci-generated-workflows"
MAX_SOURCE_BYTES = 64 * 1024
MAX_OUTPUT_BYTES = 128 * 1024
MAX_TOOLCHAIN_BYTES = 64 * 1024
MAX_ATTRIBUTES_BYTES = 16 * 1024
REQUIRED_ATTRIBUTES = (
    "tests/fixtures/ci-generated-workflows/*.json text eol=lf",
    "tests/fixtures/ci-generated-workflows/*.yml text eol=lf",
)
CONDITION = (
    "${{ github.event_name == 'workflow_dispatch' || github.event.action == 'requested' "
    "|| github.event.workflow_run.run_attempt > 1 }}"
)
SAFE_IDENTIFIER = re.compile(r"[A-Za-z_][A-Za-z0-9_-]{0,63}\Z")
SAFE_JOB_IDENTIFIER = re.compile(r"[a-z][a-z0-9-]{0,62}\Z")
SAFE_ENV_IDENTIFIER = re.compile(r"[A-Z][A-Z0-9_]{0,63}\Z")
SAFE_WORKFLOW_NAME = re.compile(r"[A-Za-z0-9][A-Za-z0-9 ._()/-]{0,127}\Z")


class ContractError(RuntimeError):
    """A generated-workflow source or output violated the closed contract."""


def exact_keys(value: Any, expected: set[str], label: str) -> dict[str, Any]:
    if not isinstance(value, dict):
        raise ContractError(f"{label} must be an object; found {type(value).__name__}")
    actual = set(value)
    if actual != expected:
        raise ContractError(
            f"{label} keys must be exactly {sorted(expected)!r}; "
            f"missing={sorted(expected - actual)!r} unknown={sorted(actual - expected)!r}"
        )
    return value


def bounded_string(value: Any, label: str, *, maximum: int = 256) -> str:
    if not isinstance(value, str):
        raise ContractError(f"{label} must be a string; found {type(value).__name__}")
    if not value or len(value.encode("utf-8")) > maximum:
        raise ContractError(f"{label} must contain 1..{maximum} UTF-8 bytes")
    if "\r" in value or "\0" in value or any(ord(character) < 0x20 for character in value):
        raise ContractError(f"{label} contains a forbidden control character")
    if "${{" in value or "}}" in value:
        raise ContractError(f"{label} contains a forbidden GitHub expression delimiter")
    return value


def safe_string(value: Any, label: str, pattern: re.Pattern[str], *, maximum: int = 256) -> str:
    result = bounded_string(value, label, maximum=maximum)
    if pattern.fullmatch(result) is None:
        raise ContractError(f"{label} has a value outside its bounded character policy: {result!r}")
    return result


def duplicate_rejecting_object(pairs: list[tuple[str, Any]]) -> dict[str, Any]:
    result: dict[str, Any] = {}
    for key, value in pairs:
        if key in result:
            raise ContractError(f"JSON contains duplicate key {key!r}")
        result[key] = value
    return result


def reject_noninteger_number(value: str) -> None:
    raise ContractError(f"JSON numeric value must be an integer; found {value!r}")


def canonical_json(value: Any) -> bytes:
    return (
        json.dumps(value, ensure_ascii=False, allow_nan=False, indent=2, sort_keys=True) + "\n"
    ).encode("utf-8")


def validate_model(value: Any, source: str) -> dict[str, Any]:
    root = exact_keys(value, {"schema_version", "workflow"}, source)
    if root["schema_version"] != 1 or isinstance(root["schema_version"], bool):
        raise ContractError(f"{source}.schema_version must be the integer 1")

    workflow = exact_keys(
        root["workflow"],
        {
            "condition_policy",
            "dispatch_inputs",
            "environment",
            "job_id",
            "job_name",
            "name",
            "permissions",
            "runner",
            "script",
            "timeout_minutes",
            "workflow_run",
        },
        f"{source}.workflow",
    )
    safe_string(workflow["name"], f"{source}.workflow.name", SAFE_WORKFLOW_NAME)
    safe_string(workflow["job_id"], f"{source}.workflow.job_id", SAFE_JOB_IDENTIFIER)
    safe_string(workflow["job_name"], f"{source}.workflow.job_name", SAFE_WORKFLOW_NAME)
    if workflow["runner"] != "ubuntu-24.04":
        raise ContractError(f"{source}.workflow.runner must be exactly 'ubuntu-24.04'")
    timeout = workflow["timeout_minutes"]
    if isinstance(timeout, bool) or not isinstance(timeout, int) or not 1 <= timeout <= 5:
        raise ContractError(f"{source}.workflow.timeout_minutes must be an integer from 1 through 5")
    if workflow["condition_policy"] != "workflow-run-or-bounded-continuation-v1":
        raise ContractError(
            f"{source}.workflow.condition_policy must be "
            "'workflow-run-or-bounded-continuation-v1'; arbitrary expressions are forbidden"
        )

    run_trigger = exact_keys(
        workflow["workflow_run"], {"types", "workflows"}, f"{source}.workflow.workflow_run"
    )
    if run_trigger["workflows"] != ["CI"]:
        raise ContractError(f"{source}.workflow.workflow_run.workflows must be exactly ['CI']")
    if run_trigger["types"] != ["requested", "in_progress"]:
        raise ContractError(
            f"{source}.workflow.workflow_run.types must be exactly ['requested', 'in_progress']"
        )

    permissions = exact_keys(
        workflow["permissions"], {"actions", "pull-requests"}, f"{source}.workflow.permissions"
    )
    if permissions != {"actions": "write", "pull-requests": "read"}:
        raise ContractError(
            f"{source}.workflow.permissions must be exactly actions:write and pull-requests:read"
        )

    dispatch_inputs = workflow["dispatch_inputs"]
    if not isinstance(dispatch_inputs, dict) or set(dispatch_inputs) != {"continuation_cursor"}:
        raise ContractError(
            f"{source}.workflow.dispatch_inputs must declare exactly 'continuation_cursor'"
        )
    input_name = next(iter(dispatch_inputs))
    safe_string(input_name, f"{source}.workflow.dispatch_inputs key", SAFE_IDENTIFIER)
    descriptor = exact_keys(
        dispatch_inputs[input_name],
        {"default", "description", "required", "type"},
        f"{source}.workflow.dispatch_inputs.{input_name}",
    )
    bounded_string(
        descriptor["description"],
        f"{source}.workflow.dispatch_inputs.{input_name}.description",
        maximum=160,
    )
    if descriptor["required"] is not False:
        raise ContractError(f"{source}.workflow.dispatch_inputs.{input_name}.required must be false")
    if descriptor["default"] != "" or descriptor["type"] != "string":
        raise ContractError(
            f"{source}.workflow.dispatch_inputs.{input_name} must be an optional string with empty default"
        )

    environment = workflow["environment"]
    if not isinstance(environment, dict) or not 1 <= len(environment) <= 8:
        raise ContractError(f"{source}.workflow.environment must contain 1..8 entries")
    allowed_sources = {"github.token", f"inputs.{input_name}"}
    used_sources: set[str] = set()
    for key, item in environment.items():
        safe_string(key, f"{source}.workflow.environment key", SAFE_ENV_IDENTIFIER)
        if item not in allowed_sources:
            raise ContractError(
                f"{source}.workflow.environment.{key} must be github.token or a declared input; "
                f"found {item!r}"
            )
        used_sources.add(item)
    if used_sources != allowed_sources:
        raise ContractError(
            f"{source}.workflow.environment must expose exactly github.token and inputs.{input_name}"
        )

    script = exact_keys(workflow["script"], {"body", "name"}, f"{source}.workflow.script")
    safe_string(script["name"], f"{source}.workflow.script.name", SAFE_WORKFLOW_NAME)
    body = script["body"]
    if not isinstance(body, str):
        raise ContractError(f"{source}.workflow.script.body must be a string")
    if not body.endswith("\n") or "\r" in body or "\0" in body:
        raise ContractError(f"{source}.workflow.script.body must be NUL-free LF text ending in LF")
    if "${{" in body or "}}" in body:
        raise ContractError(
            f"{source}.workflow.script.body contains a forbidden GitHub expression delimiter"
        )
    if len(body.encode("utf-8")) > 48 * 1024:
        raise ContractError(f"{source}.workflow.script.body exceeds the 48 KiB bound")
    if any(line == "PYTHON" for line in body.splitlines()):
        raise ContractError(f"{source}.workflow.script.body may not contain the fixed heredoc terminator")
    try:
        ast.parse(body, filename=source, mode="exec")
    except SyntaxError as error:
        raise ContractError(f"{source}.workflow.script.body is not valid Python: {error}") from error
    return root


def quote(value: str) -> str:
    return json.dumps(value, ensure_ascii=False, allow_nan=False)


def render(model: dict[str, Any]) -> bytes:
    workflow = model["workflow"]
    trigger = workflow["workflow_run"]
    input_name, descriptor = next(iter(workflow["dispatch_inputs"].items()))
    lines = [
        f"name: {quote(workflow['name'])}",
        "",
        "on:",
        "  workflow_run:",
        "    workflows:",
    ]
    lines.extend(f"      - {quote(item)}" for item in trigger["workflows"])
    lines.append("    types:")
    lines.extend(f"      - {quote(item)}" for item in trigger["types"])
    lines.extend(
        [
            "  workflow_dispatch:",
            "    inputs:",
            f"      {quote(input_name)}:",
            f"        description: {quote(descriptor['description'])}",
            "        required: false",
            f"        default: {quote(descriptor['default'])}",
            "        type: string",
            "",
            "permissions:",
            "  actions: write",
            "  pull-requests: read",
            "",
            "jobs:",
            f"  {quote(workflow['job_id'])}:",
            f"    name: {quote(workflow['job_name'])}",
            f"    if: {CONDITION}",
            "    runs-on: ubuntu-24.04",
            f"    timeout-minutes: {workflow['timeout_minutes']}",
            "    steps:",
            f"      - name: {quote(workflow['script']['name'])}",
            "        env:",
        ]
    )
    for key, source in workflow["environment"].items():
        lines.append(f"          {quote(key)}: ${{{{ {source} }}}}")
    lines.extend(["        run: |", "          python3 - <<'PYTHON'"])
    for body_line in workflow["script"]["body"].split("\n")[:-1]:
        lines.append(f"          {body_line}" if body_line else "")
    lines.append("          PYTHON")
    return ("\n".join(lines) + "\n").encode("utf-8")


def safe_read(
    root: Path,
    relative: str,
    maximum: int,
    label: str,
    *,
    require_lf_utf8: bool = True,
) -> bytes:
    path = root / relative
    current = root
    for component in Path(relative).parts[:-1]:
        current = current / component
        try:
            metadata = current.lstat()
        except OSError as error:
            raise ContractError(f"{label} parent cannot be inspected: path={current}: {error}") from error
        if stat.S_ISLNK(metadata.st_mode) or not stat.S_ISDIR(metadata.st_mode):
            raise ContractError(f"{label} parent must be a real directory, not a symlink: path={current}")
    try:
        before = path.lstat()
    except OSError as error:
        raise ContractError(f"{label} cannot be inspected: path={path}: {error}") from error
    if stat.S_ISLNK(before.st_mode) or not stat.S_ISREG(before.st_mode):
        raise ContractError(f"{label} must be a regular non-symlink file: path={path}")
    nofollow = getattr(os, "O_NOFOLLOW", None)
    if not isinstance(nofollow, int) or nofollow == 0:
        raise ContractError(f"{label} cannot be opened safely because O_NOFOLLOW is unavailable")
    flags = os.O_RDONLY | getattr(os, "O_CLOEXEC", 0) | nofollow
    try:
        descriptor = os.open(path, flags)
    except OSError as error:
        raise ContractError(f"{label} cannot be opened without following symlinks: path={path}: {error}") from error
    try:
        opened = os.fstat(descriptor)
        if not stat.S_ISREG(opened.st_mode) or (opened.st_dev, opened.st_ino) != (
            before.st_dev,
            before.st_ino,
        ):
            raise ContractError(f"{label} changed identity while opening: path={path}")
        if opened.st_size > maximum:
            raise ContractError(
                f"{label} exceeds its {maximum}-byte bound: path={path} size={opened.st_size}"
            )
        opened_identity = (
            opened.st_dev,
            opened.st_ino,
            opened.st_size,
            opened.st_mtime_ns,
            opened.st_ctime_ns,
        )
        chunks: list[bytes] = []
        remaining = maximum + 1
        while remaining:
            chunk = os.read(descriptor, min(65536, remaining))
            if not chunk:
                break
            chunks.append(chunk)
            remaining -= len(chunk)
        data = b"".join(chunks)
        after = os.fstat(descriptor)
        after_identity = (
            after.st_dev,
            after.st_ino,
            after.st_size,
            after.st_mtime_ns,
            after.st_ctime_ns,
        )
        if after_identity != opened_identity or len(data) != opened.st_size:
            raise ContractError(
                f"{label} changed while it was being read: path={path} "
                f"before={opened_identity!r} after={after_identity!r} bytes_read={len(data)}"
            )
    finally:
        os.close(descriptor)
    if len(data) > maximum:
        raise ContractError(f"{label} exceeds its {maximum}-byte bound: path={path} size>{maximum}")
    if require_lf_utf8:
        if b"\0" in data:
            raise ContractError(f"{label} contains a forbidden NUL byte: path={path}")
        if b"\r" in data:
            raise ContractError(f"{label} must use LF line endings and contains CR: path={path}")
        try:
            data.decode("utf-8")
        except UnicodeDecodeError as error:
            raise ContractError(f"{label} is not UTF-8: path={path}: {error}") from error
    return data


def load_model(root: Path, relative: str) -> dict[str, Any]:
    raw = safe_read(root, relative, MAX_SOURCE_BYTES, "generated workflow source")
    try:
        value = json.loads(
            raw,
            object_pairs_hook=duplicate_rejecting_object,
            parse_float=reject_noninteger_number,
            parse_constant=reject_noninteger_number,
        )
    except UnicodeDecodeError:
        raise
    except (json.JSONDecodeError, ContractError) as error:
        raise ContractError(f"generated workflow source is not strict JSON: path={relative}: {error}") from error
    model = validate_model(value, relative)
    expected = canonical_json(model)
    if raw != expected:
        raise ContractError(
            f"generated workflow source is not canonical JSON: path={relative}; "
            "use sorted keys, two-space indentation, and exactly one trailing LF"
        )
    return model


def verify_catalog(root: Path) -> None:
    expected_paths = {path for pair in CATALOG for path in pair}
    if len(CATALOG) != 1 or len(expected_paths) != 2:
        raise ContractError("internal generated workflow catalog must contain exactly one source/output pair")
    directory = root / FIXTURE_DIRECTORY
    try:
        metadata = directory.lstat()
    except OSError as error:
        raise ContractError(f"generated workflow fixture catalog cannot be inspected: {directory}: {error}") from error
    if stat.S_ISLNK(metadata.st_mode) or not stat.S_ISDIR(metadata.st_mode):
        raise ContractError(f"generated workflow fixture catalog must be a real directory: {directory}")
    actual_paths: set[str] = set()
    with os.scandir(directory) as entries:
        for entry in entries:
            actual_paths.add(str(Path(FIXTURE_DIRECTORY) / entry.name))
            if len(actual_paths) > len(expected_paths):
                raise ContractError(
                    "generated workflow fixture catalog path/count drifted: "
                    f"expected_count={len(expected_paths)} actual_count_at_least={len(actual_paths)} "
                    f"unknown_sample={sorted(actual_paths - expected_paths)!r}"
                )
    if actual_paths != expected_paths:
        raise ContractError(
            "generated workflow fixture catalog path/count drifted: "
            f"expected_count=2 actual_count={len(actual_paths)} "
            f"missing={sorted(expected_paths - actual_paths)!r} "
            f"unknown={sorted(actual_paths - expected_paths)!r}"
        )


def verify_attributes(root: Path) -> None:
    raw = safe_read(root, ".gitattributes", MAX_ATTRIBUTES_BYTES, "Git attributes contract")
    lines = raw.decode("utf-8").splitlines()
    counts = {line: lines.count(line) for line in REQUIRED_ATTRIBUTES}
    if any(count != 1 for count in counts.values()):
        raise ContractError(
            "Git attributes must contain exactly one LF rule for each generated fixture extension: "
            f"required={REQUIRED_ATTRIBUTES!r} counts={counts!r}"
        )
    fixture_paths = [path for pair in CATALOG for path in pair]
    try:
        result = subprocess.run(
            ["git", "-C", str(root), "check-attr", "-z", "text", "eol", "--", *fixture_paths],
            capture_output=True,
            timeout=10,
            check=False,
        )
    except (OSError, subprocess.TimeoutExpired) as error:
        raise ContractError(f"Git attributes effective-rule check could not run: {error}") from error
    if result.returncode != 0:
        raise ContractError(
            "Git attributes effective-rule check failed: "
            f"status={result.returncode} stderr={result.stderr[:4096]!r}"
        )
    fields = result.stdout.split(b"\0")
    if fields[-1:] == [b""]:
        fields.pop()
    expected_fields: list[bytes] = []
    for path in fixture_paths:
        encoded = path.encode("utf-8")
        expected_fields.extend((encoded, b"text", b"set", encoded, b"eol", b"lf"))
    if fields != expected_fields:
        raise ContractError(
            "Git attributes effective rules must keep generated fixture bytes as text=set eol=lf: "
            f"expected={expected_fields!r} actual={fields!r}"
        )


def verify_toolchain_wiring(root: Path) -> None:
    relative = "tests/toolchain_contract.sh"
    raw = safe_read(root, relative, MAX_TOOLCHAIN_BYTES, "canonical toolchain gate")
    lines = raw.decode("utf-8").splitlines()
    test_line = "python3 scripts/test_generated_ci_workflows.py"
    check_line = "python3 scripts/check_generated_ci_workflows.py"
    metadata_line = "cargo metadata --locked --no-deps --format-version 1 >/dev/null"
    if lines.count(test_line) != 1 or lines.count(check_line) != 1 or lines.count(metadata_line) != 1:
        raise ContractError(
            "canonical toolchain gate must invoke the generated-workflow test and checker exactly once "
            f"before its single Cargo metadata command: test_count={lines.count(test_line)} "
            f"check_count={lines.count(check_line)} metadata_count={lines.count(metadata_line)}"
        )
    metadata_index = lines.index(metadata_line)
    if lines.index(test_line) >= metadata_index or lines.index(check_line) >= metadata_index:
        raise ContractError(
            "canonical toolchain gate must run generated-workflow tests and verification before Cargo metadata"
        )


def difference(expected: bytes, actual: bytes) -> tuple[int, int, int]:
    common = min(len(expected), len(actual))
    offset = next((index for index in range(common) if expected[index] != actual[index]), common)
    line = expected[:offset].count(b"\n") + 1
    previous_lf = expected.rfind(b"\n", 0, offset)
    column = offset + 1 if previous_lf < 0 else offset - previous_lf
    return offset, line, column


def verify_pair(root: Path, source: str, output: str) -> None:
    expected = render(load_model(root, source))
    actual = safe_read(
        root,
        output,
        MAX_OUTPUT_BYTES,
        "generated workflow output",
        require_lf_utf8=False,
    )
    if actual == expected:
        return
    offset, line, column = difference(expected, actual)
    raise ContractError(
        "generated workflow output differs from its strict source: "
        f"source={source} output={output} first_difference_byte={offset} "
        f"line={line} column={column} expected_size={len(expected)} actual_size={len(actual)}; "
        f"reproduce: python3 scripts/check_generated_ci_workflows.py --render {source}"
    )


def repository_root(value: str) -> Path:
    path = Path(value).absolute()
    try:
        metadata = path.lstat()
    except OSError as error:
        raise ContractError(f"repository root cannot be inspected: {path}: {error}") from error
    if stat.S_ISLNK(metadata.st_mode) or not stat.S_ISDIR(metadata.st_mode):
        raise ContractError(f"repository root must be a real directory: {path}")
    return path


def parse_arguments(argv: list[str]) -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", default=".", help="repository root to verify (default: cwd)")
    parser.add_argument("--render", metavar="SOURCE", help="render one catalog source to stdout")
    return parser.parse_args(argv)


def main(argv: list[str] | None = None) -> int:
    arguments = parse_arguments(sys.argv[1:] if argv is None else argv)
    try:
        root = repository_root(arguments.root)
        verify_catalog(root)
        verify_attributes(root)
        verify_toolchain_wiring(root)
        if arguments.render is not None:
            matches = [pair for pair in CATALOG if pair[0] == arguments.render]
            if len(matches) != 1:
                raise ContractError(
                    f"--render source must name exactly one catalog source; found {arguments.render!r}"
                )
            sys.stdout.buffer.write(render(load_model(root, matches[0][0])))
            return 0
        for source, output in CATALOG:
            verify_pair(root, source, output)
    except (ContractError, OSError) as error:
        print(f"generated CI workflow verification failed: {error}", file=sys.stderr)
        return 1
    print(f"verified {len(CATALOG)} generated CI workflow fixture pair(s)")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
