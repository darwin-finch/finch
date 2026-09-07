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
        self.audit_continued_log = self.root / "cargo-audit-continued"
        self.cargo_log = self.root / "cargo-was-invoked"
        self.report = self.root / "report.json"
        self.lockfile = self.root / "Cargo.lock"
        self.lockfile.write_text("# deterministic audit fixture\nversion = 3\n", encoding="utf-8")
        self.report.write_text(json.dumps(report), encoding="utf-8")
        self._write_executable(
            self.bin / "cargo-audit",
            "#!/usr/bin/env python3\n"
            "import json, os, pathlib, sys\n"
            "state = {\n"
            "  'argv': sys.argv[1:],\n"
            "  'cwd': str(pathlib.Path.cwd()),\n"
            "  'cargo_home': os.environ.get('CARGO_HOME'),\n"
            "  'home': os.environ.get('HOME'),\n"
            "  'cwd_audit_config': (pathlib.Path.cwd() / '.cargo/audit.toml').exists(),\n"
            "  'cargo_home_audit_config': (pathlib.Path(os.environ['CARGO_HOME']) / 'audit.toml').exists(),\n"
            "  'home_audit_config': (pathlib.Path(os.environ['HOME']) / '.cargo/audit.toml').exists(),\n"
            "}\n"
            "pathlib.Path(os.environ['AUDIT_ARGV_LOG']).write_text(json.dumps(state))\n"
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
        self.hostile_alias = repository_config / "config.toml"
        self.hostile_alias.write_text(
            "[alias]\naudit = ['run', '--bin', 'forged-clean-audit']\n", encoding="utf-8"
        )
        hostile_audit_config = (
            "[advisories]\nseverity_threshold = 'critical'\n"
            "[target]\narch = ['suppressed-arch']\nos = ['suppressed-os']\n"
            "[database]\npath = '/nonexistent/suppressed-db'\n"
            "url = 'https://invalid.example/suppressed-db'\nfetch = false\nstale = true\n"
        )
        self.hostile_project_audit = repository_config / "audit.toml"
        self.hostile_project_audit.write_text(hostile_audit_config, encoding="utf-8")
        self.hostile_user_audit = self.cargo_home / "audit.toml"
        self.hostile_user_audit.write_text(hostile_audit_config, encoding="utf-8")
        self.user_home = self.root / "user-home"
        (self.user_home / ".cargo").mkdir(parents=True)
        self.hostile_home_audit = self.user_home / ".cargo/audit.toml"
        self.hostile_home_audit.write_text(hostile_audit_config, encoding="utf-8")
        self.environment = os.environ.copy()
        self.environment.update(
            {
                "CARGO_HOME": str(self.cargo_home),
                "AUDIT_ARGV_LOG": str(self.audit_log),
                "AUDIT_CONTINUED_LOG": str(self.audit_continued_log),
                "AUDIT_REPORT": str(self.report),
                "CARGO_INVOKED_LOG": str(self.cargo_log),
                "HOME": str(self.user_home),
                "PATH": f"{hostile_bin}{os.pathsep}{self.environment.get('PATH', '')}",
            }
        )

    @staticmethod
    def _write_executable(path: Path, contents: str) -> None:
        path.write_text(contents, encoding="utf-8")
        path.chmod(0o755)

    def replace_audit_executable(self, contents: str) -> None:
        self._write_executable(self.bin / "cargo-audit", contents)

    def run(self, timeout_seconds: float = 5.0) -> subprocess.CompletedProcess[str]:
        return subprocess.run(
            [
                sys.executable,
                str(CHECKER),
                "--root",
                str(self.root),
                "--timeout-seconds",
                str(timeout_seconds),
            ],
            cwd=self.root,
            env=self.environment,
            check=False,
            capture_output=True,
            text=True,
            timeout=10,
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
                json.loads(fixture.audit_log.read_text(encoding="utf-8"))["argv"],
                ["audit", "--json", "--file", str(fixture.lockfile.resolve())],
                f"cargo-audit received incorrect argv; stdout={result.stdout!r} stderr={result.stderr!r}",
            )
            state = json.loads(fixture.audit_log.read_text(encoding="utf-8"))
            self.assertNotEqual(
                Path(state["cwd"]),
                fixture.root,
                f"cargo-audit ran in the hostile repository config scope: state={state!r}",
            )
            self.assertFalse(
                state["cwd_audit_config"],
                f"cargo-audit clean cwd unexpectedly contained project audit config: state={state!r}",
            )
            self.assertNotEqual(
                Path(state["cargo_home"]),
                fixture.cargo_home,
                f"cargo-audit inherited the hostile user Cargo home: state={state!r}",
            )
            self.assertFalse(
                state["cargo_home_audit_config"],
                f"cargo-audit clean Cargo home unexpectedly contained user audit config: state={state!r}",
            )
            self.assertNotEqual(
                Path(state["home"]),
                Path(fixture.environment["HOME"]),
                f"cargo-audit inherited the hostile user home: state={state!r}",
            )
            self.assertFalse(
                state["home_audit_config"],
                f"cargo-audit clean home unexpectedly contained user audit config: state={state!r}",
            )
            self.assertFalse(
                fixture.cargo_log.exists(),
                f"repository Cargo alias or PATH cargo intercepted the audit; stdout={result.stdout!r} stderr={result.stderr!r}",
            )
        finally:
            fixture.close()

    def test_nonempty_vulnerability_list_fails_even_when_child_reports_success(self) -> None:
        fixture = AuditFixture(
            {
                "vulnerabilities": {
                    "list": [
                        {
                            "advisory": {"id": "RUSTSEC-2099-0001"},
                            "package": {"name": "unexpected-crate", "version": "2.3.4"},
                        }
                    ]
                }
            },
            status=0,
        )
        try:
            result = fixture.run()
            self.assertEqual(
                result.returncode,
                1,
                f"non-empty vulnerability report passed on child status zero: stdout={result.stdout!r} stderr={result.stderr!r}",
            )
            self.assertIn("RUSTSEC-2099-0001/unexpected-crate@2.3.4", result.stderr)
            self.assertIn("non-empty vulnerability list", result.stderr)
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
            self.assertIn("non-empty vulnerability list", result.stderr)
            self.assertIn("RUSTSEC-2099-9999/future-crate@1.0.0", result.stderr)
        finally:
            fixture.close()

    def test_oversized_stdout_is_terminated_before_the_child_continues(self) -> None:
        fixture = AuditFixture(clean_report())
        fixture.replace_audit_executable(
            "#!/usr/bin/env python3\n"
            "import os, pathlib, sys, time\n"
            "chunk = b'x' * 65536\n"
            "for _ in range(200):\n"
            "    sys.stdout.buffer.write(chunk)\n"
            "    sys.stdout.buffer.flush()\n"
            "    time.sleep(0.001)\n"
            "pathlib.Path(os.environ['AUDIT_CONTINUED_LOG']).write_text('continued')\n"
            "time.sleep(30)\n"
        )
        try:
            result = fixture.run()
            self.assertEqual(
                result.returncode,
                1,
                f"oversized cargo-audit stdout did not fail: stdout={result.stdout!r} stderr={result.stderr!r}",
            )
            self.assertIn("stdout exceeded the 10 MiB bound", result.stderr)
            self.assertFalse(
                fixture.audit_continued_log.exists(),
                f"cargo-audit continued after crossing the stdout bound: stdout={result.stdout!r} stderr={result.stderr!r}",
            )
        finally:
            fixture.close()

    def test_oversized_stderr_is_terminated_before_the_child_continues(self) -> None:
        fixture = AuditFixture(clean_report())
        fixture.replace_audit_executable(
            "#!/usr/bin/env python3\n"
            "import os, pathlib, sys, time\n"
            "chunk = b'x' * 65536\n"
            "for _ in range(32):\n"
            "    sys.stderr.buffer.write(chunk)\n"
            "    sys.stderr.buffer.flush()\n"
            "    time.sleep(0.001)\n"
            "pathlib.Path(os.environ['AUDIT_CONTINUED_LOG']).write_text('continued')\n"
            "time.sleep(30)\n"
        )
        try:
            result = fixture.run()
            self.assertEqual(
                result.returncode,
                1,
                f"oversized cargo-audit stderr did not fail: stdout={result.stdout!r} stderr={result.stderr!r}",
            )
            self.assertIn("stderr exceeded the 1 MiB bound", result.stderr)
            self.assertFalse(
                fixture.audit_continued_log.exists(),
                f"cargo-audit continued after crossing the stderr bound: stdout={result.stdout!r} stderr={result.stderr!r}",
            )
        finally:
            fixture.close()

    def test_nonterminating_audit_is_stopped_by_the_explicit_deadline(self) -> None:
        fixture = AuditFixture(clean_report())
        fixture.replace_audit_executable(
            "#!/usr/bin/env python3\n"
            "import os, pathlib, time\n"
            "time.sleep(30)\n"
            "pathlib.Path(os.environ['AUDIT_CONTINUED_LOG']).write_text('continued')\n"
        )
        try:
            result = fixture.run(timeout_seconds=0.1)
            self.assertEqual(
                result.returncode,
                1,
                f"nonterminating cargo-audit did not fail: stdout={result.stdout!r} stderr={result.stderr!r}",
            )
            self.assertIn("exceeded the 0.1-second timeout", result.stderr)
            self.assertFalse(
                fixture.audit_continued_log.exists(),
                f"cargo-audit continued after its deadline: stdout={result.stdout!r} stderr={result.stderr!r}",
            )
        finally:
            fixture.close()


if __name__ == "__main__":
    unittest.main(verbosity=2)
