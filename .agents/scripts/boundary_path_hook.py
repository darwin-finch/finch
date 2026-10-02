#!/usr/bin/env python3
"""Append structured PostToolUse paths to a partial boundary-load trace.

Codex and Claude Code use snake_case hook fields. Grok Build's native hook
contract uses camelCase and may also load Claude-compatible hook files. This
adapter accepts both forms but deliberately does not parse shell commands.

Set SOFTWARE_FACTORY_BOUNDARY_TRACE to a path outside the repository or to an
ignored local path. Configure the hook only for read/navigation tools whose
structured input carries repository paths in one of the supported fields.
The resulting trace is partial and cannot enter an escape-rate denominator.
"""

from __future__ import annotations

import json
import os
import subprocess
import sys
from pathlib import Path, PurePosixPath

TRACE_ENV = "SOFTWARE_FACTORY_BOUNDARY_TRACE"
PATH_FIELDS = ("relative_path", "relativePath", "path", "file_path", "filePath")
PATH_LIST_FIELDS = ("paths", "files")


def first(payload: dict, *fields: str) -> object | None:
    for field in fields:
        value = payload.get(field)
        if value is not None:
            return value
    return None


def repository_root(payload: dict, cwd: Path) -> Path:
    configured = (
        os.environ.get("SOFTWARE_FACTORY_REPOSITORY_ROOT")
        or first(payload, "workspaceRoot")
        or os.environ.get("CLAUDE_PROJECT_DIR")
        or os.environ.get("GROK_WORKSPACE_ROOT")
    )
    if isinstance(configured, str) and configured:
        return Path(configured).resolve()
    result = subprocess.run(
        ["git", "-C", str(cwd), "rev-parse", "--show-toplevel"],
        capture_output=True,
        text=True,
        check=False,
    )
    return Path(result.stdout.strip()).resolve() if result.returncode == 0 else cwd.resolve()


def repo_relative(value: object, cwd: Path, root: Path) -> str | None:
    if not isinstance(value, str) or not value:
        return None
    candidate = Path(value)
    try:
        absolute = candidate.resolve() if candidate.is_absolute() else (cwd / candidate).resolve()
        relative = absolute.relative_to(root)
    except (OSError, ValueError):
        return None
    posix = PurePosixPath(relative.as_posix())
    if posix.is_absolute() or not posix.parts or any(part in ("", ".", "..") for part in posix.parts):
        return None
    return posix.as_posix()


def extract_paths(payload: dict) -> list[str]:
    cwd_value = first(payload, "cwd", "workspaceRoot")
    cwd = Path(str(cwd_value or ".")).resolve()
    root = repository_root(payload, cwd)
    tool_input = first(payload, "tool_input", "toolInput")
    if not isinstance(tool_input, dict):
        return []
    values: list[object] = [tool_input.get(field) for field in PATH_FIELDS]
    for field in PATH_LIST_FIELDS:
        current = tool_input.get(field)
        if isinstance(current, list):
            values.extend(current)
    return sorted({path for value in values if (path := repo_relative(value, cwd, root)) is not None})


def main() -> int:
    trace_path = os.environ.get(TRACE_ENV)
    if not trace_path:
        return 0
    try:
        payload = json.load(sys.stdin)
    except json.JSONDecodeError as error:
        print(f"boundary_path_hook: invalid hook JSON: {error}", file=sys.stderr)
        return 2
    event = first(payload, "hook_event_name", "hookEventName") if isinstance(payload, dict) else None
    if not isinstance(payload, dict) or event != "PostToolUse":
        return 0
    paths = extract_paths(payload)
    if not paths:
        return 0
    destination = Path(trace_path)
    destination.parent.mkdir(parents=True, exist_ok=True)
    try:
        with destination.open("a") as stream:
            for path in paths:
                stream.write(path + "\n")
    except OSError as error:
        print(f"boundary_path_hook: {error}", file=sys.stderr)
        return 2
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
