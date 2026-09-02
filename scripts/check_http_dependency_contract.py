#!/usr/bin/env python3
"""Reject vulnerable or unreviewed versions in Finch's resolved HTTP graph."""

from __future__ import annotations

import argparse
import sys
import tomllib
from pathlib import Path


ROOT = Path(__file__).resolve().parent.parent
H2_MINIMUM = (0, 4, 16)


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--lockfile",
        type=Path,
        default=ROOT / "Cargo.lock",
        help="Resolved Cargo lockfile to inspect",
    )
    return parser.parse_args()


def version_tuple(version: str) -> tuple[int, int, int]:
    core = version.split("+", 1)[0].split("-", 1)[0]
    parts = core.split(".")
    if len(parts) != 3 or any(not part.isdigit() for part in parts):
        raise ValueError(f"unsupported Cargo package version {version!r}")
    major, minor, patch = (int(part) for part in parts)
    return major, minor, patch


def contract_errors(lock: object) -> list[str]:
    if not isinstance(lock, dict) or not isinstance(lock.get("package"), list):
        raise ValueError("Cargo.lock has no package array")

    versions: dict[str, set[str]] = {}
    for package in lock["package"]:
        if not isinstance(package, dict):
            raise ValueError("Cargo.lock package entry is not a table")
        name = package.get("name")
        version = package.get("version")
        if not isinstance(name, str) or not isinstance(version, str):
            raise ValueError("Cargo.lock package entry lacks a string name or version")
        versions.setdefault(name, set()).add(version)

    errors: list[str] = []
    reqwest_versions = versions.get("reqwest", set())
    if not reqwest_versions:
        errors.append("no reqwest package is resolved; the HTTP contract would be vacuous")
    for version in sorted(reqwest_versions):
        parsed = version_tuple(version)
        if parsed[:2] != (0, 12):
            errors.append(
                f"reqwest {version} is outside the reviewed 0.12 line; "
                "legacy 0.11 restores the vulnerable h2/Rustls graph"
            )

    h2_versions = versions.get("h2", set())
    if not h2_versions:
        errors.append("no h2 package is resolved; the HTTP/2 floor check would be vacuous")
    for version in sorted(h2_versions):
        if version_tuple(version) < H2_MINIMUM:
            errors.append(
                f"h2 {version} is below the fixed 0.4.16 floor for RUSTSEC-2026-0258"
            )

    for version in sorted(versions.get("rustls", set())):
        if version_tuple(version)[:2] == (0, 21):
            errors.append(
                f"rustls {version} restores the legacy HTTP-client TLS graph removed by #183"
            )

    for version in sorted(versions.get("rustls-webpki", set())):
        if version_tuple(version)[:2] == (0, 101):
            errors.append(
                f"rustls-webpki {version} is the vulnerable 0.101 line removed by #183"
            )

    return errors


def main() -> int:
    args = parse_args()
    try:
        lock = tomllib.loads(args.lockfile.read_text(encoding="utf-8"))
        errors = contract_errors(lock)
    except (OSError, UnicodeDecodeError, tomllib.TOMLDecodeError, ValueError) as error:
        print(f"HTTP dependency contract could not inspect {args.lockfile}: {error}", file=sys.stderr)
        return 2

    if errors:
        for error in errors:
            print(f"HTTP dependency contract violation: {error}", file=sys.stderr)
        return 1

    packages = {
        package["name"]: set()
        for package in lock["package"]
        if package["name"] in {"reqwest", "h2", "rustls", "rustls-webpki"}
    }
    for package in lock["package"]:
        if package["name"] in packages:
            packages[package["name"]].add(package["version"])
    summary = ", ".join(
        f"{name}={'+'.join(sorted(resolved))}" for name, resolved in sorted(packages.items())
    )
    print(f"HTTP dependency contract passed: {summary}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
