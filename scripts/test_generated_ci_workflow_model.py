#!/usr/bin/env python3
"""Mutation-sensitive regressions for the pure generated-workflow model."""

from __future__ import annotations

import ast
import copy
import hashlib
import json
import re
import unittest
from pathlib import Path

import generated_ci_workflow_model as model_module


PINNED_MAX_SOURCE_BYTES = 64 * 1024
PINNED_MAX_SCRIPT_BYTES = 48 * 1024
PINNED_MAX_OUTPUT_BYTES = 128 * 1024

SOURCE_BYTES = b'''{
  "schema_version": 1,
  "workflow": {
    "dispatch_inputs": {
      "continuation_cursor": {
        "default": "",
        "description": "Opaque bounded cursor for the next trusted reconciliation pass",
        "required": false,
        "type": "string"
      }
    },
    "environment": {
      "CONTINUATION_CURSOR": "inputs.continuation_cursor",
      "TOKEN": "github.token"
    },
    "execution_policy": "requested-and-all-in-progress-or-dispatch-v1",
    "job_id": "cancel-superseded",
    "job_name": "Cancel superseded canonical CI runs",
    "name": "Cancel superseded CI runs",
    "permissions": {
      "actions": "write",
      "pull-requests": "read"
    },
    "runner": "ubuntu-24.04",
    "script": {
      "body": "import sys\\nprint(\\"inert authority-envelope fixture; no API calls\\", file=sys.stderr)\\nraise SystemExit(1)\\n",
      "name": "Run trusted bounded reconciliation controller"
    },
    "timeout_minutes": 5,
    "workflow_run": {
      "types": [
        "requested",
        "in_progress"
      ],
      "workflows": [
        "CI"
      ]
    }
  }
}
'''

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
    runs-on: "ubuntu-24.04"
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

# Exact source from tests/test_cancel_superseded_ci_runs.py lines 28-76 at
# 3ed522a823cbc2abb80b6bebe0722babb53817dd. It is executed only to preserve
# fail-before provenance for the stopped implementation, never as new authority.
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


def canonical_json(value: object, *, ensure_ascii: bool = False) -> bytes:
    return (
        json.dumps(value, allow_nan=False, ensure_ascii=ensure_ascii, indent=2, sort_keys=True)
        + "\n"
    ).encode("ascii" if ensure_ascii else "utf-8")


class GeneratedCiWorkflowModelTests(unittest.TestCase):
    def model(self) -> dict:
        return json.loads(SOURCE_BYTES)

    def assert_rejected(
        self,
        changed: dict,
        diagnostic: str,
        coordinated_output: bytes,
        context: str,
        *,
        ensure_ascii: bool = False,
    ) -> None:
        source = canonical_json(changed, ensure_ascii=ensure_ascii)
        self.assertNotEqual(
            coordinated_output,
            GOLDEN_YAML,
            f"{context}: coordinated output mutation did not alter the authority envelope",
        )
        with self.assertRaisesRegex(
            model_module.ContractError,
            re.escape(diagnostic),
            msg=(
                f"{context}: canonical source mutation was accepted; "
                f"source_sha256={hashlib.sha256(source).hexdigest()} "
                f"coordinated_output_sha256={hashlib.sha256(coordinated_output).hexdigest()}"
            ),
        ):
            model_module.parse_and_render(source, source_name=f"{context}.json")
        with self.assertRaisesRegex(
            model_module.ContractError,
            re.escape(diagnostic),
            msg=f"{context}: decoded-object entry point bypassed the same contract guard",
        ):
            model_module.render_workflow(changed, source_name=context)

    def test_independently_pinned_source_renders_exact_independently_pinned_yaml(self):
        self.assertEqual(
            (
                model_module.MAX_SOURCE_BYTES,
                model_module.MAX_SCRIPT_BYTES,
                model_module.MAX_OUTPUT_BYTES,
            ),
            (PINNED_MAX_SOURCE_BYTES, PINNED_MAX_SCRIPT_BYTES, PINNED_MAX_OUTPUT_BYTES),
            "workflow model resource bounds drifted from the independently pinned contract",
        )
        parsed = model_module.parse_model(SOURCE_BYTES, source_name="pinned.json")
        before = copy.deepcopy(parsed)
        rendered = model_module.render_workflow(parsed, source_name="pinned model")
        self.assertEqual(
            rendered,
            GOLDEN_YAML,
            "valid canonical source did not render the independently pinned YAML bytes; "
            f"expected_sha256={hashlib.sha256(GOLDEN_YAML).hexdigest()} "
            f"actual_sha256={hashlib.sha256(rendered).hexdigest()}",
        )
        self.assertEqual(
            model_module.parse_and_render(SOURCE_BYTES),
            GOLDEN_YAML,
            "combined pure entry point did not reproduce the pinned envelope",
        )
        self.assertEqual(
            model_module.render_workflow(parsed),
            rendered,
            "repeated rendering was not deterministic over the same object",
        )
        self.assertEqual(
            parsed,
            before,
            f"pure rendering mutated its input model; before={before!r} after={parsed!r}",
        )

    def test_pinned_yaml_contains_one_executable_literal_python_body(self):
        matches = list(
            re.finditer(
                rb"^          python3 - <<'PYTHON'\n(?P<body>.*?)^          PYTHON$",
                GOLDEN_YAML,
                re.MULTILINE | re.DOTALL,
            )
        )
        self.assertEqual(
            len(matches),
            1,
            "pinned envelope must contain exactly one fixed literal PYTHON heredoc; "
            f"matches={len(matches)} yaml_sha256={hashlib.sha256(GOLDEN_YAML).hexdigest()}",
        )
        lines = matches[0].group("body").splitlines()
        self.assertFalse(
            [line for line in lines if line and not line.startswith(b"          ")],
            "rendered Python body escaped the YAML literal indentation",
        )
        body = b"\n".join(line[10:] for line in lines) + b"\n"
        compile(body.decode("utf-8", "strict"), "pinned-envelope.yml", "exec")
        self.assertNotIn(b"\r", GOLDEN_YAML, "pinned YAML contains a non-LF line ending")
        self.assertNotIn(b"\0", GOLDEN_YAML, "pinned YAML contains NUL")

    def test_execution_has_requested_all_in_progress_and_dispatch_without_job_condition(self):
        for required in (
            b'      - "requested"\n',
            b'      - "in_progress"\n',
            b"  workflow_dispatch:\n",
        ):
            self.assertIn(
                required,
                GOLDEN_YAML,
                f"pinned envelope omitted execution delivery {required!r}",
            )
        self.assertNotRegex(
            GOLDEN_YAML.decode(),
            r"(?m)^    if\s*:",
            "job-level filtering can skip initial/retry in_progress recovery deliveries",
        )

    def test_parser_rejects_unbounded_non_utf8_duplicate_noninteger_and_noncanonical_json(self):
        cases = {
            "empty": (b"", "must not be empty"),
            "oversize": (b" " * (PINNED_MAX_SOURCE_BYTES + 1), "input bound"),
            "non UTF-8": (b"\xff", "not strict UTF-8"),
            "duplicate": (
                SOURCE_BYTES.replace(
                    b'      "TOKEN": "github.token"',
                    b'      "TOKEN": "github.token",\n      "TOKEN": "github.token"',
                ),
                "duplicate key 'TOKEN'",
            ),
            "fraction": (SOURCE_BYTES.replace(b'"schema_version": 1', b'"schema_version": 1.0'), "must be integers"),
            "exponent": (SOURCE_BYTES.replace(b'"schema_version": 1', b'"schema_version": 1e0'), "must be integers"),
            "NaN": (SOURCE_BYTES.replace(b'"schema_version": 1', b'"schema_version": NaN'), "must be integers"),
            "Infinity": (
                SOURCE_BYTES.replace(b'"schema_version": 1', b'"schema_version": Infinity'),
                "must be integers",
            ),
            "CRLF": (SOURCE_BYTES.replace(b"\n", b"\r\n"), "not canonical"),
            "one-space indent": (SOURCE_BYTES.replace(b'  "schema_version"', b' "schema_version"'), "not canonical"),
            "missing LF": (SOURCE_BYTES[:-1], "not canonical"),
            "extra LF": (SOURCE_BYTES + b"\n", "not canonical"),
            "unsorted root": (
                SOURCE_BYTES.replace(
                    b'{\n  "schema_version": 1,\n  "workflow": {',
                    b'{\n  "workflow": {',
                ).replace(b"\n  }\n}\n", b'\n  },\n  "schema_version": 1\n}\n'),
                "not canonical",
            ),
        }
        for context, (source, diagnostic) in cases.items():
            with self.subTest(context=context), self.assertRaisesRegex(
                model_module.ContractError,
                re.escape(diagnostic),
                msg=f"{context}: malformed JSON source did not fail actionably",
            ):
                model_module.parse_model(source, source_name=f"{context}.json")

    def test_environment_bindings_are_exact_even_with_coordinated_output(self):
        cases = {
            "cursor bound to token": (
                {"CONTINUATION_CURSOR": "github.token", "TOKEN": "github.token"},
                GOLDEN_YAML.replace(
                    b"${{ inputs.continuation_cursor }}", b"${{ github.token }}", 1
                ),
            ),
            "token bound to a secret": (
                {"CONTINUATION_CURSOR": "inputs.continuation_cursor", "TOKEN": "secrets.ADMIN"},
                GOLDEN_YAML.replace(b"${{ github.token }}", b"${{ secrets.ADMIN }}"),
            ),
            "token and cursor swapped": (
                {"CONTINUATION_CURSOR": "github.token", "TOKEN": "inputs.continuation_cursor"},
                GOLDEN_YAML.replace(
                    b'          "CONTINUATION_CURSOR": ${{ inputs.continuation_cursor }}\n'
                    b'          "TOKEN": ${{ github.token }}',
                    b'          "CONTINUATION_CURSOR": ${{ github.token }}\n'
                    b'          "TOKEN": ${{ inputs.continuation_cursor }}',
                ),
            ),
            "second token alias": (
                {
                    "CONTINUATION_CURSOR": "inputs.continuation_cursor",
                    "SECOND_TOKEN": "github.token",
                    "TOKEN": "github.token",
                },
                GOLDEN_YAML.replace(
                    b'          "TOKEN": ${{ github.token }}',
                    b'          "SECOND_TOKEN": ${{ github.token }}\n          "TOKEN": ${{ github.token }}',
                ),
            ),
        }
        for context, (environment, output) in cases.items():
            with self.subTest(context=context):
                changed = self.model()
                changed["workflow"]["environment"] = environment
                self.assert_rejected(changed, "workflow.environment", output, context)

    def test_fixed_heredoc_terminator_and_script_scalars_are_rejected_coordinately(self):
        cases = {
            "fixed heredoc terminator first line": (
                "PYTHON\npass\n",
                "fixed PYTHON heredoc terminator",
                b"          PYTHON\n          pass",
            ),
            "fixed heredoc terminator middle line": (
                "print('before')\nPYTHON\nprint('after')\n",
                "fixed PYTHON heredoc terminator",
                b"          print('before')\n          PYTHON\n          print('after')",
            ),
            "fixed heredoc terminator last line": (
                "pass\nPYTHON\n",
                "fixed PYTHON heredoc terminator",
                b"          pass\n          PYTHON",
            ),
            "GitHub expression opening delimiter": (
                '# ${{ without a closing delimiter\npass\n',
                "forbidden GitHub expression delimiter",
                b"          # ${{ without a closing delimiter\n          pass",
            ),
            "GitHub expression closing delimiter": (
                "# unmatched }} delimiter\npass\n",
                "forbidden GitHub expression delimiter",
                b"          # unmatched }} delimiter\n          pass",
            ),
            "NUL": ("# nul:\x00\npass\n", "U+0000", b"          # nul:\x00\n          pass"),
            "CR": ("# carriage\rreturn\npass\n", "U+000D", b"          # carriage\rreturn\n          pass"),
            "tab": ("\tpass\n", "U+0009", b"          \tpass"),
            "DEL lower boundary": (
                "# del:\u007f\npass\n",
                "U+007F",
                "          # del:\u007f\n          pass".encode(),
            ),
            "C1 lower interior": (
                "# c1:\u0080\npass\n",
                "U+0080",
                "          # c1:\u0080\n          pass".encode(),
            ),
            "NEL": ("# nel:\u0085\npass\n", "U+0085", "          # nel:\u0085\n          pass".encode()),
            "C1 narrowed-mutant escape": (
                "# c1:\u0090\npass\n",
                "U+0090",
                "          # c1:\u0090\n          pass".encode(),
            ),
            "C1 upper boundary": (
                "# c1:\u009f\npass\n",
                "U+009F",
                "          # c1:\u009f\n          pass".encode(),
            ),
            "line separator": (
                "# line:\u2028\npass\n",
                "U+2028",
                "          # line:\u2028\n          pass".encode(),
            ),
            "paragraph separator": (
                "# paragraph:\u2029\npass\n",
                "U+2029",
                "          # paragraph:\u2029\n          pass".encode(),
            ),
            "noncharacter": (
                "# invalid:\uffff\npass\n",
                "U+FFFF",
                "          # invalid:\uffff\n          pass".encode(),
            ),
        }
        old_body = (
            b"          import sys\n"
            b'          print("inert authority-envelope fixture; no API calls", file=sys.stderr)\n'
            b"          raise SystemExit(1)"
        )
        for context, (body, diagnostic, rendered_body) in cases.items():
            with self.subTest(context=context):
                changed = self.model()
                changed["workflow"]["script"]["body"] = body
                output = GOLDEN_YAML.replace(old_body, rendered_body)
                self.assert_rejected(changed, diagnostic, output, context)

        changed = self.model()
        changed["workflow"]["script"]["body"] = "# surrogate:\ud800\npass\n"
        output = GOLDEN_YAML.replace(old_body, b"          # escaped surrogate: \\ud800\n          pass")
        self.assert_rejected(
            changed,
            "workflow.script.body",
            output,
            "lone surrogate",
            ensure_ascii=True,
        )

    def test_script_shape_and_bounds_are_enforced(self):
        cases = {
            "missing final LF": ("pass", "end with exactly one usable LF boundary"),
            "invalid Python": ("if:\n", "syntactically valid Python body"),
            "oversize body": ("#" + "x" * PINNED_MAX_SCRIPT_BYTES + "\n", "UTF-8 bytes"),
            "non-string body": ([], "must be a string"),
        }
        for context, (body, diagnostic) in cases.items():
            with self.subTest(context=context):
                changed = self.model()
                changed["workflow"]["script"]["body"] = body
                source = canonical_json(changed)
                with self.assertRaisesRegex(model_module.ContractError, re.escape(diagnostic)):
                    model_module.parse_and_render(source, source_name=f"{context}.json")

    def test_script_rejects_encoding_declarations_that_change_byte_execution(self):
        body = '# coding: ascii\nprint("café")\n'
        ast.parse(body, filename="decoded-controller.py", mode="exec")
        with self.assertRaises(
            SyntaxError,
            msg="probe no longer demonstrates decoded-text/UTF-8-byte execution drift",
        ):
            compile(body.encode("utf-8"), "executed-controller.py", "exec")

        for context, declared_body in (
            ("first physical line", body),
            ("second physical line", "#!/usr/bin/env python3\n" + body),
        ):
            with self.subTest(context=context):
                changed = self.model()
                changed["workflow"]["script"]["body"] = declared_body
                with self.assertRaisesRegex(
                    model_module.ContractError,
                    "forbidden Python encoding declaration",
                    msg=f"{context}: model accepted execution semantics different from reviewed text",
                ):
                    model_module.render_workflow(changed, source_name=context)

    def test_script_grammar_is_pinned_to_supported_python_3_9(self):
        body = "match value:\n    case 1:\n        pass\n"
        ast.parse(body, filename="current-host.py", mode="exec")
        changed = self.model()
        changed["workflow"]["script"]["body"] = body
        with self.assertRaisesRegex(
            model_module.ContractError,
            "syntactically valid Python body",
            msg="model accepted grammar unavailable on Finch's supported Python 3.9 floor",
        ):
            model_module.render_workflow(changed, source_name="python-3.9 grammar")

    def test_decoded_object_bounds_precede_encoding_and_diagnostics_stay_bounded(self):
        probes = []

        oversize_script = self.model()
        oversize_script["workflow"]["script"]["body"] = "x" * (2 * 1024 * 1024)
        probes.append(("oversize script", oversize_script, "cannot fit"))

        huge_integer = self.model()
        huge_integer["workflow"]["timeout_minutes"] = 1 << 40000
        probes.append(("huge integer", huge_integer, "bit_length=40001"))

        huge_unknown_key = self.model()
        huge_unknown_key["x" * (2 * 1024 * 1024)] = None
        probes.append(("huge unknown key", huge_unknown_key, "character_count=2097152"))

        for context, changed, diagnostic in probes:
            with self.subTest(context=context):
                with self.assertRaises(model_module.ContractError) as caught:
                    model_module.render_workflow(changed, source_name=context)
                message = str(caught.exception)
                self.assertIn(
                    diagnostic,
                    message,
                    f"{context}: bounded rejection omitted actionable size/type evidence; "
                    f"diagnostic={message!r}",
                )
                self.assertLess(
                    len(message),
                    1024,
                    f"{context}: diagnostic copied hostile input without a bound; "
                    f"diagnostic_length={len(message)}",
                )

        valid = self.model()
        for public_entry, arguments in (
            (model_module.validate_model, (valid,)),
            (model_module.render_workflow, (valid,)),
            (model_module.parse_model, (SOURCE_BYTES,)),
            (model_module.parse_and_render, (SOURCE_BYTES,)),
        ):
            with self.subTest(public_entry=public_entry.__name__):
                with self.assertRaisesRegex(
                    model_module.ContractError,
                    "source_name exceeds",
                    msg=f"{public_entry.__name__}: unbounded diagnostic label was accepted",
                ):
                    public_entry(*arguments, source_name="s" * 257)

    def test_timeout_bound_is_validated_with_coordinated_output(self):
        for timeout in (0, 6, True, "5"):
            with self.subTest(timeout=timeout):
                changed = self.model()
                changed["workflow"]["timeout_minutes"] = timeout
                if isinstance(timeout, bool):
                    scalar = str(timeout).lower()
                elif isinstance(timeout, str):
                    scalar = json.dumps(timeout)
                else:
                    scalar = str(timeout)
                output = GOLDEN_YAML.replace(b"timeout-minutes: 5", f"timeout-minutes: {scalar}".encode())
                self.assert_rejected(changed, "timeout_minutes", output, f"timeout {timeout!r}")

    def test_permissions_are_exact_with_coordinated_output(self):
        cases = {
            "broaden pull requests": (
                {"actions": "write", "pull-requests": "write"},
                GOLDEN_YAML.replace(b"pull-requests: read", b"pull-requests: write"),
            ),
            "narrow actions": (
                {"actions": "read", "pull-requests": "read"},
                GOLDEN_YAML.replace(b"actions: write", b"actions: read"),
            ),
            "extra contents authority": (
                {"actions": "write", "contents": "write", "pull-requests": "read"},
                GOLDEN_YAML.replace(
                    b"  actions: write\n", b"  actions: write\n  contents: write\n"
                ),
            ),
        }
        for context, (permissions, output) in cases.items():
            with self.subTest(context=context):
                changed = self.model()
                changed["workflow"]["permissions"] = permissions
                self.assert_rejected(changed, "workflow.permissions", output, context)

    def test_runner_is_exact_with_coordinated_output(self):
        changed = self.model()
        changed["workflow"]["runner"] = "ubuntu-latest"
        self.assert_rejected(
            changed,
            "workflow.runner",
            GOLDEN_YAML.replace(b'"ubuntu-24.04"', b'"ubuntu-latest"'),
            "floating runner",
        )

    def test_schema_version_names_job_and_execution_policy_are_exact(self):
        cases = (
            (
                "schema version",
                ("schema_version",),
                2,
                b"# schema-version: 2\n" + GOLDEN_YAML,
            ),
            (
                "workflow name",
                ("workflow", "name"),
                "Hidden authority workflow",
                GOLDEN_YAML.replace(
                    b'name: "Cancel superseded CI runs"', b'name: "Hidden authority workflow"', 1
                ),
            ),
            (
                "job id",
                ("workflow", "job_id"),
                "hidden-job",
                GOLDEN_YAML.replace(b'  "cancel-superseded":', b'  "hidden-job":'),
            ),
            (
                "job name",
                ("workflow", "job_name"),
                "Hidden job",
                GOLDEN_YAML.replace(
                    b'name: "Cancel superseded canonical CI runs"', b'name: "Hidden job"'
                ),
            ),
            (
                "script name",
                ("workflow", "script", "name"),
                "Hidden step",
                GOLDEN_YAML.replace(
                    b'name: "Run trusted bounded reconciliation controller"', b'name: "Hidden step"'
                ),
            ),
            (
                "execution policy",
                ("workflow", "execution_policy"),
                "requested-only-v1",
                GOLDEN_YAML.replace(
                    b'      - "in_progress"\n',
                    b"      # requested-only policy omitted in_progress\n",
                ),
            ),
        )
        for context, path, value, output in cases:
            with self.subTest(context=context):
                changed = self.model()
                target = changed
                for segment in path[:-1]:
                    target = target[segment]
                target[path[-1]] = value
                self.assert_rejected(changed, ".".join(path), output, context)

    def test_triggers_are_exact_with_coordinated_output(self):
        cases = {
            "missing requested": (
                ["in_progress"],
                GOLDEN_YAML.replace(b'      - "requested"\n', b""),
            ),
            "missing in progress": (
                ["requested"],
                GOLDEN_YAML.replace(b'      - "in_progress"\n', b""),
            ),
            "completed instead": (
                ["requested", "completed"],
                GOLDEN_YAML.replace(b'      - "in_progress"', b'      - "completed"'),
            ),
            "reordered": (
                ["in_progress", "requested"],
                GOLDEN_YAML.replace(
                    b'      - "requested"\n      - "in_progress"',
                    b'      - "in_progress"\n      - "requested"',
                ),
            ),
        }
        for context, (types, output) in cases.items():
            with self.subTest(context=context):
                changed = self.model()
                changed["workflow"]["workflow_run"]["types"] = types
                self.assert_rejected(changed, "workflow_run.types", output, context)

        changed = self.model()
        changed["workflow"]["workflow_run"]["workflows"] = ["Untrusted"]
        self.assert_rejected(
            changed,
            "workflow_run.workflows",
            GOLDEN_YAML.replace(b'      - "CI"', b'      - "Untrusted"'),
            "untrusted source workflow",
        )

    def test_dispatch_input_is_exact_with_coordinated_output(self):
        mutations = []
        renamed = self.model()
        renamed["workflow"]["dispatch_inputs"] = {
            "cursor": renamed["workflow"]["dispatch_inputs"]["continuation_cursor"]
        }
        mutations.append(
            (
                "renamed dispatch input",
                renamed,
                GOLDEN_YAML.replace(b'"continuation_cursor":', b'"cursor":'),
            )
        )
        for field, value in (
            ("required", True),
            ("type", "boolean"),
            ("default", "later"),
            ("description", "Unreviewed continuation authority"),
        ):
            changed = self.model()
            changed["workflow"]["dispatch_inputs"]["continuation_cursor"][field] = value
            old = {
                "required": b"required: false",
                "type": b"type: string",
                "default": b'default: ""',
                "description": b"description: \"Opaque bounded cursor for the next trusted reconciliation pass\"",
            }[field]
            new = {
                "required": b"required: true",
                "type": b"type: boolean",
                "default": b'default: "later"',
                "description": b'description: "Unreviewed continuation authority"',
            }[field]
            mutations.append((f"dispatch {field}", changed, GOLDEN_YAML.replace(old, new)))
        for context, changed, output in mutations:
            with self.subTest(context=context):
                self.assert_rejected(changed, "dispatch_inputs", output, context)

    def test_closed_schema_rejects_unknown_and_missing_fields_at_every_level(self):
        paths = (
            (),
            ("workflow",),
            ("workflow", "workflow_run"),
            ("workflow", "permissions"),
            ("workflow", "dispatch_inputs"),
            ("workflow", "dispatch_inputs", "continuation_cursor"),
            ("workflow", "environment"),
            ("workflow", "script"),
        )
        for path in paths:
            with self.subTest(path=path):
                changed = self.model()
                target = changed
                for segment in path:
                    target = target[segment]
                target["unexpected"] = "authority"
                output = GOLDEN_YAML + f"# ignored unknown at {'.'.join(path) or 'root'}\n".encode()
                self.assert_rejected(changed, "keys must be exactly", output, f"unknown {path}")

        changed = self.model()
        del changed["workflow"]["script"]
        self.assert_rejected(
            changed,
            "missing=['script']",
            GOLDEN_YAML.replace(b"    steps:\n", b""),
            "missing sole script",
        )

        for field, value in (("jobs", {"hidden": {}}), ("steps", [{"run": "echo hidden"}])):
            with self.subTest(forbidden_workflow_field=field):
                changed = self.model()
                changed["workflow"][field] = value
                self.assert_rejected(
                    changed,
                    f"unknown=['{field}']",
                    GOLDEN_YAML + f"# unreviewed {field}\n".encode(),
                    f"caller-supplied {field}",
                )

    def test_exact_stopped_validator_proves_old_attempt_filter_was_accepted(self):
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
        self.assertIn(
            "run_attempt > 1",
            stopped_workflow,
            "provenance workflow did not preserve the attempt-1 in_progress skip",
        )
        self.assertNotIn(
            b"run_attempt",
            GOLDEN_YAML,
            "new envelope retained the stopped attempt filter instead of running all in_progress deliveries",
        )

    def test_production_model_has_no_external_state_or_process_dependency(self):
        source = Path(model_module.__file__).read_text(encoding="utf-8")
        tree = ast.parse(source)
        imported = set()
        for node in ast.walk(tree):
            if isinstance(node, ast.Import):
                imported.update(alias.name.split(".", 1)[0] for alias in node.names)
            elif isinstance(node, ast.ImportFrom) and node.module:
                imported.add(node.module.split(".", 1)[0])
        forbidden = {
            "asyncio",
            "datetime",
            "git",
            "github",
            "http",
            "os",
            "pathlib",
            "requests",
            "socket",
            "subprocess",
            "tempfile",
            "time",
            "urllib",
        }
        self.assertFalse(
            imported & forbidden,
            "pure workflow model imported an external-state/process dependency; "
            f"forbidden_imports={sorted(imported & forbidden)!r}",
        )
        calls = {
            node.func.id
            for node in ast.walk(tree)
            if isinstance(node, ast.Call) and isinstance(node.func, ast.Name)
        }
        self.assertFalse(
            calls & {"open", "exec", "eval", "compile", "__import__"},
            "pure workflow model invoked a dynamic or filesystem builtin; "
            f"forbidden_calls={sorted(calls & {'open', 'exec', 'eval', 'compile', '__import__'})!r}",
        )


if __name__ == "__main__":
    unittest.main()
