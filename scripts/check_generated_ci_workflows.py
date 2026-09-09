#!/usr/bin/env python3
"""Render and verify Finch's bounded generated CI workflow catalog."""

from __future__ import annotations

import argparse
import ast
import json
import os
import re
import selectors
import stat
import subprocess
import sys
import time
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
TOOLCHAIN_PREFIX = b'''#!/usr/bin/env bash
set -euo pipefail

repository_root=$(git rev-parse --show-toplevel)
cd "$repository_root"

check_format=true
if [[ "${1:-}" == "--metadata-only" ]]; then
  check_format=false
elif [[ $# -ne 0 ]]; then
  echo "usage: $0 [--metadata-only]" >&2
  exit 2
fi

python3 scripts/test_generated_ci_workflows.py
python3 scripts/check_generated_ci_workflows.py
python3 tests/test_toolchain_locked_metadata.py
cargo metadata --locked --no-deps --format-version 1 >/dev/null
'''
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


def utf8_size(value: str, label: str) -> int:
    for character in value:
        codepoint = ord(character)
        if (
            codepoint < 0x20
            or 0x7F <= codepoint <= 0x9F
            or codepoint in (0x2028, 0x2029)
            or 0xD800 <= codepoint <= 0xDFFF
            or codepoint & 0xFFFF in (0xFFFE, 0xFFFF)
        ):
            raise ContractError(
                f"{label} contains forbidden YAML/control code point U+{codepoint:04X}"
            )
    try:
        return len(value.encode("utf-8"))
    except UnicodeEncodeError as error:
        raise ContractError(f"{label} is not valid Unicode: {error}") from error


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
    size = utf8_size(value, label)
    if not value or size > maximum:
        raise ContractError(f"{label} must contain 1..{maximum} UTF-8 bytes")
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
    expected_environment = {
        "CONTINUATION_CURSOR": f"inputs.{input_name}",
        "TOKEN": "github.token",
    }
    if environment != expected_environment:
        raise ContractError(
            f"{source}.workflow.environment must be exactly {expected_environment!r}; "
            f"found {environment!r}"
        )

    script = exact_keys(workflow["script"], {"body", "name"}, f"{source}.workflow.script")
    safe_string(script["name"], f"{source}.workflow.script.name", SAFE_WORKFLOW_NAME)
    body = script["body"]
    if not isinstance(body, str):
        raise ContractError(f"{source}.workflow.script.body must be a string")
    if not body.endswith("\n"):
        raise ContractError(f"{source}.workflow.script.body must end in LF")
    body_size = utf8_size(body.replace("\n", ""), f"{source}.workflow.script.body")
    if "${{" in body or "}}" in body:
        raise ContractError(
            f"{source}.workflow.script.body contains a forbidden GitHub expression delimiter"
        )
    if body_size + body.count("\n") > 48 * 1024:
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


TREE_PATHS = {
    ".gitattributes": ("100644", MAX_ATTRIBUTES_BYTES),
    "tests/toolchain_contract.sh": ("100755", MAX_TOOLCHAIN_BYTES),
    CATALOG[0][0]: ("100644", MAX_SOURCE_BYTES),
    CATALOG[0][1]: ("100644", MAX_OUTPUT_BYTES),
}


class GitSnapshot:
    """Read one immutable, tracked HEAD tree from an anchored repository root."""

    def __init__(self, root: Path):
        self.root = root
        self.root_descriptor = -1
        self.root_identity: tuple[int, int] | None = None
        self.tree: str | None = None

    def __enter__(self) -> "GitSnapshot":
        required = ("O_DIRECTORY", "O_NOFOLLOW", "O_NONBLOCK")
        values = {name: getattr(os, name, None) for name in required}
        missing = [name for name, value in values.items() if not isinstance(value, int) or value == 0]
        if missing:
            raise ContractError(f"safe repository anchor requires unavailable flags: {missing!r}")
        flags = os.O_RDONLY | values["O_DIRECTORY"] | values["O_NOFOLLOW"] | values["O_NONBLOCK"]
        flags |= getattr(os, "O_CLOEXEC", 0)
        try:
            before = self.root.lstat()
            descriptor = os.open(self.root, flags)
            opened = os.fstat(descriptor)
        except OSError as error:
            raise ContractError(f"repository root cannot be anchored safely: path={self.root}: {error}") from error
        if not stat.S_ISDIR(opened.st_mode) or (before.st_dev, before.st_ino) != (
            opened.st_dev,
            opened.st_ino,
        ):
            os.close(descriptor)
            raise ContractError(f"repository root changed identity while anchoring: path={self.root}")
        self.root_descriptor = descriptor
        self.root_identity = (opened.st_dev, opened.st_ino)
        return self

    def __exit__(self, *_args: object) -> None:
        if self.root_descriptor >= 0:
            os.close(self.root_descriptor)
            self.root_descriptor = -1

    def assert_root(self) -> None:
        try:
            resident = self.root.lstat()
            opened = os.fstat(self.root_descriptor)
        except OSError as error:
            raise ContractError(f"repository root cannot be revalidated: path={self.root}: {error}") from error
        identities = {(resident.st_dev, resident.st_ino), (opened.st_dev, opened.st_ino)}
        if identities != {self.root_identity} or not stat.S_ISDIR(resident.st_mode):
            raise ContractError(
                f"repository root changed after anchoring: expected={self.root_identity!r} "
                f"resident={(resident.st_dev, resident.st_ino)!r}"
            )

    def git(self, arguments: list[str], maximum: int, label: str) -> bytes:
        self.assert_root()
        environment = {key: value for key, value in os.environ.items() if not key.startswith("GIT_")}
        environment.update(
            {
                "GIT_CONFIG_NOSYSTEM": "1",
                "GIT_CONFIG_GLOBAL": os.devnull,
                "GIT_TERMINAL_PROMPT": "0",
                "LC_ALL": "C",
            }
        )
        try:
            process = subprocess.Popen(
                ["git", "-C", str(self.root), *arguments],
                env=environment,
                stdin=subprocess.DEVNULL,
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
            )
        except OSError as error:
            raise ContractError(f"{label} could not execute Git: {error}") from error
        assert process.stdout is not None and process.stderr is not None
        streams = selectors.DefaultSelector()
        streams.register(process.stdout, selectors.EVENT_READ, ("stdout", maximum))
        streams.register(process.stderr, selectors.EVENT_READ, ("stderr", 4096))
        buffers = {"stdout": bytearray(), "stderr": bytearray()}
        deadline = time.monotonic() + 10
        try:
            while streams.get_map():
                remaining = deadline - time.monotonic()
                if remaining <= 0:
                    process.kill()
                    process.wait(timeout=5)
                    raise ContractError(f"{label} timed out after 10 seconds")
                events = streams.select(remaining)
                if not events:
                    continue
                for key, _mask in events:
                    channel, limit = key.data
                    chunk = os.read(key.fileobj.fileno(), min(4096, limit + 1))
                    if not chunk:
                        streams.unregister(key.fileobj)
                        continue
                    buffers[channel].extend(chunk)
                    if len(buffers[channel]) > limit:
                        process.kill()
                        process.wait(timeout=5)
                        raise ContractError(
                            f"{label} exceeded its {limit}-byte {channel} bound"
                        )
            status = process.wait(timeout=max(0.1, deadline - time.monotonic()))
        finally:
            streams.close()
            process.stdout.close()
            process.stderr.close()
            if process.poll() is None:
                process.kill()
                process.wait(timeout=5)
        output = bytes(buffers["stdout"])
        stderr_prefix = bytes(buffers["stderr"])
        if status != 0:
            raise ContractError(
                f"{label} failed: status={status} stderr_prefix={stderr_prefix!r} "
                f"stderr_size={len(stderr_prefix)}"
            )
        self.assert_root()
        return output

    def load(self) -> dict[str, bytes]:
        tree_raw = self.git(["rev-parse", "--verify", "HEAD^{tree}"], 128, "HEAD tree resolution")
        tree = tree_raw.decode("ascii", "strict").strip()
        if re.fullmatch(r"[0-9a-f]{40,64}", tree) is None:
            raise ContractError(f"HEAD tree resolution returned an invalid object ID: {tree!r}")
        self.tree = tree
        selection_paths = [".gitattributes", "tests/toolchain_contract.sh", FIXTURE_DIRECTORY]
        listing = self.git(
            ["ls-tree", "-rz", "--full-tree", tree, "--", *selection_paths],
            16 * 1024,
            "authority tree catalog",
        )
        entries: dict[str, tuple[str, str]] = {}
        for raw_entry in listing.split(b"\0"):
            if not raw_entry:
                continue
            try:
                metadata, raw_path = raw_entry.split(b"\t", 1)
                mode, kind, raw_oid = metadata.split(b" ", 2)
                path = raw_path.decode("utf-8", "strict")
                oid = raw_oid.decode("ascii", "strict")
            except (UnicodeDecodeError, ValueError) as error:
                raise ContractError(f"authority tree catalog contains malformed entry: {raw_entry[:256]!r}") from error
            if path in entries or kind != b"blob" or re.fullmatch(r"[0-9a-f]{40,64}", oid) is None:
                raise ContractError(
                    f"authority tree catalog contains invalid or duplicate entry: path={path!r} "
                    f"kind={kind!r} oid={oid!r}"
                )
            entries[path] = (mode.decode("ascii", "strict"), oid)
            if len(entries) > len(TREE_PATHS):
                fixture_entries = {
                    item for item in entries if item.startswith(f"{FIXTURE_DIRECTORY}/")
                }
                raise ContractError(
                    "authority tree fixture path/count drifted: "
                    f"expected_count=2 actual_count_at_least={len(fixture_entries)} "
                    f"unknown_sample={sorted(fixture_entries - set(expected_catalog_paths()))[:2]!r}"
                )
        if set(entries) != set(TREE_PATHS):
            raise ContractError(
                "authority tree path/count drifted: "
                f"expected_count={len(TREE_PATHS)} actual_count={len(entries)} "
                f"missing={sorted(set(TREE_PATHS) - set(entries))!r} "
                f"unknown={sorted(set(entries) - set(TREE_PATHS))!r}"
            )
        blobs: dict[str, bytes] = {}
        for path, (mode, oid) in entries.items():
            expected_mode, maximum = TREE_PATHS[path]
            if mode != expected_mode:
                raise ContractError(
                    f"authority tree mode drifted: path={path} expected={expected_mode} actual={mode}"
                )
            size_raw = self.git(["cat-file", "-s", oid], 64, f"blob size for {path}")
            try:
                size = int(size_raw.strip())
            except ValueError as error:
                raise ContractError(f"blob size for {path} is invalid: {size_raw[:64]!r}") from error
            if not 0 <= size <= maximum:
                raise ContractError(
                    f"authority blob exceeds its bound: path={path} size={size} maximum={maximum}"
                )
            blob = self.git(["cat-file", "blob", oid], maximum, f"authority blob {path}")
            if len(blob) != size:
                raise ContractError(
                    f"authority blob size changed: path={path} declared={size} actual={len(blob)}"
                )
            blobs[path] = blob
        return blobs


def load_model(raw: bytes, relative: str) -> dict[str, Any]:
    if b"\0" in raw or b"\r" in raw:
        raise ContractError(f"generated workflow source must be NUL-free LF text: path={relative}")
    try:
        value = json.loads(
            raw,
            object_pairs_hook=duplicate_rejecting_object,
            parse_float=reject_noninteger_number,
            parse_constant=reject_noninteger_number,
        )
    except UnicodeDecodeError as error:
        raise ContractError(f"generated workflow source is not UTF-8: path={relative}: {error}") from error
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


def expected_catalog_paths() -> tuple[str, ...]:
    return tuple(path for pair in CATALOG for path in pair)


def verify_catalog(blobs: dict[str, bytes]) -> None:
    expected_paths = set(expected_catalog_paths())
    if len(CATALOG) != 1 or len(expected_paths) != 2:
        raise ContractError("internal generated workflow catalog must contain exactly one source/output pair")
    actual_paths = {path for path in blobs if path.startswith(f"{FIXTURE_DIRECTORY}/")}
    if actual_paths != expected_paths:
        raise ContractError(
            "generated workflow fixture catalog path/count drifted: "
            f"expected_count=2 actual_count={len(actual_paths)} "
            f"missing={sorted(expected_paths - actual_paths)!r} "
            f"unknown={sorted(actual_paths - expected_paths)!r}"
        )


def verify_attributes(snapshot: GitSnapshot, raw: bytes) -> None:
    if b"\0" in raw or b"\r" in raw:
        raise ContractError("Git attributes contract must be NUL-free LF text")
    try:
        lines = raw.decode("utf-8").splitlines()
    except UnicodeDecodeError as error:
        raise ContractError(f"Git attributes contract is not UTF-8: {error}") from error
    counts = {line: lines.count(line) for line in REQUIRED_ATTRIBUTES}
    if any(count != 1 for count in counts.values()):
        raise ContractError(
            "Git attributes must contain exactly one LF rule for each generated fixture extension: "
            f"required={REQUIRED_ATTRIBUTES!r} counts={counts!r}"
        )
    fixture_paths = [path for pair in CATALOG for path in pair]
    result = snapshot.git(
        ["check-attr", f"--source={snapshot.tree}", "-z", "text", "eol", "--", *fixture_paths],
        4096,
        "Git attributes effective-rule check",
    )
    fields = result.split(b"\0")
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


def verify_toolchain_wiring(raw: bytes) -> None:
    # This pins the executable top-level prefix instead of interpreting shell. If an
    # attacker disables every invocation, no in-repository checker can execute itself.
    if not raw.startswith(TOOLCHAIN_PREFIX):
        raise ContractError(
            "canonical toolchain gate executable prefix drifted; generated-workflow tests and checker "
            "must be unconditional top-level commands before Cargo metadata"
        )
    try:
        lines = raw.decode("utf-8").splitlines()
    except UnicodeDecodeError as error:
        raise ContractError(f"canonical toolchain gate is not UTF-8: {error}") from error
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


def verify_pair(blobs: dict[str, bytes], source: str, output: str) -> None:
    expected = render(load_model(blobs[source], source))
    actual = blobs[output]
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
        with GitSnapshot(root) as snapshot:
            blobs = snapshot.load()
            verify_catalog(blobs)
            verify_attributes(snapshot, blobs[".gitattributes"])
            verify_toolchain_wiring(blobs["tests/toolchain_contract.sh"])
            if arguments.render is not None:
                matches = [pair for pair in CATALOG if pair[0] == arguments.render]
                if len(matches) != 1:
                    raise ContractError(
                        f"--render source must name exactly one catalog source; found {arguments.render!r}"
                    )
                rendered = render(load_model(blobs[matches[0][0]], matches[0][0]))
                sys.stdout.buffer.write(rendered)
                return 0
            for source, output in CATALOG:
                verify_pair(blobs, source, output)
    except (ContractError, OSError) as error:
        print(f"generated CI workflow verification failed: {error}", file=sys.stderr)
        return 1
    print(f"verified {len(CATALOG)} generated CI workflow fixture pair(s)")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
