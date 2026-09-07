#!/usr/bin/env python3
"""Run the installed cargo-audit binary directly and enforce Finch's audit contract."""

from __future__ import annotations

import argparse
import json
import os
import subprocess
import sys
import tempfile
from pathlib import Path
from typing import Any


ROOT = Path(__file__).resolve().parent.parent
MAX_REPORT_BYTES = 10 * 1024 * 1024
HISTORICAL_ADVISORIES = {
    "RUSTSEC-2023-0071",
    "RUSTSEC-2026-0153",
    "RUSTSEC-2026-0154",
    "RUSTSEC-2026-0194",
    "RUSTSEC-2026-0195",
}
FORBIDDEN_PACKAGES = {"rsa", "russh", "russh-cryptovec", "russh-keys"}


def cargo_audit_path() -> Path:
    cargo_home = Path(os.environ.get("CARGO_HOME", Path.home() / ".cargo"))
    return cargo_home / "bin" / "cargo-audit"


def vulnerabilities(report: Any) -> list[dict[str, Any]]:
    if not isinstance(report, dict):
        raise ValueError("report root is not an object")
    container = report.get("vulnerabilities")
    if not isinstance(container, dict) or not isinstance(container.get("list"), list):
        raise ValueError("report does not contain vulnerabilities.list as an array")
    entries = container["list"]
    if not all(isinstance(entry, dict) for entry in entries):
        raise ValueError("vulnerabilities.list contains a non-object entry")
    return entries


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--root", type=Path, default=ROOT)
    arguments = parser.parse_args()
    root = arguments.root.resolve()
    lockfile = root / "Cargo.lock"
    if not lockfile.is_file():
        print(
            f"Cargo audit contract: expected lockfile is missing: {lockfile}",
            file=sys.stderr,
        )
        return 1
    executable = cargo_audit_path()
    if not executable.is_file() or not os.access(executable, os.X_OK):
        print(
            f"Cargo audit contract: expected executable is missing or not executable: {executable}",
            file=sys.stderr,
        )
        return 1

    command = [str(executable), "audit", "--json", "--file", str(lockfile)]
    with tempfile.TemporaryDirectory(prefix="finch-cargo-audit-") as temporary:
        sandbox = Path(temporary)
        clean_home = sandbox / "home"
        clean_cargo_home = sandbox / "cargo-home"
        clean_home.mkdir()
        clean_cargo_home.mkdir()
        child_environment = os.environ.copy()
        child_environment["HOME"] = str(clean_home)
        child_environment["CARGO_HOME"] = str(clean_cargo_home)
        result = subprocess.run(
            command,
            cwd=sandbox,
            env=child_environment,
            check=False,
            capture_output=True,
            text=False,
        )
    if result.stderr:
        sys.stderr.buffer.write(result.stderr)
    if result.returncode not in (0, 1):
        print(
            f"Cargo audit contract: {executable} audit --json exited unexpectedly "
            f"with status {result.returncode}",
            file=sys.stderr,
        )
        return 1
    if not result.stdout:
        print("Cargo audit contract: cargo-audit produced an empty JSON report", file=sys.stderr)
        return 1
    if len(result.stdout) > MAX_REPORT_BYTES:
        print(
            f"Cargo audit contract: cargo-audit JSON exceeded the 10 MiB bound: "
            f"{len(result.stdout)} bytes",
            file=sys.stderr,
        )
        return 1
    try:
        report = json.loads(result.stdout)
        entries = vulnerabilities(report)
    except (UnicodeDecodeError, json.JSONDecodeError, ValueError) as error:
        print(f"Cargo audit contract: malformed cargo-audit JSON: {error}", file=sys.stderr)
        return 1

    found: list[str] = []
    focused: list[str] = []
    for entry in entries:
        advisory = entry.get("advisory")
        package = entry.get("package")
        advisory_id = advisory.get("id") if isinstance(advisory, dict) else None
        package_name = package.get("name") if isinstance(package, dict) else None
        package_version = package.get("version") if isinstance(package, dict) else None
        diagnostic = f"{advisory_id}/{package_name}@{package_version}"
        found.append(diagnostic)
        print(f"{advisory_id}\t{package_name}\t{package_version}")
        if advisory_id in HISTORICAL_ADVISORIES or package_name in FORBIDDEN_PACKAGES:
            focused.append(diagnostic)

    if focused:
        print(
            "Cargo audit contract: a permanently excluded SSH/RSA/quick-xml advisory "
            f"returned: {', '.join(focused)}",
            file=sys.stderr,
        )
        return 1
    if found:
        print(
            "Cargo audit contract: cargo-audit returned a non-empty vulnerability list: "
            f"{', '.join(found)}",
            file=sys.stderr,
        )
        return 1
    if result.returncode != 0:
        print(
            "Cargo audit contract: cargo-audit found advisories outside the five "
            "permanent focused guards",
            file=sys.stderr,
        )
        return 1

    print(
        "Cargo audit contract: direct cargo-audit executable reported no vulnerabilities; "
        "five permanent advisory guards remain absent"
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
