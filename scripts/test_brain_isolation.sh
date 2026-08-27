#!/usr/bin/env bash
set -Eeuo pipefail

phase=setup
trap 'status=$?; echo "Brain isolation regression failed in phase: $phase (line $LINENO, status $status)" >&2; exit "$status"' ERR

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
source "$repo_root/scripts/lib/brain_test_isolation.sh"
supervisor="${FINCH_TEST_SUPERVISOR_BIN:-$repo_root/target/debug/finch-test-supervisor}"
[[ -x "$supervisor" ]] || { echo 'build finch-test-supervisor before running isolation regressions' >&2; exit 69; }

scratch="$(mktemp -d "$(cd "${TMPDIR:-/tmp}" && pwd -P)/finch-brain-isolation-regression.XXXXXX")"
sentinel_pid=''
signaler_pid=''
cleanup_regression() {
  if [[ -n "$signaler_pid" ]]; then wait "$signaler_pid" 2>/dev/null || true; fi
  if [[ -n "$sentinel_pid" ]]; then printf '\n' >&7 2>/dev/null || true; wait "$sentinel_pid" 2>/dev/null || true; fi
  exec 7>&- 2>/dev/null || true
  rm -rf -- "$scratch"
}
trap cleanup_regression EXIT

fake_home="$scratch/real-home"
temp_parent="$scratch/temp-homes"
mkdir -p "$fake_home/.finch/brains/existing" "$temp_parent"
printf 'keep me\n' >"$fake_home/.finch/brains/existing/events.jsonl"

run_isolated() {
  FINCH_TEST_REAL_HOME="$fake_home" FINCH_TEST_TMP_PARENT="$temp_parent" "$supervisor" "$@"
}

expect_supervisor_70() {
  if "$@" >/dev/null 2>&1; then
    echo 'expected supervisor validation failure' >&2
    exit 1
  else
    test "$?" -eq 70
  fi
}

# Canonical path validation fails before a child is launched or a disposable
# HOME is allocated.
phase=canonical-path-rejection
phase=canonical-missing-home
expect_supervisor_70 env -u HOME -u FINCH_TEST_REAL_HOME \
  FINCH_TEST_TMP_PARENT="$temp_parent" "$supervisor" true
phase=canonical-trailing-separator
expect_supervisor_70 env FINCH_TEST_REAL_HOME="$fake_home/" \
  FINCH_TEST_TMP_PARENT="$temp_parent" "$supervisor" true
ln -s "$fake_home" "$scratch/home-link"
phase=canonical-symlink-home
expect_supervisor_70 env FINCH_TEST_REAL_HOME="$scratch/home-link" \
  FINCH_TEST_TMP_PARENT="$temp_parent" "$supervisor" true
phase=canonical-relative-temp-parent
expect_supervisor_70 env FINCH_TEST_REAL_HOME="$fake_home" \
  FINCH_TEST_TMP_PARENT=relative "$supervisor" true
ln -s "$temp_parent" "$scratch/temp-link"
phase=canonical-symlink-temp-parent
expect_supervisor_70 env FINCH_TEST_REAL_HOME="$fake_home" \
  FINCH_TEST_TMP_PARENT="$scratch/temp-link" "$supervisor" true
phase=canonical-overlapping-scratch
expect_supervisor_70 env FINCH_TEST_REAL_HOME="$fake_home" \
  FINCH_TEST_TMP_PARENT="$scratch" "$supervisor" true
phase=canonical-overlapping-home
expect_supervisor_70 env FINCH_TEST_REAL_HOME="$fake_home" \
  FINCH_TEST_TMP_PARENT="$fake_home" "$supervisor" true
phase=canonical-overlapping-store
expect_supervisor_70 env FINCH_TEST_REAL_HOME="$fake_home" \
  FINCH_TEST_TMP_PARENT="$fake_home/.finch/brains" "$supervisor" true
phase=canonical-rejection-clean
test -z "$(find "$temp_parent" -mindepth 1 -print -quit)"

# An absent parent uses the canonical platform temporary directory. Caller
# input remains strict: the explicit symlink case above must still fail.
phase=supervisor-canonicalizes-default-temp-parent
env -u FINCH_TEST_TMP_PARENT FINCH_TEST_REAL_HOME="$fake_home" TMPDIR="$temp_parent/" \
  "$supervisor" true
test -z "$(find "$temp_parent" -mindepth 1 -print -quit)"

# The public launcher applies the same canonical-default contract before it
# delegates to the supervisor.
phase=launcher-canonicalizes-default-temp-parent
env -u FINCH_TEST_TMP_PARENT FINCH_TEST_REAL_HOME="$fake_home" TMPDIR="$temp_parent/" \
  "$repo_root/scripts/test_brains.sh" true
test -z "$(find "$temp_parent" -mindepth 1 -print -quit)"

hostile_home="$scratch/hostile-home"
hostile_target="$scratch/effective-production-finch"
mkdir -p "$hostile_home" "$hostile_target/brains"
printf 'production sentinel\n' >"$hostile_target/brains/sentinel"
ln -s "$hostile_target" "$hostile_home/.finch"
phase=canonical-symlinked-real-store
expect_supervisor_70 env FINCH_TEST_REAL_HOME="$hostile_home" \
  FINCH_TEST_TMP_PARENT="$temp_parent" "$supervisor" true
test "$(cat "$hostile_target/brains/sentinel")" = 'production sentinel'
test -z "$(find "$temp_parent" -mindepth 1 -print -quit)"

created="$scratch/created-home"
phase=sealed-proof-and-endpoints
FINCH_TEST_BRAIN_ADDR=127.0.0.1:11436 FINCH_TEST_DAEMON_ADDR=127.0.0.1:11435 \
FINCH_TEST_BRAIN_PASSWORD=ambient-password FINCH_CREATED_HOME="$created" \
FINCH_PROOF_HELPER="$repo_root/scripts/lib/brain_test_isolation.sh" run_isolated bash -c '
  source "$FINCH_PROOF_HELPER"
  brain_test_isolation_is_active
  test "$FINCH_BRAIN_TEST_ROOT" = "$HOME/.finch/brains"
  test "$FINCH_BRAIN_TEST_NO_AUTO_SPAWN" = 1
  test "$FINCH_TEST_BRAIN_ADDR" != 127.0.0.1:11436
  test "$FINCH_TEST_DAEMON_ADDR" != 127.0.0.1:11435
  test "$FINCH_TEST_BRAIN_PASSWORD" != ambient-password
  [[ "$FINCH_TEST_BRAIN_PASSWORD" =~ ^test-[0-9a-f]{32}$ ]]
  ! printf attacker >&9
  ! sh -c ": >/dev/fd/9"
  printf "%s\n" "$HOME" >"$FINCH_CREATED_HOME"
  printf test >"$FINCH_BRAIN_TEST_ROOT/test-created"
'
isolated_home="$(cat "$created")"
test ! -e "$isolated_home"
test -z "$(find "$temp_parent" -mindepth 1 -print -quit)"
test "$(cat "$fake_home/.finch/brains/existing/events.jsonl")" = 'keep me'

# Environment strings plus a caller-created descriptor are not wrapper
# authority. This linked, caller-owned proof must fail before any launcher can
# treat the process as isolated.
forged_proof="$scratch/forged-proof"
phase=forged-shell-proof-rejection
printf '%s\n' forged "$fake_home" "$fake_home/.finch/brains" 0:0 0:0 \
  127.0.0.1:1 127.0.0.1:2 forged "$fake_home/.finch/daemon.sock" \
  "$fake_home/.finch" 0:0 "$$" /bin/sh 0:0 >"$forged_proof"
(
  exec 9<"$forged_proof"
  FINCH_BRAIN_TEST_ISOLATED=1 FINCH_BRAIN_TEST_PROOF_FD=9 \
    FINCH_BRAIN_TEST_TOKEN=forged FINCH_TEST_SUPERVISOR_PID="$$" \
    FINCH_PROOF_HELPER="$repo_root/scripts/lib/brain_test_isolation.sh" \
    bash -c 'source "$FINCH_PROOF_HELPER"; ! brain_test_isolation_is_active'
)

phase=ordinary-nonzero-cleanup
if run_isolated bash -c 'exit 23'; then exit 1; else test "$?" -eq 23; fi
test -z "$(find "$temp_parent" -mindepth 1 -print -quit)"

# A caller-controlled PATH cannot hide a live group member from the
# supervisor. The external observer proves HOME exists for the descendant's
# entire lifetime; cleanup happens only after the trusted /bin/ps inspection
# sees the group quiesce.
inspection_bin="$scratch/inspection-bin"
inspection_called="$scratch/shadow-ps-called"
inspection_pid="$scratch/inspection-descendant.pid"
inspection_home="$scratch/inspection-home"
inspection_observer="$scratch/inspection-observer"
phase=successful-empty-shadow-ps-cannot-hide-descendant
mkdir "$inspection_bin"
printf '%s\n' '#!/bin/sh' ': >"$FINCH_SHADOW_PS_CALLED"' 'exit 0' >"$inspection_bin/ps"
chmod +x "$inspection_bin/ps"
(
  while [[ ! -s "$inspection_pid" || ! -s "$inspection_home" ]]; do sleep 0.01; done
  observed_pid="$(cat "$inspection_pid")"
  observed_home="$(cat "$inspection_home")"
  while /bin/ps -p "$observed_pid" -o pid= 2>/dev/null | grep -q '[0-9]'; do
    test -d "$observed_home" || exit 1
    sleep 0.01
  done
  printf survived >"$inspection_observer"
) &
inspection_observer_pid=$!
if FINCH_DESCENDANT_PID_FILE="$inspection_pid" FINCH_OBSERVED_HOME="$inspection_home" \
  FINCH_SHADOW_PS_CALLED="$inspection_called" PATH="$inspection_bin:$PATH" \
  run_isolated bash -c '
    printf "%s\n" "$HOME" >"$FINCH_OBSERVED_HOME"
    (trap "" TERM HUP INT; printf "%s\n" "$BASHPID" >"$FINCH_DESCENDANT_PID_FILE"; sleep 30) &
    exit 31
  '; then
  exit 1
else
  test "$?" -eq 31
fi
wait "$inspection_observer_pid"
test "$(cat "$inspection_observer")" = survived
test ! -e "$inspection_called"
test -z "$(find "$temp_parent" -mindepth 1 -print -quit)"

# An actual inspection error still fails closed and preserves both disposable
# roots for diagnosis. This is distinct from PATH shadowing, which cannot
# influence the trusted platform inspection at all.
phase=inspection-failure-preserves-home
if FINCH_TEST_FORCE_GROUP_INSPECTION_FAILURE=1 \
  run_isolated true 2>"$scratch/inspection.err"; then
  exit 1
else
  test "$?" -eq 70
fi
preserved_home="$(find "$temp_parent" -type d -name 'finch-brain-test-home.*' -print -quit)"
test -n "$preserved_home" && test -d "$preserved_home"
rg -q 'process group was not quiescent' "$scratch/inspection.err"
preserved_socket_root="$(sed -n 's/.*socket root at \([^ ]*\) because.*/\1/p' "$scratch/inspection.err")"
[[ "$preserved_socket_root" == /private/tmp/ft.* || "$preserved_socket_root" == /tmp/ft.* ]]
test -d "$preserved_socket_root"
rm -rf -- "$preserved_home"
rm -rf -- "$preserved_socket_root"
test -z "$(find "$temp_parent" -mindepth 1 -print -quit)"

# A normally exiting leader cannot strand a TERM-ignoring member of its owned
# process group. The leader remains unreaped until that group is quiescent.
normal_descendant_pid_file="$scratch/normal-descendant.pid"
phase=normal-exit-term-resistant-descendant
if FINCH_DESCENDANT_PID_FILE="$normal_descendant_pid_file" run_isolated bash -c '
  (trap "" TERM HUP INT; echo "$BASHPID" >"$FINCH_DESCENDANT_PID_FILE"; sleep 30) &
  exit 29
'; then
  exit 1
else
  test "$?" -eq 29
fi
normal_descendant_pid="$(cat "$normal_descendant_pid_file")"
! /bin/ps -p "$normal_descendant_pid" -o pid= 2>/dev/null | grep -q '[0-9]'
test -z "$(find "$temp_parent" -mindepth 1 -print -quit)"

phase=real-store-manifest-guard
if FINCH_REAL_STORE="$fake_home/.finch/brains/existing" \
  run_isolated bash -c 'printf changed >"$FINCH_REAL_STORE/events.jsonl"'; then
  exit 1
else
  test "$?" -eq 70
fi
printf 'keep me\n' >"$fake_home/.finch/brains/existing/events.jsonl"
test -z "$(find "$temp_parent" -mindepth 1 -print -quit)"

# Manifest failures disclose only the parent-held digests, never path names or
# symlink targets from the real store.
diagnostic="$scratch/manifest-diagnostic"
phase=manifest-directory-status
directory_status=0
FINCH_REAL_STORE="$fake_home/.finch/brains" \
  run_isolated bash -c 'mkdir "$FINCH_REAL_STORE/secret-directory"' \
  2>"$diagnostic" || directory_status=$?
[[ "$directory_status" == 70 ]] || {
  echo "directory manifest adversary returned $directory_status, expected 70" >&2
  exit 1
}
phase=manifest-directory-redaction
if rg -q 'secret-directory|existing|events.jsonl' "$diagnostic"; then
  echo 'directory manifest diagnostic disclosed a protected path' >&2
  exit 1
fi
rmdir "$fake_home/.finch/brains/secret-directory"

phase=manifest-symlink-status
symlink_status=0
FINCH_REAL_STORE="$fake_home/.finch/brains" \
  run_isolated bash -c \
  'ln -s /private/secret-target "$FINCH_REAL_STORE/secret-link"' \
  2>"$diagnostic" || symlink_status=$?
[[ "$symlink_status" == 70 ]] || {
  echo "symlink manifest adversary returned $symlink_status, expected 70" >&2
  exit 1
}
phase=manifest-symlink-redaction
if rg -q 'secret-link|secret-target|existing|events.jsonl' "$diagnostic"; then
  echo 'symlink manifest diagnostic disclosed a protected path' >&2
  exit 1
fi
rm "$fake_home/.finch/brains/secret-link"

case "$(uname -s)" in
  Darwin) original_mode="$(stat -f '%Lp' "$fake_home/.finch/brains/existing/events.jsonl")" ;;
  Linux) original_mode="$(stat -c '%a' "$fake_home/.finch/brains/existing/events.jsonl")" ;;
  *) exit 1 ;;
esac
phase=manifest-mode-status
mode_status=0
run_isolated chmod 600 "$fake_home/.finch/brains/existing/events.jsonl" \
  2>"$diagnostic" || mode_status=$?
[[ "$mode_status" == 70 ]] || {
  echo "mode manifest adversary returned $mode_status, expected 70" >&2
  exit 1
}
chmod "$original_mode" "$fake_home/.finch/brains/existing/events.jsonl"

phase=manifest-fifo-status
fifo_status=0
FINCH_REAL_STORE="$fake_home/.finch/brains" \
  run_isolated bash -c 'mkfifo "$FINCH_REAL_STORE/secret-fifo"' \
  2>"$diagnostic" || fifo_status=$?
[[ "$fifo_status" == 70 ]] || {
  echo "FIFO manifest adversary returned $fifo_status, expected 70" >&2
  exit 1
}
phase=manifest-fifo-redaction
if rg -q 'secret-fifo|existing|events.jsonl' "$diagnostic"; then
  echo 'FIFO manifest diagnostic disclosed a protected path' >&2
  exit 1
fi
rm "$fake_home/.finch/brains/secret-fifo"

socket_path="$fake_home/.finch/brains/secret-socket"
if command -v python3 >/dev/null 2>&1 && [[ "${#socket_path}" -lt 100 ]]; then
  phase=manifest-socket-status
  socket_status=0
  FINCH_REAL_STORE="$fake_home/.finch/brains" \
    run_isolated python3 -c \
    'import os,socket; p=os.environ["FINCH_REAL_STORE"]+"/secret-socket"; s=socket.socket(socket.AF_UNIX); s.bind(p); s.close()' \
    2>"$diagnostic" || socket_status=$?
  [[ "$socket_status" == 70 ]] || {
    echo "socket manifest adversary returned $socket_status, expected 70" >&2
    exit 1
  }
  phase=manifest-socket-redaction
  if rg -q 'secret-socket|existing|events.jsonl' "$diagnostic"; then
    echo 'socket manifest diagnostic disclosed a protected path' >&2
    exit 1
  fi
  rm "$socket_path"
fi

# A first, external signal arriving only after the leader is a retained zombie
# and a TERM-resistant descendant has entered teardown must still determine the
# final status. Sampling only before teardown would miss this signal.
stubborn_pid_file="$scratch/stubborn.pid"
stubborn_ready_file="$scratch/stubborn.ready"
stubborn_term_file="$scratch/stubborn.term"
stubborn_target_file="$scratch/stubborn.target"
stubborn_home_file="$scratch/stubborn.home"
late_signal_file="$scratch/late-signal.observed"
phase=signal-during-teardown
(
  for _ in {1..400}; do
    if [[ -s "$stubborn_target_file" && -s "$stubborn_term_file" ]]; then
      read -r supervisor_pid leader_pid <"$stubborn_target_file"
      printf '%s\n' "$leader_pid" >"$late_signal_file"
      kill -TERM "$supervisor_pid"
      exit 0
    fi
    sleep 0.005
  done
  exit 91
) & signaler_pid=$!
signal_status=0
FINCH_STUBBORN_PID_FILE="$stubborn_pid_file" FINCH_STUBBORN_READY_FILE="$stubborn_ready_file" \
FINCH_STUBBORN_TERM_FILE="$stubborn_term_file" FINCH_STUBBORN_TARGET_FILE="$stubborn_target_file" \
FINCH_STUBBORN_HOME_FILE="$stubborn_home_file" run_isolated bash -c '
  leader_pid=$BASHPID
  python3 -c '\''
import os, signal, time
signal.signal(signal.SIGTERM, lambda *_: open(os.environ["FINCH_STUBBORN_TERM_FILE"], "w").write("term\n"))
signal.signal(signal.SIGHUP, signal.SIG_IGN)
signal.signal(signal.SIGINT, signal.SIG_IGN)
open(os.environ["FINCH_STUBBORN_READY_FILE"], "w").write("ready\n")
while True:
    time.sleep(1)
'\'' &
  while [[ ! -s "$FINCH_STUBBORN_READY_FILE" ]]; do sleep 0.005; done
  printf "%s %s\n" "$FINCH_TEST_SUPERVISOR_PID" "$leader_pid" >"$FINCH_STUBBORN_TARGET_FILE"
  printf "%s\n" "$HOME" >"$FINCH_STUBBORN_HOME_FILE"
  printf "%s\n" "$leader_pid" >"$FINCH_STUBBORN_PID_FILE"
  exit 0
' || signal_status=$?
signaler_status=0
wait "$signaler_pid" || signaler_status=$?
signaler_pid=''
test "$signaler_status" -eq 0
test "$signal_status" -eq 143
test -s "$late_signal_file" && test -s "$stubborn_term_file"
stubborn_group="$(cat "$stubborn_pid_file")"
! kill -0 -- "-$stubborn_group" 2>/dev/null
test ! -e "$(cat "$stubborn_home_file")"
test -z "$(find "$temp_parent" -mindepth 1 -print -quit)"

launchers=(demo_boot.sh smoke_vm_wire_provider.sh stress_test.sh test_persistence.sh test_server.sh test_tool_passthrough.sh test_tui_debug.sh)
phase=launcher-probe-closure
for launcher in "${launchers[@]}"; do
  launcher_probe="$scratch/probe-$launcher"
  FINCH_TEST_LAUNCHER_PROBE_FILE="$launcher_probe" FINCH_TEST_LAUNCHER_PROBE_ONLY=1 \
    run_isolated "$repo_root/scripts/$launcher"
  [[ "$(cat "$launcher_probe")" == "$temp_parent"/finch-brain-test-home.* ]]
done

# Exercise the maintained HTTP launchers beyond their re-exec probe. The
# synthetic Finch consumes inherited FD 11, records the exact sealed bind
# argument, and serves the endpoints each launcher requires.
phase=real-wrapped-server-launchers
mock_debug_dir="$scratch/target/debug"
mock_release_dir="$scratch/target/release"
mock_finch="$mock_debug_dir/finch"
mock_bind_log="$scratch/mock-bind.log"
mkdir -p "$mock_debug_dir" "$mock_release_dir"
printf '%s\n' \
  '#!/usr/bin/env python3' \
  'import json, os, socket, sys' \
  'from http.server import BaseHTTPRequestHandler, HTTPServer' \
  'expected = os.environ["FINCH_TEST_DAEMON_ADDR"]' \
  'if sys.argv[1:] != ["daemon", "--bind", expected]: sys.exit(64)' \
  'sock = socket.socket(fileno=11)' \
  'actual = "%s:%s" % sock.getsockname()' \
  'if actual != expected: sys.exit(65)' \
  'with open(os.environ["FINCH_MOCK_BIND_LOG"], "a") as log: log.write(expected + "|" + actual + "\n")' \
  'with open(os.environ["FINCH_TEST_BOUND_ADDR_FILE"], "w") as address: address.write(actual)' \
  'class Handler(BaseHTTPRequestHandler):' \
  '    def log_message(self, *_): pass' \
  '    def send(self, status, body, kind="application/json"):' \
  '        data = body.encode(); self.send_response(status); self.send_header("Content-Type", kind); self.send_header("Content-Length", str(len(data))); self.end_headers(); self.wfile.write(data)' \
  '    def do_GET(self):' \
  '        if self.path == "/health": self.send(200, "{\"status\":\"ok\"}")' \
  '        elif self.path == "/metrics": self.send(200, "finch_test 1\n", "text/plain")' \
  '        else: self.send(404, "{}")' \
  '    def do_POST(self):' \
  '        request = json.loads(self.rfile.read(int(self.headers.get("Content-Length", "0"))))' \
  '        if any(message.get("role") == "tool" for message in request.get("messages", [])):' \
  '            response = {"choices":[{"message":{"content":"tool result accepted"}}]}' \
  '        else:' \
  '            response = {"choices":[{"message":{"tool_calls":[{"id":"call_test","type":"function","function":{"name":"bash","arguments":"{\\\"command\\\":\\\"ls\\\"}"}}]}}]}' \
  '        self.send(200, json.dumps(response))' \
  'server = HTTPServer(("127.0.0.1", 0), Handler, bind_and_activate=False)' \
  'server.socket = sock; server.server_address = sock.getsockname(); server.server_name = "localhost"; server.server_port = sock.getsockname()[1]' \
  'server.serve_forever()' >"$mock_finch"
chmod +x "$mock_finch"
cp "$mock_finch" "$mock_release_dir/finch"
FINCH_BIN="$mock_finch" FINCH_MOCK_BIND_LOG="$mock_bind_log" \
  run_isolated "$repo_root/scripts/test_server.sh" >/dev/null
FINCH_BIN="$mock_finch" FINCH_MOCK_BIND_LOG="$mock_bind_log" ANTHROPIC_API_KEY=synthetic \
  run_isolated "$repo_root/scripts/test_tool_passthrough.sh" >/dev/null
test "$(wc -l <"$mock_bind_log" | tr -d ' ')" -eq 2
awk -F '|' '$1 != $2 || $1 ~ /:0$/ { exit 1 }' "$mock_bind_log"

phase=debug-release-profile-mismatch-rejected
profile_status=0
FINCH_BIN="$mock_release_dir/finch" FINCH_MOCK_BIND_LOG="$mock_bind_log" \
  run_isolated "$repo_root/scripts/test_server.sh" >/dev/null 2>&1 || profile_status=$?
test "$profile_status" -eq 64

# CI builds the release supervisor before this harness, making this a real
# matched release-profile proof. Developer runs without that artifact retain
# the debug and mismatch coverage above.
release_supervisor="$repo_root/target/release/finch-test-supervisor"
if [[ -x "$release_supervisor" ]]; then
  phase=real-wrapped-release-profile
  FINCH_TEST_REAL_HOME="$fake_home" FINCH_TEST_TMP_PARENT="$temp_parent" \
    FINCH_TEST_SUPERVISOR_BIN="$release_supervisor" FINCH_BIN="$mock_release_dir/finch" \
    FINCH_MOCK_BIND_LOG="$mock_bind_log" "$release_supervisor" \
    "$repo_root/scripts/test_server.sh" >/dev/null
  test -z "$(find "$temp_parent" -mindepth 1 -print -quit)"
fi

# An unrelated same-name process outside the supervisor's group survives.
sentinel_fifo="$scratch/sentinel.control"
phase=unrelated-finch-sentinel
mkfifo "$sentinel_fifo"
exec 7<>"$sentinel_fifo"
bash -c 'exec -a finch bash -c "read -r _"' <&7 & sentinel_pid=$!
mock_finch="$scratch/mock-finch"
printf '%s\n' '#!/bin/bash' 'while :; do sleep 1; done' >"$mock_finch"
chmod +x "$mock_finch"
FINCH_BIN="$mock_finch" run_isolated "$repo_root/scripts/test_tui_debug.sh" >/dev/null
jobs -pr | awk -v pid="$sentinel_pid" '$1 == pid { found=1 } END { exit !found }'
printf '\n' >&7
wait "$sentinel_pid"
sentinel_pid=''

! rg -n 'pkill|killall|127\.0\.0\.1:[1-9][0-9]*|(^|[;&|[:space:]])kill[[:space:]]+-' \
  "$repo_root/docs/AUTOMATIC_TRAINING.md" "$repo_root/docs/DEVELOPMENT.md" "$repo_root/tests/README.md"

# Scan the full executable/test closure. Only the supervisor's group creation,
# the production ambient-daemon detach (which the isolated gate denies first),
# and the deliberate outside-peer adversary may use an escape API.
escape_uses="$(
  phase=escape-api-allowlist
  cd "$repo_root"
  rg --no-heading --no-line-number \
    --glob '*.rs' --glob '*.sh' --glob '!scripts/test_brain_isolation.sh' \
    '(^|[^[:alnum:]_])(setsid|setpgid|process_group\(|set[[:space:]]+-m)' \
    scripts src tests | sed 's/:[[:space:]]*/:/' | sort
)"
expected_escape_uses="$(cat <<'EOF'
src/bin/finch-test-supervisor.rs:if libc::setpgid(0, 0) == -1 {
src/brain/mod.rs:.process_group(0)
src/daemon/spawn.rs:.process_group(0)
EOF
)"
[[ "$escape_uses" == "$expected_escape_uses" ]]

echo 'Brain test isolation regression checks passed.'
