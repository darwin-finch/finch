#!/usr/bin/env python3
"""Production-boundary regression for locked Cargo metadata validation."""

from __future__ import annotations

import os
import shlex
import subprocess
import tempfile
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parent.parent
TOOLCHAIN_CONTRACT = ROOT / "tests/toolchain_contract.sh"


class LockedMetadataContractTests(unittest.TestCase):
    def test_metadata_only_contract_executes_locked_resolution_once(self) -> None:
        with tempfile.TemporaryDirectory(prefix="finch-locked-metadata-") as temporary:
            fixture = Path(temporary)
            tool_directory = fixture / "bin"
            tool_directory.mkdir()
            cargo_invocations = fixture / "cargo-invocations"

            wrappers = {
                "python3": "#!/bin/sh\nexit 0\n",
                "rustc": "#!/bin/sh\necho rustc 1.98.0 fixture\n",
                "cargo": (
                    "#!/bin/sh\n"
                    f"printf 'CALL\\0' >> {shlex.quote(str(cargo_invocations))}\n"
                    "for argument in \"$@\"; do\n"
                    f"  printf '%s\\0' \"$argument\" >> "
                    f"{shlex.quote(str(cargo_invocations))}\n"
                    "done\n"
                    f"printf 'END\\0' >> {shlex.quote(str(cargo_invocations))}\n"
                    "if test \"${FINCH_FAKE_CARGO_FAIL:-0}\" = 1; then\n"
                    "  echo 'fixture locked metadata failure' >&2\n"
                    "  exit 42\n"
                    "fi\n"
                ),
            }
            for name, body in wrappers.items():
                executable = tool_directory / name
                executable.write_text(body, encoding="utf-8")
                executable.chmod(0o700)

            environment = os.environ.copy()
            environment.pop("FINCH_FAKE_CARGO_FAIL", None)
            environment["PATH"] = f"{tool_directory}{os.pathsep}{environment['PATH']}"

            def run_contract(run_environment: dict[str, str]) -> subprocess.CompletedProcess[str]:
                return subprocess.run(
                    ["bash", str(TOOLCHAIN_CONTRACT), "--metadata-only"],
                    cwd=ROOT,
                    env=run_environment,
                    check=False,
                    capture_output=True,
                    text=True,
                )

            result = run_contract(environment)

            self.assertEqual(
                result.returncode,
                0,
                "metadata-only toolchain contract must execute with controlled tool "
                f"boundaries: stdout={result.stdout!r} stderr={result.stderr!r}",
            )
            self.assertTrue(
                cargo_invocations.is_file(),
                "toolchain contract must invoke Cargo; no invocation log was created: "
                f"stdout={result.stdout!r} stderr={result.stderr!r}",
            )
            expected_invocation = [
                b"CALL",
                b"metadata",
                b"--locked",
                b"--no-deps",
                b"--format-version",
                b"1",
                b"END",
                b"",
            ]
            self.assertEqual(
                cargo_invocations.read_bytes().split(b"\0"),
                expected_invocation,
                "toolchain contract must execute exactly one locked metadata resolution "
                "with lossless argument boundaries before accepting Cargo.toml and Cargo.lock",
            )

            cargo_invocations.unlink()
            failing_environment = environment.copy()
            failing_environment["FINCH_FAKE_CARGO_FAIL"] = "1"
            failure = run_contract(failing_environment)
            self.assertEqual(
                failure.returncode,
                42,
                "toolchain contract must propagate the exact failed locked metadata status: "
                f"stdout={failure.stdout!r} stderr={failure.stderr!r}",
            )
            self.assertIn(
                "fixture locked metadata failure",
                failure.stderr,
                "failed locked resolution must preserve Cargo diagnostics: "
                f"returncode={failure.returncode} stderr={failure.stderr!r}",
            )
            self.assertEqual(
                cargo_invocations.read_bytes().split(b"\0"),
                expected_invocation,
                "failed locked resolution must execute exactly once with no unlocked or "
                "retry fallback",
            )


if __name__ == "__main__":
    unittest.main()
