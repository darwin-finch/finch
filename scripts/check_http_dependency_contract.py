#!/usr/bin/env python3
"""Reject vulnerable or unreviewed versions in Finch's resolved HTTP/TLS graph."""

from __future__ import annotations

import argparse
import sys
import tomllib
from pathlib import Path


ROOT = Path(__file__).resolve().parent.parent
H2_MINIMUM = (0, 4, 16)
RUSTLS_MINIMUM = (0, 23, 0)
WEBPKI_MINIMUM = (0, 103, 13)


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
    core = version.split("+", 1)[0]
    if "-" in core:
        raise ValueError(f"prerelease Cargo package version {version!r} is not stable")
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

    def stable_versions(package: str, resolved: set[str]) -> list[str]:
        stable = []
        for version in sorted(resolved):
            if "-" in version.split("+", 1)[0]:
                errors.append(
                    f"{package} {version} is a prerelease; security floors require stable releases"
                )
                continue
            stable.append(version)
        return stable

    reqwest_versions = versions.get("reqwest", set())
    if not reqwest_versions:
        errors.append("no reqwest package is resolved; the HTTP contract would be vacuous")
    for version in stable_versions("reqwest", reqwest_versions):
        parsed = version_tuple(version)
        if parsed[:2] != (0, 12):
            errors.append(
                f"reqwest {version} is outside the reviewed 0.12 line; "
                "legacy 0.11 restores the vulnerable h2/Rustls graph"
            )

    h2_versions = versions.get("h2", set())
    if not h2_versions:
        errors.append("no h2 package is resolved; the HTTP/2 floor check would be vacuous")
    for version in stable_versions("h2", h2_versions):
        if version_tuple(version) < H2_MINIMUM:
            errors.append(
                f"h2 {version} is below the fixed 0.4.16 floor for RUSTSEC-2026-0258"
            )

    websocket_versions = versions.get("tokio-tungstenite", set())
    if not websocket_versions:
        errors.append(
            "no tokio-tungstenite package is resolved; the Brain WebSocket contract would be vacuous"
        )
    if len(websocket_versions) > 1:
        errors.append(
            "multiple tokio-tungstenite versions are resolved: "
            + ", ".join(sorted(websocket_versions))
        )
    for version in stable_versions("tokio-tungstenite", websocket_versions):
        if version_tuple(version)[:2] != (0, 24):
            errors.append(
                f"tokio-tungstenite {version} is outside the reviewed 0.24 line; "
                "legacy 0.21 restores the Rustls 0.22 Brain WebSocket graph"
            )

    rustls_versions = versions.get("rustls", set())
    if not rustls_versions:
        errors.append("no rustls package is resolved; the TLS generation check would be vacuous")
    if len(rustls_versions) > 1:
        errors.append("multiple rustls versions are resolved: " + ", ".join(sorted(rustls_versions)))
    for version in stable_versions("rustls", rustls_versions):
        parsed = version_tuple(version)
        if parsed[:2] != RUSTLS_MINIMUM[:2]:
            errors.append(
                f"rustls {version} is outside Finch's reviewed, consolidated 0.23 TLS generation"
            )

    webpki_versions = versions.get("rustls-webpki", set())
    if not webpki_versions:
        errors.append(
            "no rustls-webpki package is resolved; the certificate-validation floor would be vacuous"
        )
    if len(webpki_versions) > 1:
        errors.append(
            "multiple rustls-webpki versions are resolved: " + ", ".join(sorted(webpki_versions))
        )
    for version in stable_versions("rustls-webpki", webpki_versions):
        parsed = version_tuple(version)
        if parsed[:2] != WEBPKI_MINIMUM[:2] or parsed < WEBPKI_MINIMUM:
            errors.append(
                f"rustls-webpki {version} is outside the fixed, reviewed 0.103.13+ line for "
                "RUSTSEC-2026-0049, -0098, -0099, and -0104"
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
        if package["name"]
        in {"reqwest", "h2", "tokio-tungstenite", "rustls", "rustls-webpki"}
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
