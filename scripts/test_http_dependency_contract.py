#!/usr/bin/env python3
"""Regression tests for Finch's resolved HTTP dependency contract."""

from __future__ import annotations

import subprocess
import sys
import tempfile
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parent.parent
CHECKER = ROOT / "scripts/check_http_dependency_contract.py"


def lockfile(*packages: tuple[str, str]) -> str:
    sections = ['version = 4\n']
    for name, version in packages:
        sections.append(f'[[package]]\nname = "{name}"\nversion = "{version}"\n')
    return "\n".join(sections)


class HttpDependencyContractTests(unittest.TestCase):
    def run_contract(
        self, contents: str
    ) -> tuple[subprocess.CompletedProcess[str], tempfile.TemporaryDirectory[str]]:
        temporary = tempfile.TemporaryDirectory()
        path = Path(temporary.name) / "Cargo.lock"
        path.write_text(contents, encoding="utf-8")
        result = subprocess.run(
            [sys.executable, str(CHECKER), "--lockfile", str(path)],
            check=False,
            capture_output=True,
            text=True,
        )
        return result, temporary

    def assert_rejected(self, contents: str, diagnostic: str) -> None:
        result, temporary = self.run_contract(contents)
        try:
            self.assertEqual(result.returncode, 1, result.stdout + result.stderr)
            self.assertIn(diagnostic, result.stderr)
        finally:
            temporary.cleanup()

    def test_accepts_reviewed_http_graph_while_websocket_batch_remains(self) -> None:
        result, temporary = self.run_contract(
            lockfile(
                ("reqwest", "0.12.28"),
                ("h2", "0.4.19"),
                ("rustls", "0.22.4"),
                ("rustls", "0.23.43"),
                ("rustls-webpki", "0.102.8"),
                ("rustls-webpki", "0.103.15"),
            )
        )
        try:
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
            self.assertIn("reqwest=0.12.28", result.stdout)
            self.assertIn("h2=0.4.19", result.stdout)
        finally:
            temporary.cleanup()

    def test_rejects_legacy_reqwest_with_specific_diagnostic(self) -> None:
        self.assert_rejected(
            lockfile(("reqwest", "0.11.27"), ("h2", "0.4.19")),
            "reqwest 0.11.27 is outside the reviewed 0.12 line",
        )

    def test_rejects_unreviewed_future_reqwest_line(self) -> None:
        self.assert_rejected(
            lockfile(("reqwest", "0.13.4"), ("h2", "0.4.19")),
            "reqwest 0.13.4 is outside the reviewed 0.12 line",
        )

    def test_rejects_each_h2_version_below_the_fixed_floor(self) -> None:
        for version in ("0.3.27", "0.4.15"):
            with self.subTest(version=version):
                self.assert_rejected(
                    lockfile(("reqwest", "0.12.28"), ("h2", version)),
                    f"h2 {version} is below the fixed 0.4.16 floor",
                )

    def test_rejects_legacy_rustls_http_graph(self) -> None:
        self.assert_rejected(
            lockfile(
                ("reqwest", "0.12.28"),
                ("h2", "0.4.19"),
                ("rustls", "0.21.12"),
            ),
            "rustls 0.21.12 restores the legacy HTTP-client TLS graph",
        )

    def test_rejects_vulnerable_webpki_line(self) -> None:
        self.assert_rejected(
            lockfile(
                ("reqwest", "0.12.28"),
                ("h2", "0.4.19"),
                ("rustls-webpki", "0.101.7"),
            ),
            "rustls-webpki 0.101.7 is the vulnerable 0.101 line",
        )

    def test_rejects_missing_reqwest_or_h2_instead_of_passing_vacuously(self) -> None:
        self.assert_rejected(
            lockfile(("h2", "0.4.19")),
            "no reqwest package is resolved; the HTTP contract would be vacuous",
        )
        self.assert_rejected(
            lockfile(("reqwest", "0.12.28")),
            "no h2 package is resolved; the HTTP/2 floor check would be vacuous",
        )


if __name__ == "__main__":
    unittest.main()
