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

    def test_accepts_consolidated_http_and_websocket_graph(self) -> None:
        result, temporary = self.run_contract(
            lockfile(
                ("reqwest", "0.12.28"),
                ("h2", "0.4.19"),
                ("tokio-tungstenite", "0.24.0"),
                ("rustls", "0.23.43"),
                ("rustls-webpki", "0.103.15"),
            )
        )
        try:
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
            self.assertIn("reqwest=0.12.28", result.stdout)
            self.assertIn("h2=0.4.19", result.stdout)
            self.assertIn("tokio-tungstenite=0.24.0", result.stdout)
        finally:
            temporary.cleanup()

    def test_rejects_legacy_reqwest_with_specific_diagnostic(self) -> None:
        self.assert_rejected(
            lockfile(
                ("reqwest", "0.11.27"),
                ("h2", "0.4.19"),
                ("tokio-tungstenite", "0.24.0"),
                ("rustls", "0.23.43"),
            ),
            "reqwest 0.11.27 is outside the reviewed 0.12 line",
        )

    def test_rejects_unreviewed_future_reqwest_line(self) -> None:
        self.assert_rejected(
            lockfile(
                ("reqwest", "0.13.4"),
                ("h2", "0.4.19"),
                ("tokio-tungstenite", "0.24.0"),
                ("rustls", "0.23.43"),
            ),
            "reqwest 0.13.4 is outside the reviewed 0.12 line",
        )

    def test_rejects_each_h2_version_below_the_fixed_floor(self) -> None:
        for version in ("0.3.27", "0.4.15"):
            with self.subTest(version=version):
                self.assert_rejected(
                    lockfile(
                        ("reqwest", "0.12.28"),
                        ("h2", version),
                        ("tokio-tungstenite", "0.24.0"),
                        ("rustls", "0.23.43"),
                    ),
                    f"h2 {version} is below the fixed 0.4.16 floor",
                )

    def test_rejects_legacy_brain_websocket_stack(self) -> None:
        self.assert_rejected(
            lockfile(
                ("reqwest", "0.12.28"),
                ("h2", "0.4.19"),
                ("tokio-tungstenite", "0.21.0"),
                ("rustls", "0.23.43"),
            ),
            "tokio-tungstenite 0.21.0 is outside the reviewed 0.24 line",
        )

    def test_rejects_each_legacy_rustls_generation(self) -> None:
        for version in ("0.21.12", "0.22.4"):
            with self.subTest(version=version):
                self.assert_rejected(
                    lockfile(
                        ("reqwest", "0.12.28"),
                        ("h2", "0.4.19"),
                        ("tokio-tungstenite", "0.24.0"),
                        ("rustls", version),
                    ),
                    f"rustls {version} is outside Finch's reviewed, consolidated 0.23 TLS generation",
                )

        self.assert_rejected(
            lockfile(
                ("reqwest", "0.12.28"),
                ("h2", "0.4.19"),
                ("tokio-tungstenite", "0.24.0"),
                ("rustls", "0.24.0"),
            ),
            "rustls 0.24.0 is outside Finch's reviewed, consolidated 0.23 TLS generation",
        )

    def test_rejects_each_webpki_version_below_the_fixed_floor(self) -> None:
        for version in ("0.101.7", "0.102.8", "0.103.12"):
            with self.subTest(version=version):
                self.assert_rejected(
                    lockfile(
                        ("reqwest", "0.12.28"),
                        ("h2", "0.4.19"),
                        ("tokio-tungstenite", "0.24.0"),
                        ("rustls", "0.23.43"),
                        ("rustls-webpki", version),
                    ),
                    f"rustls-webpki {version} is outside the fixed, reviewed 0.103.13+ line",
                )

        self.assert_rejected(
            lockfile(
                ("reqwest", "0.12.28"),
                ("h2", "0.4.19"),
                ("tokio-tungstenite", "0.24.0"),
                ("rustls", "0.23.43"),
                ("rustls-webpki", "0.104.0"),
            ),
            "rustls-webpki 0.104.0 is outside the fixed, reviewed 0.103.13+ line",
        )

    def test_rejects_prereleases_for_every_reviewed_dependency(self) -> None:
        stable = {
            "reqwest": "0.12.28",
            "h2": "0.4.19",
            "tokio-tungstenite": "0.24.0",
            "rustls": "0.23.43",
            "rustls-webpki": "0.103.15",
        }
        for package, version in stable.items():
            with self.subTest(package=package):
                packages = [
                    (name, f"{resolved}-alpha.1" if name == package else resolved)
                    for name, resolved in stable.items()
                ]
                self.assert_rejected(
                    lockfile(*packages),
                    f"{package} {version}-alpha.1 is a prerelease; security floors require stable releases",
                )

    def test_rejects_split_tls_and_websocket_generations(self) -> None:
        self.assert_rejected(
            lockfile(
                ("reqwest", "0.12.28"),
                ("h2", "0.4.19"),
                ("tokio-tungstenite", "0.24.0"),
                ("tokio-tungstenite", "0.24.1"),
                ("rustls", "0.23.42"),
                ("rustls", "0.23.43"),
                ("rustls-webpki", "0.103.14"),
                ("rustls-webpki", "0.103.15"),
            ),
            "multiple tokio-tungstenite versions are resolved: 0.24.0, 0.24.1",
        )
        self.assert_rejected(
            lockfile(
                ("reqwest", "0.12.28"),
                ("h2", "0.4.19"),
                ("tokio-tungstenite", "0.24.0"),
                ("rustls", "0.23.42"),
                ("rustls", "0.23.43"),
                ("rustls-webpki", "0.103.15"),
            ),
            "multiple rustls versions are resolved: 0.23.42, 0.23.43",
        )
        self.assert_rejected(
            lockfile(
                ("reqwest", "0.12.28"),
                ("h2", "0.4.19"),
                ("tokio-tungstenite", "0.24.0"),
                ("rustls", "0.23.43"),
                ("rustls-webpki", "0.103.14"),
                ("rustls-webpki", "0.103.15"),
            ),
            "multiple rustls-webpki versions are resolved: 0.103.14, 0.103.15",
        )

    def test_rejects_missing_websocket_or_tls_packages_instead_of_passing_vacuously(self) -> None:
        self.assert_rejected(
            lockfile(
                ("reqwest", "0.12.28"),
                ("h2", "0.4.19"),
                ("rustls", "0.23.43"),
                ("rustls-webpki", "0.103.15"),
            ),
            "no tokio-tungstenite package is resolved; the Brain WebSocket contract would be vacuous",
        )
        self.assert_rejected(
            lockfile(
                ("reqwest", "0.12.28"),
                ("h2", "0.4.19"),
                ("tokio-tungstenite", "0.24.0"),
                ("rustls-webpki", "0.103.15"),
            ),
            "no rustls package is resolved; the TLS generation check would be vacuous",
        )
        self.assert_rejected(
            lockfile(
                ("reqwest", "0.12.28"),
                ("h2", "0.4.19"),
                ("tokio-tungstenite", "0.24.0"),
                ("rustls", "0.23.43"),
            ),
            "no rustls-webpki package is resolved; the certificate-validation floor would be vacuous",
        )

    def test_rejects_missing_reqwest_or_h2_instead_of_passing_vacuously(self) -> None:
        self.assert_rejected(
            lockfile(
                ("h2", "0.4.19"),
                ("tokio-tungstenite", "0.24.0"),
                ("rustls", "0.23.43"),
                ("rustls-webpki", "0.103.15"),
            ),
            "no reqwest package is resolved; the HTTP contract would be vacuous",
        )
        self.assert_rejected(
            lockfile(
                ("reqwest", "0.12.28"),
                ("tokio-tungstenite", "0.24.0"),
                ("rustls", "0.23.43"),
                ("rustls-webpki", "0.103.15"),
            ),
            "no h2 package is resolved; the HTTP/2 floor check would be vacuous",
        )


if __name__ == "__main__":
    unittest.main()
