#!/usr/bin/env python3
"""Pure canonical model for Finch's superseded-run workflow envelope.

This module intentionally knows nothing about repositories or installed workflow
files.  Its entire authority surface is a bounded JSON byte string (or an
already-decoded object) and the deterministic YAML bytes returned to its caller.
"""

from __future__ import annotations

import ast
import json
import re
from typing import Any


MAX_SOURCE_BYTES = 64 * 1024
MAX_SCRIPT_BYTES = 48 * 1024
MAX_OUTPUT_BYTES = 128 * 1024
MAX_SOURCE_NAME_CHARS = 256
MAX_DIAGNOSTIC_VALUE_CHARS = 80
_PYTHON_ENCODING_DECLARATION = re.compile(
    r"^[ \t\f]*#.*?coding[:=][ \t]*[-_.a-zA-Z0-9]+"
)

WORKFLOW_NAME = "Cancel superseded CI runs"
JOB_ID = "cancel-superseded"
JOB_NAME = "Cancel superseded canonical CI runs"
RUNNER = "ubuntu-24.04"
SCRIPT_NAME = "Run trusted bounded reconciliation controller"
EXECUTION_POLICY = "requested-and-all-in-progress-or-dispatch-v1"
DISPATCH_INPUT = "continuation_cursor"
DISPATCH_DESCRIPTION = "Opaque bounded cursor for the next trusted reconciliation pass"
WORKFLOW_RUN_TYPES = ("requested", "in_progress")
WORKFLOW_RUN_WORKFLOWS = ("CI",)
PERMISSIONS = (("actions", "write"), ("pull-requests", "read"))
ENVIRONMENT = (
    ("CONTINUATION_CURSOR", "inputs.continuation_cursor"),
    ("TOKEN", "github.token"),
)


class ContractError(ValueError):
    """A workflow-envelope source or model violated the closed contract."""


def _bounded_type_name(value: Any) -> str:
    name = type(value).__name__
    if len(name) <= MAX_DIAGNOSTIC_VALUE_CHARS:
        return name
    return f"{name[:MAX_DIAGNOSTIC_VALUE_CHARS]}...(character_count={len(name)})"


def _validate_source_name(value: Any) -> str:
    if type(value) is not str:
        raise ContractError(
            f"source_name must be a string; found {_bounded_type_name(value)}"
        )
    if len(value) > MAX_SOURCE_NAME_CHARS:
        raise ContractError(
            f"source_name exceeds the {MAX_SOURCE_NAME_CHARS}-character diagnostic bound; "
            f"found character_count={len(value)}"
        )
    return value


def _bounded_value_description(value: Any) -> str:
    """Describe hostile decoded input without formatting it without a bound."""

    if type(value) is str:
        prefix = value[:MAX_DIAGNOSTIC_VALUE_CHARS]
        suffix = "..." if len(value) > MAX_DIAGNOSTIC_VALUE_CHARS else ""
        return (
            f"string(character_count={len(value)}, "
            f"prefix={prefix!r}{suffix})"
        )
    if type(value) is int:
        return f"int(bit_length={value.bit_length()})"
    if type(value) in (bool, type(None)):
        return repr(value)
    if type(value) in (bytes, bytearray, list, tuple, dict, set, frozenset):
        return f"{_bounded_type_name(value)}(item_count={len(value)})"
    return f"value(type={_bounded_type_name(value)})"


def _bounded_key_description(value: Any) -> str:
    if type(value) is str:
        if len(value) <= MAX_DIAGNOSTIC_VALUE_CHARS:
            return value
        return _bounded_value_description(value)
    return f"<{_bounded_type_name(value)} key>"


def _reject_noninteger_number(value: str) -> None:
    raise ContractError(f"JSON numeric values must be integers; found {value!r}")


def _reject_duplicate_keys(pairs: list[tuple[str, Any]]) -> dict[str, Any]:
    result: dict[str, Any] = {}
    for key, value in pairs:
        if key in result:
            raise ContractError(f"JSON contains duplicate key {key!r}")
        result[key] = value
    return result


def _canonical_json(value: Any) -> bytes:
    try:
        text = json.dumps(
            value,
            allow_nan=False,
            ensure_ascii=False,
            indent=2,
            sort_keys=True,
        )
        return (text + "\n").encode("utf-8", "strict")
    except (TypeError, ValueError, UnicodeEncodeError) as error:
        raise ContractError(f"JSON value cannot be represented canonically: {error}") from error


def _first_difference(expected: bytes, actual: bytes) -> tuple[int, int, int]:
    limit = min(len(expected), len(actual))
    offset = next((index for index in range(limit) if expected[index] != actual[index]), limit)
    line = actual.count(b"\n", 0, offset) + 1
    previous_lf = actual.rfind(b"\n", 0, offset)
    column = offset + 1 if previous_lf < 0 else offset - previous_lf
    return offset, line, column


def _exact_object(value: Any, expected: set[str], label: str) -> dict[str, Any]:
    if type(value) is not dict:
        raise ContractError(f"{label} must be an object; found {_bounded_type_name(value)}")
    missing = sorted(key for key in expected if key not in value)
    unknown: list[str] = []
    for key in value:
        if type(key) is str and key in expected:
            continue
        unknown.append(_bounded_key_description(key))
        if len(unknown) == 8:
            break
    if len(value) != len(expected) or missing or unknown:
        raise ContractError(
            f"{label} keys must be exactly {sorted(expected)!r}; "
            f"missing={missing!r} unknown={unknown!r} actual_count={len(value)}"
        )
    return value


def _exact_value(value: Any, expected: Any, label: str) -> None:
    if type(value) is not type(expected):
        raise ContractError(
            f"{label} must be exactly {expected!r}; "
            f"found value of type {_bounded_type_name(value)}"
        )
    if value != expected:
        raise ContractError(
            f"{label} must be exactly {expected!r}; "
            f"found {_bounded_value_description(value)}"
        )


def _exact_string_list(value: Any, expected: tuple[str, ...], label: str) -> None:
    if type(value) is not list:
        raise ContractError(f"{label} must be a list; found {_bounded_type_name(value)}")
    if len(value) != len(expected):
        raise ContractError(
            f"{label} must contain exactly {list(expected)!r}; found item_count={len(value)}"
        )
    for index, expected_item in enumerate(expected):
        _exact_value(value[index], expected_item, f"{label}[{index}]")


def _validate_script_body(value: Any, label: str) -> str:
    if type(value) is not str:
        raise ContractError(f"{label} must be a string; found {_bounded_type_name(value)}")
    if len(value) > MAX_SCRIPT_BYTES:
        raise ContractError(
            f"{label} cannot fit in 1..{MAX_SCRIPT_BYTES} UTF-8 bytes; "
            f"found character_count={len(value)}"
        )
    if not value.endswith("\n"):
        raise ContractError(f"{label} must end with exactly one usable LF boundary")
    if "${{" in value or "}}" in value:
        raise ContractError(f"{label} contains a forbidden GitHub expression delimiter")
    try:
        size = len(value.encode("utf-8", "strict"))
    except UnicodeEncodeError as error:
        raise ContractError(f"{label} is not valid Unicode: {error}") from error
    if not 1 <= size <= MAX_SCRIPT_BYTES:
        raise ContractError(
            f"{label} must contain 1..{MAX_SCRIPT_BYTES} UTF-8 bytes; found {size}"
        )
    for line_number, line in enumerate(value.split("\n", 2)[:2], start=1):
        if _PYTHON_ENCODING_DECLARATION.match(line):
            raise ContractError(
                f"{label} contains a forbidden Python encoding declaration "
                f"on physical line {line_number}; execution is fixed to UTF-8"
            )
    for character in value:
        codepoint = ord(character)
        if character == "\n":
            continue
        if (
            codepoint < 0x20
            or 0x7F <= codepoint <= 0x9F
            or codepoint in (0x2028, 0x2029)
            or 0xD800 <= codepoint <= 0xDFFF
            or codepoint & 0xFFFF in (0xFFFE, 0xFFFF)
        ):
            raise ContractError(
                f"{label} contains forbidden control/YAML code point U+{codepoint:04X}"
            )
    if any(line == "PYTHON" for line in value.splitlines()):
        raise ContractError(f"{label} contains the fixed PYTHON heredoc terminator")
    try:
        ast.parse(value, filename=label, mode="exec", feature_version=(3, 9))
    except (SyntaxError, ValueError, MemoryError, RecursionError) as error:
        raise ContractError(f"{label} must be one syntactically valid Python body: {error}") from error
    return value


def validate_model(value: Any, *, source_name: str = "workflow envelope") -> dict[str, Any]:
    """Validate and return the one closed superseded-run envelope model.

    The returned object is the caller's object.  Callers that retain and mutate
    it must call :func:`render_workflow`, which repeats this validation before
    producing bytes.
    """

    source_name = _validate_source_name(source_name)
    root = _exact_object(value, {"schema_version", "workflow"}, source_name)
    _exact_value(root["schema_version"], 1, f"{source_name}.schema_version")

    workflow = _exact_object(
        root["workflow"],
        {
            "dispatch_inputs",
            "environment",
            "execution_policy",
            "job_id",
            "job_name",
            "name",
            "permissions",
            "runner",
            "script",
            "timeout_minutes",
            "workflow_run",
        },
        f"{source_name}.workflow",
    )
    _exact_value(workflow["name"], WORKFLOW_NAME, f"{source_name}.workflow.name")
    _exact_value(workflow["job_id"], JOB_ID, f"{source_name}.workflow.job_id")
    _exact_value(workflow["job_name"], JOB_NAME, f"{source_name}.workflow.job_name")
    _exact_value(workflow["runner"], RUNNER, f"{source_name}.workflow.runner")
    _exact_value(
        workflow["execution_policy"],
        EXECUTION_POLICY,
        f"{source_name}.workflow.execution_policy",
    )

    timeout = workflow["timeout_minutes"]
    if type(timeout) is not int or not 1 <= timeout <= 5:
        raise ContractError(
            f"{source_name}.workflow.timeout_minutes must be an integer from 1 through 5; "
            f"found {_bounded_value_description(timeout)}"
        )

    trigger = _exact_object(
        workflow["workflow_run"],
        {"types", "workflows"},
        f"{source_name}.workflow.workflow_run",
    )
    _exact_string_list(
        trigger["types"], WORKFLOW_RUN_TYPES, f"{source_name}.workflow.workflow_run.types"
    )
    _exact_string_list(
        trigger["workflows"],
        WORKFLOW_RUN_WORKFLOWS,
        f"{source_name}.workflow.workflow_run.workflows",
    )

    permissions = _exact_object(
        workflow["permissions"],
        {name for name, _value in PERMISSIONS},
        f"{source_name}.workflow.permissions",
    )
    for name, expected_value in PERMISSIONS:
        _exact_value(
            permissions[name], expected_value, f"{source_name}.workflow.permissions.{name}"
        )

    inputs = _exact_object(
        workflow["dispatch_inputs"],
        {DISPATCH_INPUT},
        f"{source_name}.workflow.dispatch_inputs",
    )
    descriptor = _exact_object(
        inputs[DISPATCH_INPUT],
        {"default", "description", "required", "type"},
        f"{source_name}.workflow.dispatch_inputs.{DISPATCH_INPUT}",
    )
    _exact_value(
        descriptor["description"],
        DISPATCH_DESCRIPTION,
        f"{source_name}.workflow.dispatch_inputs.{DISPATCH_INPUT}.description",
    )
    _exact_value(
        descriptor["required"],
        False,
        f"{source_name}.workflow.dispatch_inputs.{DISPATCH_INPUT}.required",
    )
    _exact_value(
        descriptor["default"],
        "",
        f"{source_name}.workflow.dispatch_inputs.{DISPATCH_INPUT}.default",
    )
    _exact_value(
        descriptor["type"],
        "string",
        f"{source_name}.workflow.dispatch_inputs.{DISPATCH_INPUT}.type",
    )

    environment = _exact_object(
        workflow["environment"],
        {name for name, _value in ENVIRONMENT},
        f"{source_name}.workflow.environment",
    )
    for name, expected_value in ENVIRONMENT:
        _exact_value(
            environment[name], expected_value, f"{source_name}.workflow.environment.{name}"
        )

    script = _exact_object(
        workflow["script"], {"body", "name"}, f"{source_name}.workflow.script"
    )
    _exact_value(script["name"], SCRIPT_NAME, f"{source_name}.workflow.script.name")
    _validate_script_body(script["body"], f"{source_name}.workflow.script.body")
    return root


def parse_model(source: bytes, *, source_name: str = "workflow envelope JSON") -> dict[str, Any]:
    """Parse bounded, duplicate-free, canonical UTF-8 JSON into the closed model."""

    source_name = _validate_source_name(source_name)
    if type(source) is not bytes:
        raise ContractError(f"{source_name} must be bytes; found {_bounded_type_name(source)}")
    if not source:
        raise ContractError(f"{source_name} must not be empty")
    if len(source) > MAX_SOURCE_BYTES:
        raise ContractError(
            f"{source_name} exceeds the {MAX_SOURCE_BYTES}-byte input bound; found {len(source)}"
        )
    try:
        text = source.decode("utf-8", "strict")
    except UnicodeDecodeError as error:
        raise ContractError(f"{source_name} is not strict UTF-8: {error}") from error
    try:
        value = json.loads(
            text,
            object_pairs_hook=_reject_duplicate_keys,
            parse_float=_reject_noninteger_number,
            parse_constant=_reject_noninteger_number,
        )
    except ContractError:
        raise
    except (json.JSONDecodeError, ValueError, MemoryError, RecursionError) as error:
        raise ContractError(f"{source_name} is not strict JSON: {error}") from error

    validated = validate_model(value, source_name=source_name)
    canonical = _canonical_json(validated)
    if canonical != source:
        offset, line, column = _first_difference(canonical, source)
        raise ContractError(
            f"{source_name} is not canonical sorted two-space JSON with one trailing LF: "
            f"first_difference_byte={offset} line={line} column={column} "
            f"expected_size={len(canonical)} actual_size={len(source)}"
        )
    return validated


def _quote(value: str) -> str:
    return json.dumps(value, allow_nan=False, ensure_ascii=False)


def render_workflow(value: Any, *, source_name: str = "workflow envelope") -> bytes:
    """Render the validated model to one deterministic GitHub Actions YAML byte sequence."""

    model = validate_model(value, source_name=source_name)
    workflow = model["workflow"]
    trigger = workflow["workflow_run"]
    descriptor = workflow["dispatch_inputs"][DISPATCH_INPUT]
    script = workflow["script"]

    lines = [
        f"name: {_quote(workflow['name'])}",
        "",
        "on:",
        "  workflow_run:",
        "    workflows:",
        *(f"      - {_quote(item)}" for item in trigger["workflows"]),
        "    types:",
        *(f"      - {_quote(item)}" for item in trigger["types"]),
        "  workflow_dispatch:",
        "    inputs:",
        f"      {_quote(DISPATCH_INPUT)}:",
        f"        description: {_quote(descriptor['description'])}",
        "        required: false",
        f"        default: {_quote(descriptor['default'])}",
        "        type: string",
        "",
        "permissions:",
        "  actions: write",
        "  pull-requests: read",
        "",
        "jobs:",
        f"  {_quote(workflow['job_id'])}:",
        f"    name: {_quote(workflow['job_name'])}",
        f"    runs-on: {_quote(workflow['runner'])}",
        f"    timeout-minutes: {workflow['timeout_minutes']}",
        "    steps:",
        f"      - name: {_quote(script['name'])}",
        "        env:",
        '          "CONTINUATION_CURSOR": ${{ inputs.continuation_cursor }}',
        '          "TOKEN": ${{ github.token }}',
        "        run: |",
        "          python3 - <<'PYTHON'",
    ]
    for body_line in script["body"].split("\n")[:-1]:
        lines.append(f"          {body_line}" if body_line else "")
    lines.append("          PYTHON")
    output = ("\n".join(lines) + "\n").encode("utf-8", "strict")
    if len(output) > MAX_OUTPUT_BYTES:
        raise ContractError(
            f"rendered workflow exceeds the {MAX_OUTPUT_BYTES}-byte output bound; "
            f"found {len(output)}"
        )
    return output


def parse_and_render(source: bytes, *, source_name: str = "workflow envelope JSON") -> bytes:
    """Parse the canonical JSON model and return its deterministic YAML bytes."""

    return render_workflow(parse_model(source, source_name=source_name), source_name=source_name)
