#!/usr/bin/env python3
"""Emit a stable digest of native tools that can affect cached Rust objects."""

from __future__ import annotations

import hashlib
import os
import subprocess
import sys
from pathlib import Path


def command_identity(command: list[str]) -> str:
    result = subprocess.run(command, check=False, capture_output=True, text=True)
    output = (result.stdout + result.stderr).strip()
    if result.returncode != 0 or not output:
        print(
            f"native cache identity command failed ({result.returncode}): {' '.join(command)}\n{output}",
            file=sys.stderr,
        )
        raise SystemExit(1)
    return output


runner_os = os.environ.get("RUNNER_OS", "")
runner_arch = os.environ.get("RUNNER_ARCH", "")
github_output = os.environ.get("GITHUB_OUTPUT", "")
if not runner_os or not runner_arch or not github_output:
    raise SystemExit("RUNNER_OS, RUNNER_ARCH, and GITHUB_OUTPUT are required")

parts = [runner_os, runner_arch, command_identity(["capnp", "--version"])]
if runner_os == "macOS":
    parts.extend(
        [command_identity(["clang", "--version"]), command_identity(["xcrun", "--show-sdk-version"])]
    )
elif runner_os == "Linux":
    parts.extend([command_identity(["cc", "--version"]), command_identity(["ld", "--version"])])
else:
    raise SystemExit(f"unsupported native cache runner: {runner_os}/{runner_arch}")

digest = hashlib.sha256("\0".join(parts).encode()).hexdigest()
with Path(github_output).open("a", encoding="utf-8") as output:
    output.write(f"digest={digest}\n")
print(f"native compiler cache identity: {runner_os}/{runner_arch} {digest}")
