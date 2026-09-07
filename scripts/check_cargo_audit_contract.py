#!/usr/bin/env python3
"""Run the installed cargo-audit binary directly and enforce Finch's audit contract."""

from __future__ import annotations

import argparse
import json
import math
import os
import queue
import subprocess
import sys
import tempfile
import threading
import time
from dataclasses import dataclass
from pathlib import Path
from typing import Any


ROOT = Path(__file__).resolve().parent.parent
MAX_REPORT_BYTES = 10 * 1024 * 1024
MAX_DIAGNOSTIC_BYTES = 1024 * 1024
DEFAULT_TIMEOUT_SECONDS = 300.0
READ_CHUNK_BYTES = 64 * 1024
TERMINATE_GRACE_SECONDS = 2.0
HISTORICAL_ADVISORIES = {
    "RUSTSEC-2023-0071",
    "RUSTSEC-2026-0153",
    "RUSTSEC-2026-0154",
    "RUSTSEC-2026-0194",
    "RUSTSEC-2026-0195",
}
FORBIDDEN_PACKAGES = {"rsa", "russh", "russh-cryptovec", "russh-keys"}


@dataclass(frozen=True)
class AuditProcessResult:
    returncode: int
    stdout: bytes
    stderr: bytes
    failure: str | None = None


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


def terminate_process(process: subprocess.Popen[bytes]) -> None:
    if process.poll() is not None:
        return
    process.terminate()
    try:
        process.wait(timeout=TERMINATE_GRACE_SECONDS)
    except subprocess.TimeoutExpired:
        process.kill()
        process.wait()


def read_bounded_stream(
    stream: Any,
    label: str,
    limit: int,
    results: dict[str, bytes],
    failures: queue.Queue[str],
) -> None:
    collected = bytearray()
    try:
        while True:
            chunk = stream.read(READ_CHUNK_BYTES)
            if not chunk:
                break
            remaining = limit - len(collected)
            if len(chunk) > remaining:
                if remaining > 0:
                    collected.extend(chunk[:remaining])
                results[label] = bytes(collected)
                failures.put(
                    f"cargo-audit {label} exceeded the "
                    f"{limit // (1024 * 1024)} MiB bound"
                )
                return
            collected.extend(chunk)
        results[label] = bytes(collected)
    except (OSError, ValueError) as error:
        results[label] = bytes(collected)
        failures.put(f"failed while reading cargo-audit {label}: {error}")


def run_bounded_audit(
    command: list[str],
    cwd: Path,
    environment: dict[str, str],
    timeout_seconds: float,
) -> AuditProcessResult:
    try:
        process = subprocess.Popen(
            command,
            cwd=cwd,
            env=environment,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
        )
    except OSError as error:
        return AuditProcessResult(
            returncode=-1,
            stdout=b"",
            stderr=b"",
            failure=f"failed to start cargo-audit: {error}",
        )

    assert process.stdout is not None
    assert process.stderr is not None
    results: dict[str, bytes] = {}
    failures: queue.Queue[str] = queue.Queue()
    readers = [
        threading.Thread(
            target=read_bounded_stream,
            args=(process.stdout, "stdout", MAX_REPORT_BYTES, results, failures),
            daemon=True,
        ),
        threading.Thread(
            target=read_bounded_stream,
            args=(process.stderr, "stderr", MAX_DIAGNOSTIC_BYTES, results, failures),
            daemon=True,
        ),
    ]
    for reader in readers:
        reader.start()

    deadline = time.monotonic() + timeout_seconds
    failure: str | None = None
    while process.poll() is None:
        try:
            failure = failures.get(timeout=0.05)
            break
        except queue.Empty:
            pass
        if time.monotonic() >= deadline:
            failure = f"cargo-audit exceeded the {timeout_seconds:g}-second timeout"
            break

    if failure is not None:
        terminate_process(process)
    returncode = process.wait()
    for reader in readers:
        reader.join(timeout=TERMINATE_GRACE_SECONDS)
    process.stdout.close()
    process.stderr.close()
    if any(reader.is_alive() for reader in readers):
        failure = failure or "cargo-audit output readers did not terminate"
    if failure is None:
        try:
            failure = failures.get_nowait()
        except queue.Empty:
            pass
    return AuditProcessResult(
        returncode=returncode,
        stdout=results.get("stdout", b""),
        stderr=results.get("stderr", b""),
        failure=failure,
    )


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--root", type=Path, default=ROOT)
    parser.add_argument(
        "--timeout-seconds",
        type=float,
        default=DEFAULT_TIMEOUT_SECONDS,
        help="maximum wall-clock time allowed for the audit subprocess",
    )
    arguments = parser.parse_args()
    if not math.isfinite(arguments.timeout_seconds) or arguments.timeout_seconds <= 0:
        parser.error("--timeout-seconds must be a finite positive number")
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
        result = run_bounded_audit(
            command,
            cwd=sandbox,
            environment=child_environment,
            timeout_seconds=arguments.timeout_seconds,
        )
    if result.stderr:
        sys.stderr.buffer.write(result.stderr)
    if result.failure is not None:
        print(f"Cargo audit contract: {result.failure}", file=sys.stderr)
        return 1
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
