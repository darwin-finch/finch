#!/usr/bin/env bash
# Exercise the raw provider -> Finch VM wire path with the configured cloud provider.
#
# This is deliberately a manual smoke test: it sends two tiny requests to the
# configured provider and therefore requires an already-configured credential.
# It prints the returned raw program to stderr, never reads or prints
# credentials. CI should use deterministic fixtures.
#
# Usage:
#   ./scripts/smoke_vm_wire_provider.sh
#   FINCH_BIN=target/release/finch ./scripts/smoke_vm_wire_provider.sh

set -euo pipefail

script_path="$(cd "$(dirname "$0")" && pwd -P)/$(basename "$0")"
source "$(dirname "$script_path")/lib/brain_test_isolation.sh"
brain_test_isolation_reexec_launcher "$script_path" "$@"

finch_bin="${FINCH_BIN:-target/debug/finch}"

if [[ ! -x "$finch_bin" ]]; then
  echo "Finch binary not found or not executable: $finch_bin" >&2
  echo "Build it first with: cargo build" >&2
  exit 1
fi

run_smoke() {
  local name="$1"
  local prompt="$2"
  local expected="$3"
  local output

  output="$("$finch_bin" --cloud-only query --show-program "$prompt")"
  if [[ "$output" != "$expected" ]]; then
    echo "$name VM-wire smoke test failed." >&2
    echo "Expected: $expected" >&2
    echo "Received: $output" >&2
    exit 1
  fi
  echo "ok: $name"
}

run_smoke \
  "Lisp" \
  'Reply only with a complete raw Finch Lisp program which calls say and says exactly: Lisp VM wire smoke test passed. Do not use Markdown or tools.' \
  'Lisp VM wire smoke test passed.'

run_smoke \
  "Co-Forth" \
  'Reply only with one raw complete Finch Co-Forth program. Define a pure typed square word with an explicit stack signature, then use it to say exactly the decimal result of 12 squared. No tools, Markdown, prose, or Lisp.' \
  '144'

echo "Provider VM-wire smoke tests passed."
