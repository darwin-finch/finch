#!/usr/bin/env python3
"""Tests snake_case and camelCase boundary path hook contracts."""

from __future__ import annotations

import json
import os
import subprocess
import sys
import tempfile
from pathlib import Path

SCRIPT = Path(__file__).with_name("boundary_path_hook.py")


def run(payload: dict, trace: Path) -> subprocess.CompletedProcess[str]:
    env = dict(os.environ)
    env["SOFTWARE_FACTORY_BOUNDARY_TRACE"] = str(trace)
    return subprocess.run(
        [sys.executable, str(SCRIPT)],
        input=json.dumps(payload),
        capture_output=True,
        text=True,
        env=env,
        check=False,
    )


def main() -> int:
    with tempfile.TemporaryDirectory() as directory:
        root = Path(directory)

        claude_trace = root / "claude.txt"
        claude = run(
            {
                "hook_event_name": "PostToolUse",
                "cwd": str(root),
                "tool_name": "Read",
                "tool_input": {"file_path": str(root / "src/rpc/api.py")},
            },
            claude_trace,
        )
        assert claude.returncode == 0, claude.stderr
        assert claude_trace.read_text().splitlines() == ["src/rpc/api.py"]

        grok_trace = root / "grok.txt"
        nested = root / "packages/client"
        nested.mkdir(parents=True)
        grok = run(
            {
                "hookEventName": "PostToolUse",
                "cwd": str(nested),
                "workspaceRoot": str(root),
                "toolName": "Read",
                "toolInput": {"filePath": "../../src/rpc/private/provider.py"},
            },
            grok_trace,
        )
        assert grok.returncode == 0, grok.stderr
        assert grok_trace.read_text().splitlines() == ["src/rpc/private/provider.py"]

        shell_trace = root / "shell.txt"
        shell = run(
            {
                "hook_event_name": "PostToolUse",
                "cwd": str(root),
                "tool_name": "Bash",
                "tool_input": {"command": "sed -n 1,20p src/hidden.py"},
            },
            shell_trace,
        )
        assert shell.returncode == 0
        assert not shell_trace.exists()

        root_trace = root / "root.txt"
        root_path = run(
            {
                "hook_event_name": "PostToolUse",
                "cwd": str(root),
                "tool_name": "Read",
                "tool_input": {"file_path": str(root)},
            },
            root_trace,
        )
        assert root_path.returncode == 0
        assert not root_trace.exists()
    print("ok")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
