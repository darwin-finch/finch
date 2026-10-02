#!/usr/bin/env python3
"""Tests the narrow Codex structured-path hook adapter."""

from __future__ import annotations

import json
import os
import subprocess
import sys
import tempfile
from pathlib import Path

SCRIPT = Path(__file__).with_name("codex_boundary_hook.py")


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
        trace = root / "trace.txt"
        result = run(
            {
                "hook_event_name": "PostToolUse",
                "cwd": str(root),
                "tool_name": "mcp__serena__find_symbol",
                "tool_input": {
                    "relative_path": "src/rpc/private/provider.py",
                    "paths": ["src/rpc/api.py", "../outside.py"],
                },
            },
            trace,
        )
        assert result.returncode == 0, result.stderr
        assert trace.read_text().splitlines() == ["src/rpc/api.py", "src/rpc/private/provider.py"]

        shell = run(
            {
                "hook_event_name": "PostToolUse",
                "cwd": str(root),
                "tool_name": "Bash",
                "tool_input": {"command": "sed -n 1,20p src/hidden.py"},
            },
            trace,
        )
        assert shell.returncode == 0
        assert "src/hidden.py" not in trace.read_text()
    print("ok")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
