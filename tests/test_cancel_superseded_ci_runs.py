#!/usr/bin/env python3
"""Exercise the exact trusted CI cancellation controller against a fake GitHub API."""

from __future__ import annotations

import copy
import json
import os
import re
import subprocess
import tempfile
import threading
import unittest
import urllib.parse
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path


ROOT = Path(__file__).resolve().parent.parent
WORKFLOW = ROOT / ".github/workflows/ci-superseded-run-cancellation.yml"
REPO = "darwin-finch/finch"
A = "a" * 40
B = "b" * 40
C = "c" * 40


def extract_controller(workflow: str | None = None) -> str:
    text = WORKFLOW.read_text() if workflow is None else workflow
    match = re.search(
        r"^          python3 - <<'PYTHON'\n(?P<script>.*?)^          PYTHON$",
        text,
        re.MULTILINE | re.DOTALL,
    )
    if match is None:
        raise AssertionError("workflow must contain one literal PYTHON heredoc controller")
    lines = match.group("script").splitlines()
    if any(line and not line.startswith("          ") for line in lines):
        raise AssertionError("controller heredoc indentation drifted from the executable script")
    return "\n".join(line[10:] for line in lines) + "\n"


def validate_workflow_contract(text: str) -> None:
    lines = {line.strip() for line in text.splitlines()}
    required_lines = {
        "workflows: [CI]",
        "types: [requested, in_progress]",
        "actions: write",
        "pull-requests: read",
        "runs-on: ubuntu-24.04",
        "timeout-minutes: 5",
        "if: github.event.action == 'requested' || github.event.workflow_run.run_attempt > 1",
        "MAX_PAGES = 4",
        '"branch": branch,',
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
    if missing or present or text.count("runs-on:") != 1:
        raise AssertionError(
            "trusted cancellation workflow contract drifted: "
            f"missing={missing!r} forbidden={present!r} jobs={text.count('runs-on:')}"
        )
    compile(extract_controller(text), str(WORKFLOW), "exec")


def pr(number: int, sha: str, branch: str = "feature", *, state: str = "open") -> dict:
    return {
        "number": number,
        "state": state,
        "head": {"sha": sha, "ref": branch, "repo": {"full_name": "fork/repo"}},
        "base": {"ref": "main", "repo": {"full_name": REPO}},
    }


def run(
    run_id: int,
    sha: str,
    run_number: int,
    *,
    number: int = 7,
    status: str = "queued",
    attempt: int = 1,
    branch: str = "feature",
    event: str = "pull_request",
) -> dict:
    value = {
        "id": run_id,
        "name": "CI",
        "path": ".github/workflows/ci.yml",
        "event": event,
        "repository": {"full_name": REPO},
        "head_sha": sha,
        "head_branch": branch,
        "run_number": run_number,
        "run_attempt": attempt,
        "status": status,
        "pull_requests": [
            {
                "number": number,
                "head": {"sha": sha, "ref": branch, "repo": {"full_name": "fork/repo"}},
                "base": {"ref": "main", "repo": {"full_name": REPO}},
            }
        ],
    }
    return value


class FakeGitHub:
    def __init__(self, trigger: dict, prs: dict[int, dict], listed: list[dict] | None = None):
        self.runs = {trigger["id"]: copy.deepcopy(trigger)}
        self.runs.update({item["id"]: copy.deepcopy(item) for item in listed or []})
        self.prs = copy.deepcopy(prs)
        self.listed = [copy.deepcopy(item) for item in listed or []]
        self.requests: list[tuple[str, str]] = []
        self.cancel_attempts: list[int] = []
        self.errors: dict[str, tuple[int, bytes, dict[str, str]]] = {}
        self.full_pages = False
        self.advance_on_pr_get: tuple[int, str] | None = None
        self.pr_gets = 0
        self.cancel_codes: dict[int, int] = {}
        state = self

        class Handler(BaseHTTPRequestHandler):
            def log_message(self, *_args):
                return

            def reply(self, code: int, value=b"", headers: dict[str, str] | None = None):
                raw = json.dumps(value).encode() if not isinstance(value, bytes) else value
                self.send_response(code)
                self.send_header("Content-Type", "application/json")
                self.send_header("Content-Length", str(len(raw)))
                for key, item in (headers or {}).items():
                    self.send_header(key, item)
                self.end_headers()
                self.wfile.write(raw)

            def route_error(self) -> bool:
                for needle, response in state.errors.items():
                    if needle in self.path:
                        self.reply(*response)
                        return True
                return False

            def do_GET(self):
                state.requests.append(("GET", self.path))
                if self.route_error():
                    return
                parsed = urllib.parse.urlparse(self.path)
                run_match = re.fullmatch(rf"/repos/{REPO}/actions/runs/(\d+)", parsed.path)
                pr_match = re.fullmatch(rf"/repos/{REPO}/pulls/(\d+)", parsed.path)
                if run_match:
                    value = state.runs.get(int(run_match.group(1)))
                    self.reply(200, value if value is not None else {})
                    return
                if pr_match:
                    number = int(pr_match.group(1))
                    state.pr_gets += 1
                    if state.advance_on_pr_get and state.pr_gets == state.advance_on_pr_get[0]:
                        state.prs[number]["head"]["sha"] = state.advance_on_pr_get[1]
                    self.reply(200, state.prs.get(number, {}))
                    return
                if parsed.path == f"/repos/{REPO}/actions/workflows/ci.yml/runs":
                    query = urllib.parse.parse_qs(parsed.query)
                    page = int(query.get("page", ["1"])[0])
                    branch = query.get("branch", [""])[0]
                    if state.full_pages:
                        start = (page - 1) * 100
                        values = [
                            run(10_000 + index, A, index + 1, branch=branch, status="completed")
                            for index in range(start, start + 100)
                        ]
                    else:
                        values = [
                            item
                            for item in state.listed
                            if item["head_branch"] == branch
                            and item["event"] == query.get("event", [""])[0]
                        ]
                        values = values[(page - 1) * 100 : page * 100]
                    self.reply(200, {"workflow_runs": values})
                    return
                self.reply(404, {"message": "not found"})

            def do_POST(self):
                state.requests.append(("POST", self.path))
                if self.route_error():
                    return
                match = re.fullmatch(rf"/repos/{REPO}/actions/runs/(\d+)/cancel", self.path)
                if match is None:
                    self.reply(404, {"message": "not found"})
                    return
                run_id = int(match.group(1))
                state.cancel_attempts.append(run_id)
                self.reply(state.cancel_codes.get(run_id, 202), {})

        self.server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        self.thread = threading.Thread(target=self.server.serve_forever, daemon=True)

    def __enter__(self):
        self.thread.start()
        return self

    def __exit__(self, *_args):
        self.server.shutdown()
        self.server.server_close()
        self.thread.join()

    @property
    def url(self) -> str:
        return f"http://127.0.0.1:{self.server.server_port}"


def event(trigger: dict, action: str = "requested") -> dict:
    return {"action": action, "repository": {"full_name": REPO}, "workflow_run": trigger}


def execute(fake: FakeGitHub, payload: dict) -> subprocess.CompletedProcess[str]:
    with tempfile.TemporaryDirectory() as directory:
        event_path = Path(directory) / "event.json"
        event_path.write_text(json.dumps(payload))
        environment = {
            "PATH": os.environ["PATH"],
            "GITHUB_EVENT_PATH": str(event_path),
            "GITHUB_API_URL": fake.url,
            "TOKEN": "test-token",
        }
        return subprocess.run(
            ["python3", "-c", extract_controller()],
            env=environment,
            text=True,
            capture_output=True,
            timeout=10,
            check=False,
        )


class ControllerTests(unittest.TestCase):
    def assert_result(self, result, fake, expected, context):
        self.assertEqual(
            result.returncode,
            0,
            f"{context}: controller failed; stdout={result.stdout!r} stderr={result.stderr!r} "
            f"requests={fake.requests!r} cancellations={fake.cancel_attempts!r}",
        )
        self.assertEqual(
            fake.cancel_attempts,
            expected,
            f"{context}: unsafe cancellation plan; stdout={result.stdout!r} stderr={result.stderr!r} "
            f"requests={fake.requests!r} cancellations={fake.cancel_attempts!r}",
        )

    def test_current_b_cancels_only_superseded_a_in_both_arrival_orders(self):
        trigger = run(200, B, 20, status="queued")
        old_queued = run(100, A, 10, status="queued")
        old_rerun = run(101, A, 11, status="in_progress", attempt=2)
        future = run(300, C, 30, status="queued")
        same_sha = run(201, B, 19, status="in_progress")
        other_pr = run(99, A, 9, number=8, status="in_progress")
        push = run(98, A, 8, event="push")
        with FakeGitHub(
            trigger,
            {7: pr(7, B), 8: pr(8, A)},
            [old_queued, old_rerun, future, same_sha, other_pr, push],
        ) as fake:
            result = execute(fake, event(trigger))
            self.assert_result(result, fake, [100, 101], "new B after queued/running A")
            self.assertNotIn(push["id"], fake.cancel_attempts, "push work must remain isolated")

    def test_current_a_and_initial_in_progress_do_not_cancel(self):
        trigger = run(100, A, 10, status="in_progress")
        with FakeGitHub(trigger, {7: pr(7, A)}) as fake:
            result = execute(fake, event(trigger, "in_progress"))
            self.assert_result(result, fake, [], "initial in_progress duplicate controller")
            self.assertEqual(fake.requests, [], "initial in_progress must skip all API allocation")

    def test_stale_a_rerun_after_b_cancels_self_only(self):
        trigger = run(100, A, 10, status="in_progress", attempt=2)
        current = run(200, B, 20, status="in_progress")
        with FakeGitHub(trigger, {7: pr(7, B)}, [current]) as fake:
            result = execute(fake, event(trigger, "in_progress"))
            self.assert_result(result, fake, [100], "stale A rerun after B")

    def test_duplicate_delivery_and_terminal_409_are_idempotent(self):
        trigger = run(200, B, 20)
        old = run(100, A, 10)
        with FakeGitHub(trigger, {7: pr(7, B)}, [old]) as fake:
            fake.cancel_codes[100] = 409
            first = execute(fake, event(trigger))
            second = execute(fake, event(trigger))
            self.assertEqual(first.returncode, 0, f"first terminal 409 failed: {first.stderr}")
            self.assertEqual(second.returncode, 0, f"requested redelivery failed: {second.stderr}")
            self.assertEqual(
                fake.cancel_attempts,
                [100, 100],
                f"duplicate requested delivery was not idempotent: requests={fake.requests!r}",
            )

    def test_pr_head_advance_before_mutation_cancels_b_and_preserves_c(self):
        trigger = run(200, B, 20)
        old = run(100, A, 10)
        future = run(300, C, 30)
        with FakeGitHub(trigger, {7: pr(7, B)}, [old, future]) as fake:
            fake.advance_on_pr_get = (2, C)
            result = execute(fake, event(trigger))
            self.assert_result(result, fake, [200], "PR advanced from B to C during planning")

    def test_candidate_revalidation_preserves_head_rolled_back_to_a(self):
        trigger = run(200, B, 20)
        old = run(100, A, 10)
        with FakeGitHub(trigger, {7: pr(7, B)}, [old]) as fake:
            fake.advance_on_pr_get = (3, A)
            result = execute(fake, event(trigger))
            self.assert_result(
                result,
                fake,
                [],
                "candidate that became the live PR head immediately before cancellation",
            )

    def test_bounded_pagination_finishes_before_first_cancellation(self):
        trigger = run(500, B, 500)
        completed = [
            run(10_000 + index, A, 400 - index, status="completed")
            for index in range(100)
        ]
        old = run(100, A, 10)
        with FakeGitHub(trigger, {7: pr(7, B)}, completed + [old]) as fake:
            result = execute(fake, event(trigger))
            self.assert_result(result, fake, [100], "two-page branch inventory")
            first_post = next(
                index for index, request in enumerate(fake.requests) if request[0] == "POST"
            )
            listed_pages = [
                request
                for request in fake.requests[:first_post]
                if "/actions/workflows/ci.yml/runs?" in request[1]
            ]
            self.assertEqual(
                len(listed_pages),
                2,
                "controller must finish both bounded listing pages before any cancellation: "
                f"requests={fake.requests!r}",
            )

    def test_ambiguous_closed_and_malformed_identity_fail_closed(self):
        cases = []
        zero = run(100, A, 10)
        zero["pull_requests"] = []
        cases.append(("zero associations", zero, {7: pr(7, A)}))
        multiple = run(100, A, 10)
        multiple["pull_requests"].append(copy.deepcopy(multiple["pull_requests"][0]))
        cases.append(("multiple associations", multiple, {7: pr(7, A)}))
        closed = run(100, A, 10)
        cases.append(("closed PR", closed, {7: pr(7, A, state="closed")}))
        wrong_repo = run(100, A, 10)
        wrong_repo["path"] = ".github/workflows/other.yml"
        cases.append(("noncanonical workflow", wrong_repo, {7: pr(7, A)}))
        for label, trigger, prs in cases:
            with self.subTest(label=label), FakeGitHub(trigger, prs) as fake:
                result = execute(fake, event(trigger))
                self.assertNotEqual(result.returncode, 0, f"{label} must fail closed: {result.stdout!r}")
                self.assertEqual(fake.cancel_attempts, [], f"{label} cancelled runs: {fake.cancel_attempts!r}")

    def test_api_errors_malformed_json_and_pagination_cap_fail_closed(self):
        trigger = run(200, B, 20)
        for label, error in (
            ("forbidden", (403, b"{}", {})),
            ("rate limited", (429, b"{}", {"Retry-After": "60"})),
            ("server error", (500, b"{}", {})),
            ("malformed", (200, b"not-json", {})),
        ):
            with self.subTest(label=label), FakeGitHub(trigger, {7: pr(7, B)}) as fake:
                fake.errors["workflows/ci.yml/runs"] = error
                result = execute(fake, event(trigger))
                self.assertNotEqual(result.returncode, 0, f"{label} response was accepted: {result.stdout!r}")
                self.assertEqual(fake.cancel_attempts, [], f"{label} caused cancellation: {fake.cancel_attempts!r}")
                if error[0] in (403, 429, 500):
                    self.assertIn("rate_limit_remaining", result.stderr, result.stderr)
        with FakeGitHub(trigger, {7: pr(7, B)}) as fake:
            fake.full_pages = True
            result = execute(fake, event(trigger))
            self.assertNotEqual(result.returncode, 0, f"pagination cap was accepted: {result.stdout!r}")
            self.assertEqual(fake.cancel_attempts, [], f"pagination cap cancelled: {fake.cancel_attempts!r}")

    def test_malicious_event_text_is_data_not_shell(self):
        trigger = run(100, A, 10)
        with tempfile.TemporaryDirectory() as directory:
            marker = Path(directory) / "executed"
            malicious = copy.deepcopy(trigger)
            malicious["name"] = f"CI$(touch {marker})"
            with FakeGitHub(trigger, {7: pr(7, A)}) as fake:
                result = execute(fake, event(malicious))
            self.assertNotEqual(result.returncode, 0, "malicious noncanonical workflow must fail closed")
            self.assertFalse(marker.exists(), f"payload text executed shell command and created {marker}")


class StaticContractTests(unittest.TestCase):
    def test_workflow_has_narrow_trusted_contract(self):
        text = WORKFLOW.read_text()
        validate_workflow_contract(text)

    def test_known_wrong_expression_shapes_are_rejected(self):
        text = WORKFLOW.read_text()
        mutations = {
            "requested only": text.replace("types: [requested, in_progress]", "types: [requested]"),
            "PR-only concurrency": text + "\nconcurrency: pr-${{ github.event.workflow_run.pull_requests[0].number }}\n",
            "unique rerun concurrency": text + "\nconcurrency: run-${{ github.event.workflow_run.id }}\n",
            "checkout": text.replace("steps:\n", "steps:\n      - uses: actions/checkout@v4\n", 1),
            "broader permission": text.replace("pull-requests: read", "pull-requests: write"),
            "unbounded listing": text.replace("MAX_PAGES = 4", "MAX_PAGES = 4000"),
        }
        for label, mutation in mutations.items():
            with self.subTest(label=label):
                with self.assertRaises(AssertionError, msg=f"static contract accepted {label}"):
                    self._validate(mutation)

    def _validate(self, text: str):
        validate_workflow_contract(text)


if __name__ == "__main__":
    unittest.main()
