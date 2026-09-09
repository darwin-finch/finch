#!/usr/bin/env python3
"""Production-boundary regressions for generated CI workflow authority."""

from __future__ import annotations

import contextlib
import copy
import json
import os
import re
import shutil
import subprocess
import tempfile
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parent.parent
CHECKER = ROOT / "scripts/check_generated_ci_workflows.py"
FIXTURE_DIRECTORY = Path("tests/fixtures/ci-generated-workflows")
SOURCE_NAME = "superseded-run-envelope.json"
OUTPUT_NAME = "superseded-run-envelope.yml"
SOURCE_BYTES = (ROOT / FIXTURE_DIRECTORY / SOURCE_NAME).read_bytes()
GOLDEN_YAML = b'''name: "Cancel superseded CI runs"

on:
  workflow_run:
    workflows:
      - "CI"
    types:
      - "requested"
      - "in_progress"
  workflow_dispatch:
    inputs:
      "continuation_cursor":
        description: "Opaque bounded cursor for the next trusted reconciliation pass"
        required: false
        default: ""
        type: string

permissions:
  actions: write
  pull-requests: read

jobs:
  "cancel-superseded":
    name: "Cancel superseded canonical CI runs"
    if: ${{ github.event_name == 'workflow_dispatch' || github.event.action == 'requested' || github.event.workflow_run.run_attempt > 1 }}
    runs-on: ubuntu-24.04
    timeout-minutes: 5
    steps:
      - name: "Run trusted bounded reconciliation controller"
        env:
          "CONTINUATION_CURSOR": ${{ inputs.continuation_cursor }}
          "TOKEN": ${{ github.token }}
        run: |
          python3 - <<'PYTHON'
          import sys
          print("inert authority-envelope fixture; no API calls", file=sys.stderr)
          raise SystemExit(1)
          PYTHON
'''
TOOLCHAIN = b'''#!/usr/bin/env bash
python3 scripts/test_generated_ci_workflows.py
python3 scripts/check_generated_ci_workflows.py
cargo metadata --locked --no-deps --format-version 1 >/dev/null
'''


def canonical_json(value: object) -> bytes:
    return (json.dumps(value, ensure_ascii=False, indent=2, sort_keys=True) + "\n").encode()


class GeneratedWorkflowBoundaryTests(unittest.TestCase):
    @contextlib.contextmanager
    def repository(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            fixtures = root / FIXTURE_DIRECTORY
            fixtures.mkdir(parents=True)
            (root / "tests/toolchain_contract.sh").write_bytes(TOOLCHAIN)
            (fixtures / SOURCE_NAME).write_bytes(SOURCE_BYTES)
            (fixtures / OUTPUT_NAME).write_bytes(GOLDEN_YAML)
            yield root

    def run_checker(self, root: Path, *arguments: str) -> subprocess.CompletedProcess[bytes]:
        return subprocess.run(
            ["python3", str(CHECKER), "--root", str(root), *arguments],
            capture_output=True,
            timeout=10,
            check=False,
        )

    def assert_success(self, result: subprocess.CompletedProcess[bytes], context: str) -> None:
        self.assertEqual(
            result.returncode,
            0,
            f"{context}: checker unexpectedly failed; stdout={result.stdout!r} stderr={result.stderr!r}",
        )

    def assert_failure(
        self,
        result: subprocess.CompletedProcess[bytes],
        context: str,
        *diagnostics: bytes,
    ) -> None:
        self.assertNotEqual(
            result.returncode,
            0,
            f"{context}: checker accepted authority drift; stdout={result.stdout!r} stderr={result.stderr!r}",
        )
        self.assertIn(
            b"generated CI workflow verification failed:",
            result.stderr,
            f"{context}: failure was not actionable; stderr={result.stderr!r}",
        )
        for diagnostic in diagnostics:
            self.assertIn(
                diagnostic,
                result.stderr,
                f"{context}: missing diagnostic {diagnostic!r}; stderr={result.stderr!r}",
            )

    def source_path(self, root: Path) -> Path:
        return root / FIXTURE_DIRECTORY / SOURCE_NAME

    def output_path(self, root: Path) -> Path:
        return root / FIXTURE_DIRECTORY / OUTPUT_NAME

    def model(self) -> dict:
        return json.loads(SOURCE_BYTES)

    def write_model(self, root: Path, model: dict) -> None:
        self.source_path(root).write_bytes(canonical_json(model))

    def test_checked_in_fixture_matches_independently_pinned_golden(self):
        checked_in = (ROOT / FIXTURE_DIRECTORY / OUTPUT_NAME).read_bytes()
        self.assertEqual(
            checked_in,
            GOLDEN_YAML,
            "checked-in generated workflow fixture drifted from the independently pinned golden YAML; "
            f"fixture={checked_in!r}",
        )
        with self.repository() as root:
            result = self.run_checker(root)
            self.assert_success(result, "independently pinned valid fixture")
            self.assertIn(
                b"verified 1 generated CI workflow fixture pair(s)",
                result.stdout,
                f"valid fixture did not report exact catalog count; stdout={result.stdout!r}",
            )

    def test_render_reproduces_exact_golden_without_rewriting_output(self):
        with self.repository() as root:
            output = self.output_path(root)
            before = output.read_bytes()
            result = self.run_checker(
                root,
                "--render",
                str(FIXTURE_DIRECTORY / SOURCE_NAME),
            )
            self.assert_success(result, "explicit render reproduction")
            self.assertEqual(
                result.stdout,
                GOLDEN_YAML,
                f"render command did not reproduce pinned YAML; stdout={result.stdout!r}",
            )
            self.assertEqual(
                output.read_bytes(),
                before,
                "--render rewrote the checked output instead of emitting only to stdout",
            )

    def test_every_effective_yaml_or_byte_mutation_fails_actionably(self):
        condition = (
            b"    if: ${{ github.event_name == 'workflow_dispatch' || github.event.action == "
            b"'requested' || github.event.workflow_run.run_attempt > 1 }}"
        )
        mutations = {
            "duplicate if": GOLDEN_YAML.replace(condition, condition + b"\n    if: false"),
            "whitespace duplicate if": GOLDEN_YAML.replace(condition, condition + b"\n    if : false"),
            "duplicate actions": GOLDEN_YAML.replace(b"  actions: write", b"  actions: write\n  actions: read"),
            "whitespace duplicate actions": GOLDEN_YAML.replace(
                b"  actions: write", b"  actions: write\n  actions : read"
            ),
            "extra job": GOLDEN_YAML + b"  hidden-job:\n    runs-on: ubuntu-24.04\n",
            "changed trigger": GOLDEN_YAML.replace(b"  workflow_dispatch:", b"  push:"),
            "comment": b"# hand-edited authority\n" + GOLDEN_YAML,
            "explicit tag": GOLDEN_YAML.replace(
                b'name: "Cancel superseded CI runs"', b'name: !!str "Cancel superseded CI runs"'
            ),
            "anchor": GOLDEN_YAML.replace(b"permissions:", b"permissions: &authority"),
            "alias": GOLDEN_YAML + b"authority-copy: *authority\n",
            "alternate scalar": GOLDEN_YAML.replace(b"timeout-minutes: 5", b"timeout-minutes: 05"),
            "CRLF": GOLDEN_YAML.replace(b"\n", b"\r\n"),
            "truncation": GOLDEN_YAML[:-1],
            "appended byte": GOLDEN_YAML + b"x",
        }
        for label, mutation in mutations.items():
            with self.subTest(label=label), self.repository() as root:
                self.output_path(root).write_bytes(mutation)
                result = self.run_checker(root)
                self.assert_failure(result, label, OUTPUT_NAME.encode())
                if label != "CRLF":
                    self.assertIn(
                        b"source=tests/fixtures/ci-generated-workflows/superseded-run-envelope.json",
                        result.stderr,
                        f"{label}: byte mismatch did not name source; stderr={result.stderr!r}",
                    )
                    for diagnostic in (b"first_difference_byte=", b"line=", b"expected_size=", b"actual_size=", b"reproduce:"):
                        self.assertIn(
                            diagnostic,
                            result.stderr,
                            f"{label}: byte mismatch omitted {diagnostic!r}; stderr={result.stderr!r}",
                        )

    def test_stopped_line_validator_accepts_whitespace_duplicate_but_checker_rejects_it(self):
        condition = (
            b"    if: ${{ github.event_name == 'workflow_dispatch' || github.event.action == "
            b"'requested' || github.event.workflow_run.run_attempt > 1 }}"
        )
        mutation = GOLDEN_YAML.replace(condition, condition + b"\n    if : false")
        text = mutation.decode()
        stopped_validator_job_if_count = len(re.findall(r"^    if:", text, re.MULTILINE))
        stopped_validator_required_line_present = condition.decode().strip() in {
            line.strip() for line in text.splitlines()
        }
        self.assertTrue(
            stopped_validator_required_line_present and stopped_validator_job_if_count == 1,
            "preserved #533 line/regex validator reproduction no longer demonstrates its whitespace-key gap; "
            f"required={stopped_validator_required_line_present} count={stopped_validator_job_if_count}",
        )
        with self.repository() as root:
            self.output_path(root).write_bytes(mutation)
            result = self.run_checker(root)
            self.assert_failure(result, "preserved whitespace-equivalent duplicate-key regression", b"first_difference_byte=")

    def test_strict_json_rejects_duplicates_nonfinite_numbers_and_noncanonical_bytes(self):
        cases = {
            "nested duplicate key": SOURCE_BYTES.replace(
                b'        "type": "string"',
                b'        "type": "string",\n        "type": "string"',
            ),
            "NaN": SOURCE_BYTES.replace(b'"schema_version": 1', b'"schema_version": NaN'),
            "Infinity": SOURCE_BYTES.replace(b'"schema_version": 1', b'"schema_version": Infinity'),
            "uncanonical whitespace": SOURCE_BYTES.replace(b"{\n", b"{ \n", 1),
        }
        for label, source in cases.items():
            with self.subTest(label=label), self.repository() as root:
                self.source_path(root).write_bytes(source)
                result = self.run_checker(root)
                expected = b"not canonical JSON" if label == "uncanonical whitespace" else b"not strict JSON"
                self.assert_failure(result, label, expected, SOURCE_NAME.encode())

    def test_closed_schema_rejects_missing_unknown_broadened_and_arbitrary_authority(self):
        cases: dict[str, dict] = {}
        missing = self.model()
        del missing["workflow"]["runner"]
        cases["missing field"] = missing
        unknown = self.model()
        unknown["workflow"]["steps"] = []
        cases["unknown steps"] = unknown
        broad = self.model()
        broad["workflow"]["permissions"]["pull-requests"] = "write"
        cases["broadened permission"] = broad
        narrow = self.model()
        narrow["workflow"]["permissions"]["actions"] = "read"
        cases["narrowed required permission"] = narrow
        expression = self.model()
        expression["workflow"]["condition_policy"] = "${{ always() }}"
        cases["arbitrary expression"] = expression
        secret = self.model()
        secret["workflow"]["environment"]["TOKEN"] = "secrets.ADMIN"
        cases["arbitrary environment"] = secret
        trigger = self.model()
        trigger["workflow"]["workflow_run"]["types"] = ["completed"]
        cases["changed trigger"] = trigger
        second_input = self.model()
        second_input["workflow"]["dispatch_inputs"]["arbitrary"] = copy.deepcopy(
            second_input["workflow"]["dispatch_inputs"]["continuation_cursor"]
        )
        cases["extra dispatch input"] = second_input
        for label, model in cases.items():
            with self.subTest(label=label), self.repository() as root:
                self.write_model(root, model)
                result = self.run_checker(root)
                self.assert_failure(result, label, SOURCE_NAME.encode())

    def test_catalog_count_and_path_drift_fail_closed(self):
        with self.repository() as root:
            (root / FIXTURE_DIRECTORY / "unreviewed.yml").write_bytes(b"name: hidden\n")
            result = self.run_checker(root)
            self.assert_failure(result, "extra catalog entry", b"path/count drifted", b"actual_count=3")
        with self.repository() as root:
            self.output_path(root).rename(root / FIXTURE_DIRECTORY / "renamed.yml")
            result = self.run_checker(root)
            self.assert_failure(result, "renamed catalog output", b"path/count drifted", b"renamed.yml")
        with self.repository() as root:
            self.source_path(root).unlink()
            result = self.run_checker(root)
            self.assert_failure(result, "missing catalog source", b"path/count drifted", SOURCE_NAME.encode())

    def test_unsafe_file_shapes_and_bytes_fail_closed(self):
        cases = ("symlink", "nonregular", "oversize", "non-UTF-8", "CR", "NUL")
        for label in cases:
            with self.subTest(label=label), self.repository() as root:
                output = self.output_path(root)
                if label == "symlink":
                    target = root / "outside.yml"
                    target.write_bytes(GOLDEN_YAML)
                    output.unlink()
                    output.symlink_to(target)
                elif label == "nonregular":
                    output.unlink()
                    os.mkfifo(output)
                elif label == "oversize":
                    output.write_bytes(b"x" * (128 * 1024 + 1))
                elif label == "non-UTF-8":
                    output.write_bytes(b"\xff")
                elif label == "CR":
                    output.write_bytes(GOLDEN_YAML.replace(b"\n", b"\r\n"))
                elif label == "NUL":
                    output.write_bytes(GOLDEN_YAML + b"\0")
                result = self.run_checker(root)
                self.assert_failure(result, label, OUTPUT_NAME.encode())

    def test_source_file_bounds_and_parent_symlink_fail_closed(self):
        cases = {
            "oversize source": b" " * (64 * 1024 + 1),
            "non-UTF-8 source": b"\xff",
            "CR source": SOURCE_BYTES.replace(b"\n", b"\r\n"),
            "NUL source": SOURCE_BYTES + b"\0",
        }
        for label, value in cases.items():
            with self.subTest(label=label), self.repository() as root:
                self.source_path(root).write_bytes(value)
                result = self.run_checker(root)
                self.assert_failure(result, label, SOURCE_NAME.encode())
        with self.repository() as root:
            real_directory = root / "real-fixtures"
            shutil.move(root / FIXTURE_DIRECTORY, real_directory)
            (root / FIXTURE_DIRECTORY).symlink_to(real_directory, target_is_directory=True)
            result = self.run_checker(root)
            self.assert_failure(result, "symlinked fixture directory", b"must be a real directory")

    def test_toolchain_wiring_is_exactly_once_and_precedes_cargo_metadata(self):
        mutations = {
            "missing checker": TOOLCHAIN.replace(b"python3 scripts/check_generated_ci_workflows.py\n", b""),
            "duplicate checker": TOOLCHAIN.replace(
                b"python3 scripts/check_generated_ci_workflows.py\n",
                b"python3 scripts/check_generated_ci_workflows.py\n" * 2,
            ),
            "missing test": TOOLCHAIN.replace(b"python3 scripts/test_generated_ci_workflows.py\n", b""),
            "duplicate test": TOOLCHAIN.replace(
                b"python3 scripts/test_generated_ci_workflows.py\n",
                b"python3 scripts/test_generated_ci_workflows.py\n" * 2,
            ),
            "checker after cargo": TOOLCHAIN.replace(
                b"python3 scripts/check_generated_ci_workflows.py\n", b""
            )
            + b"python3 scripts/check_generated_ci_workflows.py\n",
        }
        for label, value in mutations.items():
            with self.subTest(label=label), self.repository() as root:
                (root / "tests/toolchain_contract.sh").write_bytes(value)
                result = self.run_checker(root)
                self.assert_failure(result, label, b"canonical toolchain gate")


if __name__ == "__main__":
    unittest.main()
