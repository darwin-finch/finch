#!/usr/bin/env python3
import json
import os
import signal
import subprocess
import sys
import time

scenario = sys.argv[1]
extra = sys.argv[2:-3]
ending = "\r\n" if scenario == "crlf" else "\n"

if sys.argv[-3:] != ["app-server", "--listen", "stdio://"]:
    raise SystemExit("unexpected app-server invocation")
if "PATH" in os.environ or "OPENAI_API_KEY" in os.environ:
    raise SystemExit("unexpected inherited process environment")


def send(value):
    sys.stdout.write(json.dumps(value, separators=(",", ":")) + ending)
    sys.stdout.flush()


def response(message, result):
    send({"id": message["id"], "result": result})


def notification(method, params):
    send({"method": method, "params": params})


if scenario in ("child_tree", "child_exits_first"):
    child = subprocess.Popen(["/bin/sh", "-c", "while :; do sleep 60; done"])
    with open(extra[0], "w", encoding="utf-8") as handle:
        handle.write(str(child.pid))

for raw in sys.stdin:
    message = json.loads(raw)
    method = message.get("method")
    if method == "initialize":
        if "capabilities" in message.get("params", {}):
            send({"id": message["id"], "error": {"code": -32602, "message": "experimental capability opt-in rejected"}})
            continue
        if scenario == "hang_initialize":
            time.sleep(60)
        if scenario == "malformed":
            sys.stdout.write("{not-json\n")
            sys.stdout.flush()
            continue
        if scenario == "oversized":
            sys.stdout.write("x" * 1025 + "\n")
            sys.stdout.flush()
            continue
        if scenario == "truncated":
            sys.stdout.write('{"id":1,"result":{}')
            sys.stdout.flush()
            sys.exit(0)
        if scenario == "invalid_utf8":
            sys.stdout.buffer.write(b'\xff\n')
            sys.stdout.buffer.flush()
            continue
        if scenario == "noisy_stderr":
            sys.stderr.write("userCode=DO-NOT-LEAK Bearer eyJ Cookie password secret sk- https://example.test/?token=")
            sys.stderr.flush()
            sys.stderr.write("split-across-write ordinary diagnostic\n" + "z" * 4096)
            sys.stderr.flush()
        response(message, {"userAgent": "fake", "platformFamily": "test", "platformOs": "test"})
        continue
    if method == "initialized":
        if scenario == "unknown_request":
            send({"id": "server-1", "method": "attestation/generate", "params": {"secret": "not-logged"}})
        if scenario == "unexpected_notification":
            notification("plugin/started", {"plugin": "ambient"})
        if scenario == "child_exits_first":
            sys.exit(0)
        continue
    if "id" in message and "error" in message:
        continue
    if method == "account/read":
        if message.get("params") not in ({"refreshToken": False}, {"refreshToken": True}):
            raise SystemExit("unexpected account/read params")
        if scenario in ("crash_account", "noisy_stderr"):
            os.kill(os.getpid(), signal.SIGKILL)
        if scenario == "aggregate_exhaustion":
            for index in range(100):
                notification("account/updated", {"authMode": "chatgpt", "index": index})
        response(message, {"account": {"type": "chatgpt", "planType": "plus"}, "requiresOpenaiAuth": True})
    elif method == "account/login/start":
        if message.get("params") != {"type": "chatgptDeviceCode"}:
            raise SystemExit("unexpected account/login/start params")
        response(message, {
            "type": "chatgptDeviceCode",
            "loginId": "login-1",
            "verificationUrl": "https://auth.openai.example/device",
            "userCode": "SECRET-CODE",
        })
        if scenario == "login_success":
            notification("account/login/completed", {"loginId": "login-1", "success": True, "error": None})
        elif scenario == "login_duplicate_late":
            notification("account/login/completed", {"loginId": "login-1", "success": True, "error": None})
            notification("account/login/completed", {"loginId": "login-1", "success": True, "error": None})
        elif scenario == "login_denied":
            notification("account/login/completed", {"loginId": "login-1", "success": False, "error": "expired secret details"})
        elif scenario == "login_wrong_then_success":
            notification("account/login/completed", {"loginId": "other", "success": True, "error": None})
            notification("account/login/completed", {"loginId": "login-1", "success": True, "error": None})
    elif method == "account/login/cancel":
        if message.get("params") != {"loginId": "login-1"}:
            raise SystemExit("unexpected account/login/cancel params")
        response(message, {})
        notification("account/login/completed", {"loginId": "login-1", "success": False, "error": "cancelled"})
    elif method == "account/logout":
        if message.get("params") != {}:
            raise SystemExit("unexpected account/logout params")
        response(message, {})
    elif method == "model/list":
        params = message.get("params", {})
        if params.get("limit") != 100 or params.get("includeHidden") is not False:
            raise SystemExit("unexpected model/list params")
        cursor = message.get("params", {}).get("cursor")
        if scenario == "sol_absent":
            response(message, {"data": [{
                "id": "gpt-5.6-terra", "model": "gpt-5.6-terra",
                "displayName": "GPT-5.6 Terra", "hidden": False
            }], "nextCursor": None})
        elif scenario == "hidden_sol":
            response(message, {"data": [{
                "id": "gpt-5.6-sol", "model": "gpt-5.6-sol",
                "displayName": "GPT-5.6 Sol", "hidden": True
            }], "nextCursor": None})
        elif scenario == "cursor_cycle":
            next_cursor = "page-b" if cursor is None else ("page-a" if cursor == "page-b" else "page-b")
            response(message, {"data": [], "nextCursor": next_cursor})
        elif cursor is None:
            response(message, {"data": [{
                "id": "gpt-5.6-terra", "model": "gpt-5.6-terra",
                "displayName": "GPT-5.6 Terra", "hidden": False
            }], "nextCursor": "page-2"})
        else:
            response(message, {"data": [{
                "id": "gpt-5.6-sol", "model": "gpt-5.6-sol",
                "displayName": "GPT-5.6 Sol", "hidden": False,
                "defaultReasoningEffort": "low",
                "supportedReasoningEfforts": [{"reasoningEffort": "low", "description": "fast"}],
                "inputModalities": ["text", "image"]
            }], "nextCursor": None})
    else:
        send({"id": message.get("id"), "error": {"code": -32601, "message": "unsupported"}})
