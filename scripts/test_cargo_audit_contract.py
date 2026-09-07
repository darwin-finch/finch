#!/usr/bin/env python3
"""Production-boundary regressions for the direct cargo-audit invocation."""

from __future__ import annotations

import json
import os
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parent.parent
CHECKER = ROOT / "scripts/check_cargo_audit_contract.py"


class AuditFixture:
    def __init__(self, report: object, status: int = 0) -> None:
        self.temporary = tempfile.TemporaryDirectory()
        self.root = Path(self.temporary.name)
        self.cargo_home = self.root / "cargo-home"
        self.bin = self.cargo_home / "bin"
        self.bin.mkdir(parents=True)
        self.audit_log = self.root / "cargo-audit-argv.json"
        self.cargo_log = self.root / "cargo-was-invoked"
        self.report = self.root / "report.json"
        self.report.write_text(json.dumps(report), encoding="utf-8")
        self._write_executable(
            self.bin / "cargo-audit",
            "#!/usr/bin/env python3\n"
            "import json, os, pathlib, sys\n"
            "pathlib.Path(os.environ['AUDIT_ARGV_LOG']).write_text(json.dumps(sys.argv[1:]))\n"
            "sys.stdout.buffer.write(pathlib.Path(os.environ['AUDIT_REPORT']).read_bytes())\n"
            f"raise SystemExit({status})\n",
        )
        hostile_bin = self.root / "hostile-bin"
        hostile_bin.mkdir()
        self._write_executable(
            hostile_bin / "cargo",
            "#!/usr/bin/env python3\n"
            "import os, pathlib\n"
            "pathlib.Path(os.environ['CARGO_INVOKED_LOG']).write_text('invoked')\n"
            "raise SystemExit(0)\n",
        )
        repository_config = self.root / ".cargo"
        repository_config.mkdir()
        self.hostile_config = repository_config / "config.toml"
        self.hostile_config.write_text(
            "[alias]\naudit = ['run', '--bin', 'forged-clean-audit']\n", encoding="utf-8"
        )
        self.environment = os.environ.copy()
        self.environment.update(
            {
                "CARGO_HOME": str(self.cargo_home),
                "AUDIT_ARGV_LOG": str(self.audit_log),
                "AUDIT_REPORT": str(self.report),
                "CARGO_INVOKED_LOG": str(self.cargo_log),
                "PATH": f"{hostile_bin}{os.pathsep}{self.environment.get('PATH', '')}",
            }
        )

    @staticmethod
    def _write_executable(path: Path, contents: str) -> None:
        path.write_text(contents, encoding="utf-8")
        path.chmod(0o755)

    def run(self) -> subprocess.CompletedProcess[str]:
        return subprocess.run(
            [sys.executable, str(CHECKER), "--root", str(self.root)],
            cwd=self.root,
            env=self.environment,
            check=False,
            capture_output=True,
            text=True,
        )

    def close(self) -> None:
        self.temporary.cleanup()


def clean_report() -> dict[str, object]:
    return {"vulnerabilities": {"list": []}}


class CargoAuditContractTests(unittest.TestCase):
    def test_repository_alias_and_path_cargo_cannot_replace_direct_executable(self) -> None:
        fixture = AuditFixture(clean_report())
        try:
            result = fixture.run()
            self.assertEqual(
                result.returncode,
                0,
                f"direct cargo-audit boundary rejected a clean report: stdout={result.stdout!r} stderr={result.stderr!r}",
            )
            self.assertEqual(
                json.loads(fixture.audit_log.read_text(encoding="utf-8")),
                ["audit", "--json"],
                f"cargo-audit received incorrect argv; stdout={result.stdout!r} stderr={result.stderr!r}",
            )
            self.assertFalse(
                fixture.cargo_log.exists(),
                f"repository Cargo alias or PATH cargo intercepted the audit; stdout={result.stdout!r} stderr={result.stderr!r}",
            )
        finally:
            fixture.close()

    def test_named_historical_advisory_is_actionable(self) -> None:
        fixture = AuditFixture(
            {
                "vulnerabilities": {
                    "list": [
                        {
                            "advisory": {"id": "RUSTSEC-2026-0194"},
                            "package": {"name": "quick-xml", "version": "0.40.0"},
                        }
                    ]
                }
            },
            status=1,
        )
        try:
            result = fixture.run()
            self.assertEqual(result.returncode, 1, result.stdout + result.stderr)
            self.assertIn("RUSTSEC-2026-0194/quick-xml@0.40.0", result.stderr)
        finally:
            fixture.close()

    def test_unrelated_nonzero_audit_is_not_misreported_as_clean(self) -> None:
        fixture = AuditFixture(
            {
                "vulnerabilities": {
                    "list": [
                        {
                            "advisory": {"id": "RUSTSEC-2099-9999"},
                            "package": {"name": "future-crate", "version": "1.0.0"},
                        }
                    ]
                }
            },
            status=1,
        )
        try:
            result = fixture.run()
            self.assertEqual(result.returncode, 1, result.stdout + result.stderr)
            self.assertIn("outside the five permanent focused guards", result.stderr)
        finally:
            fixture.close()


if __name__ == "__main__":
    unittest.main(verbosity=2)
