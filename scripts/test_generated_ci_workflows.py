#!/usr/bin/env python3
"""Production-boundary regressions for generated CI workflow authority."""

from __future__ import annotations

import contextlib
import copy
import hashlib
import importlib.util
import io
import json
import os
import re
import shutil
import subprocess
import tempfile
import time
import unittest
from pathlib import Path
from unittest import mock


ROOT = Path(__file__).resolve().parent.parent
CHECKER = ROOT / "scripts/check_generated_ci_workflows.py"
FIXTURE_DIRECTORY = Path("tests/fixtures/ci-generated-workflows")
SOURCE_NAME = "superseded-run-envelope.json"
OUTPUT_NAME = "superseded-run-envelope.yml"
CHECKER_SPEC = importlib.util.spec_from_file_location("generated_workflow_checker", CHECKER)
if CHECKER_SPEC is None or CHECKER_SPEC.loader is None:
    raise RuntimeError(f"could not load generated workflow checker from {CHECKER}")
CHECKER_MODULE = importlib.util.module_from_spec(CHECKER_SPEC)
CHECKER_SPEC.loader.exec_module(CHECKER_MODULE)
with CHECKER_MODULE.GitSnapshot(ROOT) as INITIAL_SNAPSHOT:
    INITIAL_BLOBS = INITIAL_SNAPSHOT.load()
SOURCE_BYTES = INITIAL_BLOBS[str(FIXTURE_DIRECTORY / SOURCE_NAME)]
TOOLCHAIN = INITIAL_BLOBS["tests/toolchain_contract.sh"]
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
ATTRIBUTES = b'''.github/workflows/*.yml text eol=lf
.github/workflows/*.yaml text eol=lf
tests/fixtures/ci-generated-workflows/*.json text eol=lf
tests/fixtures/ci-generated-workflows/*.yml text eol=lf
'''
STOPPED_EXTRACT_SOURCE = '''def extract_controller(workflow: str | None = None) -> str:
    text = WORKFLOW.read_text() if workflow is None else workflow
    match = re.search(
        r"^          python3 - <<'PYTHON'\\n(?P<script>.*?)^          PYTHON$",
        text,
        re.MULTILINE | re.DOTALL,
    )
    if match is None:
        raise AssertionError("workflow must contain one literal PYTHON heredoc controller")
    lines = match.group("script").splitlines()
    if any(line and not line.startswith("          ") for line in lines):
        raise AssertionError("controller heredoc indentation drifted from the executable script")
    return "\\n".join(line[10:] for line in lines) + "\\n"
'''
STOPPED_VALIDATOR_SOURCE = '''def validate_workflow_contract(text: str) -> None:
    lines = {line.strip() for line in text.splitlines()}
    required_lines = {
        "workflows: [CI]",
        "types: [requested, in_progress]",
        "actions: write",
        "pull-requests: read",
        "runs-on: ubuntu-24.04",
        "timeout-minutes: 5",
        "if: github.event.action == 'requested' || github.event.workflow_run.run_attempt > 1",
        "PAGE_SIZE = 100",
        "MAX_CANDIDATES = 50",
        '"branch": branch,',
        '"per_page": PAGE_SIZE,',
        '"Authorization": f"Bearer {token}",',
    }
    forbidden = (
        "concurrency:",
        "actions/checkout",
        "actions/cache",
        "artifact",
        "secrets.",
        "github.event.workflow_run.head_",
    )
    missing = sorted(required_lines - lines)
    present = [item for item in forbidden if item in text]
    job_if_count = len(re.findall(r"^    if:", text, re.MULTILINE))
    if missing or present or text.count("runs-on:") != 1 or job_if_count != 1:
        raise AssertionError(
            "trusted cancellation workflow contract drifted: "
            f"missing={missing!r} forbidden={present!r} "
            f"jobs={text.count('runs-on:')} job_if_count={job_if_count}"
        )
    compile(extract_controller(text), str(WORKFLOW), "exec")
'''
STOPPED_SOURCE_SHA256 = "78988be25406c7b69ad614a5e97827f4f0f46754a937a133e68b4fb6244ef1d9"


def canonical_json(value: object) -> bytes:
    return (json.dumps(value, ensure_ascii=False, indent=2, sort_keys=True) + "\n").encode()


class GeneratedWorkflowBoundaryTests(unittest.TestCase):
    @contextlib.contextmanager
    def repository(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            subprocess.run(
                ["git", "init", "-q", str(root)],
                capture_output=True,
                timeout=10,
                check=True,
            )
            fixtures = root / FIXTURE_DIRECTORY
            fixtures.mkdir(parents=True)
            (root / ".gitattributes").write_bytes(ATTRIBUTES)
            toolchain = root / "tests/toolchain_contract.sh"
            toolchain.write_bytes(TOOLCHAIN)
            toolchain.chmod(0o755)
            (fixtures / SOURCE_NAME).write_bytes(SOURCE_BYTES)
            (fixtures / OUTPUT_NAME).write_bytes(GOLDEN_YAML)
            self.commit(root, amend=False)
            yield root

    def commit(self, root: Path, *, amend: bool = True) -> None:
        add = subprocess.run(
            ["git", "-C", str(root), "add", "-A"],
            capture_output=True,
            timeout=10,
            check=False,
        )
        self.assertEqual(
            add.returncode,
            0,
            f"test repository staging failed: stderr_prefix={add.stderr[:4096]!r}",
        )
        command = [
            "git",
            "-C",
            str(root),
            "-c",
            "user.name=Finch Test",
            "-c",
            "user.email=finch@test.invalid",
            "commit",
        ]
        command.extend(["--amend", "--no-edit"] if amend else ["-m", "fixture"])
        result = subprocess.run(command, capture_output=True, timeout=10, check=False)
        self.assertEqual(
            result.returncode,
            0,
            f"test repository commit failed: stdout_prefix={result.stdout[:4096]!r} "
            f"stderr_prefix={result.stderr[:4096]!r}",
        )

    def commit_raw_blob(self, root: Path, relative: str, contents: bytes) -> None:
        hashed = subprocess.run(
            ["git", "-C", str(root), "hash-object", "-w", "--stdin"],
            input=contents,
            capture_output=True,
            timeout=10,
            check=False,
        )
        self.assertEqual(
            hashed.returncode,
            0,
            f"raw blob hashing failed: stderr_prefix={hashed.stderr[:4096]!r}",
        )
        oid = hashed.stdout.decode("ascii", "strict").strip()
        self.assertRegex(oid, r"\A[0-9a-f]{40,64}\Z", f"raw blob returned invalid OID: {oid!r}")
        updated = subprocess.run(
            ["git", "-C", str(root), "update-index", "--cacheinfo", f"100644,{oid},{relative}"],
            capture_output=True,
            timeout=10,
            check=False,
        )
        self.assertEqual(
            updated.returncode,
            0,
            f"raw blob index update failed: stderr_prefix={updated.stderr[:4096]!r}",
        )
        command = [
            "git",
            "-C",
            str(root),
            "-c",
            "user.name=Finch Test",
            "-c",
            "user.email=finch@test.invalid",
            "commit",
            "--amend",
            "--no-edit",
        ]
        committed = subprocess.run(command, capture_output=True, timeout=10, check=False)
        self.assertEqual(
            committed.returncode,
            0,
            f"raw blob commit failed: stderr_prefix={committed.stderr[:4096]!r}",
        )

    def run_checker(
        self,
        root: Path,
        *arguments: str,
        commit_changes: bool = True,
    ) -> subprocess.CompletedProcess[bytes]:
        if commit_changes:
            status = subprocess.run(
                ["git", "-C", str(root), "status", "--porcelain=v1"],
                capture_output=True,
                timeout=10,
                check=True,
            ).stdout
            if status:
                self.commit(root)
        return subprocess.run(
            ["python3", str(CHECKER), "--root", str(root), *arguments],
            capture_output=True,
            timeout=10,
            check=False,
        )

    def assert_success(self, result: subprocess.CompletedProcess[bytes], context: str) -> None:
        stdout = result.stdout[:4096]
        stderr = result.stderr[:4096]
        self.assertEqual(
            result.returncode,
            0,
            f"{context}: checker unexpectedly failed; stdout_prefix={stdout!r} "
            f"stderr_prefix={stderr!r} stdout_size={len(result.stdout)} stderr_size={len(result.stderr)}",
        )

    def assert_failure(
        self,
        result: subprocess.CompletedProcess[bytes],
        context: str,
        *diagnostics: bytes,
    ) -> None:
        stdout = result.stdout[:4096]
        stderr = result.stderr[:4096]
        self.assertNotEqual(
            result.returncode,
            0,
            f"{context}: checker accepted authority drift; stdout_prefix={stdout!r} "
            f"stderr_prefix={stderr!r} stdout_size={len(result.stdout)} stderr_size={len(result.stderr)}",
        )
        self.assertIn(
            b"generated CI workflow verification failed:",
            result.stderr,
            f"{context}: failure was not actionable; stderr_prefix={stderr!r} "
            f"stderr_size={len(result.stderr)}",
        )
        for diagnostic in diagnostics:
            self.assertIn(
                diagnostic,
                result.stderr,
                f"{context}: missing diagnostic {diagnostic!r}; stderr_prefix={stderr!r} "
                f"stderr_size={len(result.stderr)}",
            )

    def source_path(self, root: Path) -> Path:
        return root / FIXTURE_DIRECTORY / SOURCE_NAME

    def output_path(self, root: Path) -> Path:
        return root / FIXTURE_DIRECTORY / OUTPUT_NAME

    def model(self) -> dict:
        return json.loads(SOURCE_BYTES)

    def write_model(self, root: Path, model: dict) -> None:
        self.source_path(root).write_bytes(canonical_json(model))

    def snapshot_blobs(self, root: Path) -> dict[str, bytes]:
        with CHECKER_MODULE.GitSnapshot(root) as snapshot:
            return snapshot.load()

    def test_checked_in_fixture_matches_independently_pinned_golden(self):
        checked_in = INITIAL_BLOBS[str(FIXTURE_DIRECTORY / OUTPUT_NAME)]
        self.assertEqual(
            checked_in,
            GOLDEN_YAML,
            "checked-in generated workflow fixture drifted from the independently pinned golden YAML; "
            f"fixture_size={len(checked_in)} fixture_sha256={hashlib.sha256(checked_in).hexdigest()}",
        )
        real_result = self.run_checker(ROOT, commit_changes=False)
        self.assert_success(real_result, "real checked-in toolchain and fixture contract")
        with self.repository() as root:
            result = self.run_checker(root)
            self.assert_success(result, "independently pinned valid fixture")
            self.assertIn(
                b"verified 1 generated CI workflow fixture pair(s)",
                result.stdout,
                f"valid fixture did not report exact catalog count; stdout_prefix={result.stdout[:4096]!r}",
            )

    def test_render_reproduces_exact_golden_without_rewriting_output(self):
        with self.repository() as root:
            before = self.snapshot_blobs(root)[str(FIXTURE_DIRECTORY / OUTPUT_NAME)]
            result = self.run_checker(
                root,
                "--render",
                str(FIXTURE_DIRECTORY / SOURCE_NAME),
            )
            self.assert_success(result, "explicit render reproduction")
            self.assertEqual(
                result.stdout,
                GOLDEN_YAML,
                "render command did not reproduce pinned YAML; "
                f"stdout_size={len(result.stdout)} stdout_sha256={hashlib.sha256(result.stdout).hexdigest()}",
            )
            self.assertEqual(
                self.snapshot_blobs(root)[str(FIXTURE_DIRECTORY / OUTPUT_NAME)],
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
                if label == "CRLF":
                    self.commit_raw_blob(root, str(FIXTURE_DIRECTORY / OUTPUT_NAME), mutation)
                result = self.run_checker(root, commit_changes=label != "CRLF")
                self.assert_failure(result, label, OUTPUT_NAME.encode())
                self.assertIn(
                    b"source=tests/fixtures/ci-generated-workflows/superseded-run-envelope.json",
                    result.stderr,
                    f"{label}: byte mismatch did not name source; stderr_prefix={result.stderr[:4096]!r}",
                )
                for diagnostic in (b"first_difference_byte=", b"line=", b"expected_size=", b"actual_size=", b"reproduce:"):
                    self.assertIn(
                        diagnostic,
                        result.stderr,
                            f"{label}: byte mismatch omitted {diagnostic!r}; "
                        f"stderr_prefix={result.stderr[:4096]!r}",
                    )

    def test_difference_diagnostics_report_exact_byte_line_and_column(self):
        cases = {
            "first byte": (
                b"x" + GOLDEN_YAML[1:],
                (0, 1, 1),
            ),
            "appended byte": (
                GOLDEN_YAML + b"x",
                (len(GOLDEN_YAML), GOLDEN_YAML.count(b"\n") + 1, 1),
            ),
        }
        for label, (mutation, location) in cases.items():
            with self.subTest(label=label), self.repository() as root:
                self.output_path(root).write_bytes(mutation)
                result = self.run_checker(root)
                offset, line, column = location
                self.assert_failure(
                    result,
                    label,
                    f"first_difference_byte={offset}".encode(),
                    f"line={line}".encode(),
                    f"column={column}".encode(),
                )

    def test_exact_stopped_validator_accepts_whitespace_duplicate_but_checker_rejects_it(self):
        stopped_source = STOPPED_EXTRACT_SOURCE + "\n\n" + STOPPED_VALIDATOR_SOURCE
        self.assertEqual(
            hashlib.sha256(stopped_source.encode()).hexdigest(),
            STOPPED_SOURCE_SHA256,
            "vendored stopped-validator source drifted from exact commit 3ed522a8 lines 28-76",
        )
        namespace = {"re": re, "WORKFLOW": Path("stopped-ci-superseded-run-cancellation.yml")}
        exec(stopped_source, namespace)
        stopped_workflow = '''workflows: [CI]
types: [requested, in_progress]
actions: write
pull-requests: read
    if: github.event.action == 'requested' || github.event.workflow_run.run_attempt > 1
    if : false
    runs-on: ubuntu-24.04
timeout-minutes: 5
PAGE_SIZE = 100
MAX_CANDIDATES = 50
"branch": branch,
"per_page": PAGE_SIZE,
"Authorization": f"Bearer {token}",
          python3 - <<'PYTHON'
          pass
          PYTHON'''
        namespace["validate_workflow_contract"](stopped_workflow)

        condition = (
            b"    if: ${{ github.event_name == 'workflow_dispatch' || github.event.action == "
            b"'requested' || github.event.workflow_run.run_attempt > 1 }}"
        )
        mutation = GOLDEN_YAML.replace(condition, condition + b"\n    if : false")
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
        script_expression = self.model()
        script_expression["workflow"]["script"]["body"] = (
            'print("${{ secrets.ADMIN }}")\n'
        )
        cases["script-body expression"] = script_expression
        secret = self.model()
        secret["workflow"]["environment"]["TOKEN"] = "secrets.ADMIN"
        cases["arbitrary environment"] = secret
        swapped_environment = self.model()
        swapped_environment["workflow"]["environment"] = {
            "CONTINUATION_CURSOR": "github.token",
            "TOKEN": "inputs.continuation_cursor",
        }
        cases["swapped token binding"] = swapped_environment
        token_alias = self.model()
        token_alias["workflow"]["environment"]["SECOND_TOKEN"] = "github.token"
        cases["second token alias"] = token_alias
        renamed_environment = self.model()
        renamed_environment["workflow"]["environment"] = {
            "CURSOR": "inputs.continuation_cursor",
            "REPOSITORY_TOKEN": "github.token",
        }
        cases["renamed authority environment"] = renamed_environment
        malformed_environment = self.model()
        malformed_environment["workflow"]["environment"]["TOKEN"] = ["github.token"]
        cases["unhashable environment value"] = malformed_environment
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

    def test_runner_and_workflow_run_values_are_validated_not_only_rendered(self):
        cases: list[tuple[str, dict, bytes]] = []
        runner = self.model()
        runner["workflow"]["runner"] = "ubuntu-latest"
        cases.append(("runner", runner, GOLDEN_YAML))
        types = self.model()
        types["workflow"]["workflow_run"]["types"] = ["completed", "in_progress"]
        cases.append(
            (
                "workflow_run types",
                types,
                GOLDEN_YAML.replace(b'      - "requested"', b'      - "completed"'),
            )
        )
        workflows = self.model()
        workflows["workflow"]["workflow_run"]["workflows"] = ["Untrusted"]
        cases.append(
            (
                "workflow_run workflows",
                workflows,
                GOLDEN_YAML.replace(b'      - "CI"', b'      - "Untrusted"'),
            )
        )
        for label, model, coordinated_output in cases:
            with self.subTest(label=label), self.repository() as root:
                self.write_model(root, model)
                self.output_path(root).write_bytes(coordinated_output)
                result = self.run_checker(root)
                self.assert_failure(result, label, label.split()[0].encode())

    def test_yaml_line_break_controls_and_lone_surrogate_fail_actionably(self):
        cases = {
            "NEL in script": "import sys\n# \u0085injected: true\nraise SystemExit(1)\n",
            "line separator in script": "import sys\n# \u2028injected: true\nraise SystemExit(1)\n",
            "paragraph separator in script": "import sys\n# \u2029injected: true\nraise SystemExit(1)\n",
            "lone surrogate in script": "import sys\n# \ud800\nraise SystemExit(1)\n",
        }
        for label, body in cases.items():
            with self.subTest(label=label), self.repository() as root:
                model = self.model()
                model["workflow"]["script"]["body"] = body
                encoded = (
                    json.dumps(model, ensure_ascii=True, indent=2, sort_keys=True) + "\n"
                ).encode("ascii")
                self.source_path(root).write_bytes(encoded)
                result = self.run_checker(root)
                self.assert_failure(
                    result,
                    label,
                    b"workflow.script.body",
                    b"forbidden YAML/control code point",
                )

    def test_catalog_count_and_path_drift_fail_closed(self):
        with self.repository() as root:
            (root / FIXTURE_DIRECTORY / "unreviewed.yml").write_bytes(b"name: hidden\n")
            result = self.run_checker(root)
            self.assert_failure(
                result, "extra catalog entry", b"path/count drifted", b"actual_count_at_least=3"
            )
        with self.repository() as root:
            self.output_path(root).rename(root / FIXTURE_DIRECTORY / "renamed.yml")
            result = self.run_checker(root)
            self.assert_failure(result, "renamed catalog output", b"path/count drifted", b"renamed.yml")
        with self.repository() as root:
            self.source_path(root).unlink()
            result = self.run_checker(root)
            self.assert_failure(result, "missing catalog source", b"path/count drifted", SOURCE_NAME.encode())

    def test_unsafe_file_shapes_and_bytes_fail_closed(self):
        cases = ("symlink", "oversize", "non-UTF-8", "CR", "NUL")
        for label in cases:
            with self.subTest(label=label), self.repository() as root:
                output = self.output_path(root)
                if label == "symlink":
                    target = root / "outside.yml"
                    target.write_bytes(GOLDEN_YAML)
                    output.unlink()
                    output.symlink_to(target)
                elif label == "oversize":
                    output.write_bytes(b"x" * (128 * 1024 + 1))
                elif label == "non-UTF-8":
                    output.write_bytes(b"\xff")
                elif label == "CR":
                    output.write_bytes(GOLDEN_YAML.replace(b"\n", b"\r\n"))
                elif label == "NUL":
                    output.write_bytes(GOLDEN_YAML + b"\0")
                if label == "CR":
                    self.commit_raw_blob(
                        root,
                        str(FIXTURE_DIRECTORY / OUTPUT_NAME),
                        GOLDEN_YAML.replace(b"\n", b"\r\n"),
                    )
                result = self.run_checker(root, commit_changes=label != "CR")
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
                if label == "CR source":
                    self.commit_raw_blob(root, str(FIXTURE_DIRECTORY / SOURCE_NAME), value)
                result = self.run_checker(root, commit_changes=label != "CR source")
                self.assert_failure(result, label, SOURCE_NAME.encode())
        with self.repository() as root:
            real_directory = root / "real-fixtures"
            shutil.move(root / FIXTURE_DIRECTORY, real_directory)
            (root / FIXTURE_DIRECTORY).symlink_to(real_directory, target_is_directory=True)
            result = self.run_checker(root)
            self.assert_failure(result, "symlinked fixture directory", b"authority tree path/count drifted")

    def test_catalog_enumeration_stops_at_first_overpopulation_evidence(self):
        with self.repository() as root:
            for index in range(100):
                (root / FIXTURE_DIRECTORY / f"unreviewed-{index:03}.yml").write_bytes(b"name: hidden\n")
            result = self.run_checker(root)
            self.assert_failure(
                result,
                "overpopulated catalog",
                b"expected_count=2",
                b"actual_count_at_least=",
                b"unknown_sample=",
            )

    def test_lf_attributes_are_exactly_once_and_effective(self):
        mutations = {
            "missing JSON LF rule": ATTRIBUTES.replace(
                b"tests/fixtures/ci-generated-workflows/*.json text eol=lf\n", b""
            ),
            "changed YAML LF rule": ATTRIBUTES.replace(
                b"tests/fixtures/ci-generated-workflows/*.yml text eol=lf",
                b"tests/fixtures/ci-generated-workflows/*.yml -text",
            ),
            "duplicate JSON LF rule": ATTRIBUTES
            + b"tests/fixtures/ci-generated-workflows/*.json text eol=lf\n",
            "later override": ATTRIBUTES + b"tests/fixtures/ci-generated-workflows/*.yml -text\n",
        }
        for label, value in mutations.items():
            with self.subTest(label=label), self.repository() as root:
                (root / ".gitattributes").write_bytes(value)
                result = self.run_checker(root)
                self.assert_failure(result, label, b"Git attributes", b"eol=lf")
        with self.repository() as root:
            with (root / ".gitattributes").open("ab") as stream:
                stream.write(b"unrelated/*.txt text eol=lf\n")
            result = self.run_checker(root)
            self.assert_success(result, "unrelated later Git attribute")

    def test_git_snapshot_requires_nofollow_nonblock_directory_anchor(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            for flag in ("O_NOFOLLOW", "O_NONBLOCK", "O_DIRECTORY"):
                with self.subTest(flag=flag), mock.patch.object(CHECKER_MODULE.os, flag, 0):
                    with self.assertRaisesRegex(
                        CHECKER_MODULE.ContractError,
                        "unavailable flags",
                        msg=f"Git snapshot silently weakened its repository anchor without {flag}",
                    ):
                        with CHECKER_MODULE.GitSnapshot(root):
                            pass

    def test_immutable_head_snapshot_ignores_swapped_live_source_output_and_catalog(self):
        with self.repository() as root:
            with CHECKER_MODULE.GitSnapshot(root) as snapshot:
                before = snapshot.load()
                changed = self.model()
                changed["workflow"]["name"] = "Swapped live source"
                self.write_model(root, changed)
                self.output_path(root).write_bytes(b"malicious resident output\n")
                (root / FIXTURE_DIRECTORY / "late.yml").write_bytes(b"unreviewed\n")
                outside = root / "swapped-live-fixtures"
                (root / FIXTURE_DIRECTORY).rename(outside)
                (root / FIXTURE_DIRECTORY).symlink_to(outside, target_is_directory=True)
                after = snapshot.load()
                self.assertEqual(
                    after,
                    before,
                    "live source/output/catalog replacements changed the immutable HEAD tree snapshot",
                )
            result = self.run_checker(root, commit_changes=False)
            self.assert_success(
                result,
                "uncommitted hostile worktree data is outside the checked-in HEAD authority domain",
            )

    def test_fifo_worktree_replacement_fails_without_waiting_for_writer(self):
        with self.repository() as root:
            output = self.output_path(root)
            output.unlink()
            os.mkfifo(output)
            started = time.monotonic()
            result = self.run_checker(root, commit_changes=False)
            elapsed = time.monotonic() - started
            self.assert_success(
                result,
                "FIFO worktree replacement must not affect immutable checked-in authority",
            )
            self.assertLess(
                elapsed,
                2.0,
                f"Git snapshot blocked on FIFO instead of inspecting tracked metadata; elapsed={elapsed:.3f}s",
            )

    def test_toolchain_wiring_is_exactly_once_and_precedes_cargo_metadata(self):
        executable_pair = (
            b"python3 scripts/test_generated_ci_workflows.py\n"
            b"python3 scripts/check_generated_ci_workflows.py\n"
        )
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
            "both invocations inside false branch": TOOLCHAIN.replace(
                executable_pair,
                b"if false; then\n" + executable_pair + b"fi\n",
            ),
            "checker behind false and": TOOLCHAIN.replace(
                b"python3 scripts/check_generated_ci_workflows.py",
                b"false && python3 scripts/check_generated_ci_workflows.py",
            ),
            "test behind false and": TOOLCHAIN.replace(
                b"python3 scripts/test_generated_ci_workflows.py",
                b"false && python3 scripts/test_generated_ci_workflows.py",
            ),
        }
        for label, value in mutations.items():
            with self.subTest(label=label), self.repository() as root:
                (root / "tests/toolchain_contract.sh").write_bytes(value)
                result = self.run_checker(root)
                self.assert_failure(result, label, b"canonical toolchain gate")


if __name__ == "__main__":
    unittest.main()
