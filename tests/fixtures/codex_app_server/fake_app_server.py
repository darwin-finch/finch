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


if scenario == "child_tree":
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
        if scenario == "noisy_stderr":
            sys.stderr.write("userCode=DO-NOT-LEAK\nordinary diagnostic\n" + "z" * 4096)
            sys.stderr.flush()
        response(message, {"userAgent": "fake", "platformFamily": "test", "platformOs": "test"})
        continue
    if method == "initialized":
        if scenario == "unknown_request":
            send({"id": "server-1", "method": "attestation/generate", "params": {"secret": "not-logged"}})
        continue
    if "id" in message and "error" in message:
        continue
    if method == "account/read":
        if scenario in ("crash_account", "noisy_stderr"):
            os.kill(os.getpid(), signal.SIGKILL)
        response(message, {"account": {"type": "chatgpt", "planType": "plus"}, "requiresOpenaiAuth": True})
    elif method == "account/login/start":
        response(message, {
            "type": "chatgptDeviceCode",
            "loginId": "login-1",
            "verificationUrl": "https://auth.openai.example/device",
            "userCode": "SECRET-CODE",
        })
        if scenario == "login_success":
            notification("account/login/completed", {"loginId": "login-1", "success": True, "error": None})
        elif scenario == "login_wrong_then_success":
            notification("account/login/completed", {"loginId": "other", "success": True, "error": None})
            notification("account/login/completed", {"loginId": "login-1", "success": True, "error": None})
    elif method == "account/login/cancel":
        response(message, {})
        notification("account/login/completed", {"loginId": "login-1", "success": False, "error": "cancelled"})
    elif method == "account/logout":
        response(message, {})
    elif method == "model/list":
        cursor = message.get("params", {}).get("cursor")
        if scenario == "sol_absent":
            response(message, {"data": [{
                "id": "gpt-5.6-terra", "model": "gpt-5.6-terra",
                "displayName": "GPT-5.6 Terra", "hidden": False
            }], "nextCursor": None})
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
    elif method == "thread/start":
        response(message, {"thread": {"id": "thread-1"}})
    elif method == "turn/start":
        response(message, {"turn": {"id": "turn-1", "status": "inProgress"}})
        if scenario == "text_turn":
            notification("item/started", {"threadId": "thread-1", "turnId": "turn-1", "item": {"id": "item-1", "type": "agentMessage"}})
            notification("item/agentMessage/delta", {"threadId": "thread-1", "turnId": "turn-1", "itemId": "item-1", "delta": "draft"})
            notification("item/completed", {"threadId": "thread-1", "turnId": "turn-1", "item": {"id": "item-1", "type": "agentMessage", "text": "authoritative"}})
            notification("turn/completed", {"threadId": "thread-1", "turn": {"id": "turn-1", "status": "completed"}})
    elif method == "turn/interrupt":
        response(message, {})
        notification("turn/completed", {"threadId": "thread-1", "turn": {"id": "turn-1", "status": "interrupted"}})
    else:
        send({"id": message.get("id"), "error": {"code": -32601, "message": "unsupported"}})
