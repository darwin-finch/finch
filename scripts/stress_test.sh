#!/usr/bin/env bash
# stress_test.sh — run N concurrent finch sessions and report failures
#
# Usage:
#   ./scripts/stress_test.sh [N] [prompt]
#
# Examples:
#   ./scripts/stress_test.sh 10
#   ./scripts/stress_test.sh 100 "what is 2+2"
#   ./scripts/stress_test.sh 1000 "help"

set -euo pipefail

script_path="$(cd "$(dirname "$0")" && pwd -P)/$(basename "$0")"
source "$(dirname "$script_path")/lib/brain_test_isolation.sh"
brain_test_isolation_reexec_launcher "$script_path" "$@"

N=${1:-10}
PROMPT=${2:-"hello"}
FINCH=${FINCH_BIN:-finch}
TMPDIR_BASE=$(mktemp -d)
TIMEOUT=15

cleanup() { rm -rf "$TMPDIR_BASE"; }
trap cleanup EXIT

echo "stress_test: N=$N prompt='$PROMPT' binary=$FINCH"
echo ""

# Spawn N finch processes in parallel, each with --direct --no-tui
pids=()
outfiles=()
for i in $(seq 1 "$N"); do
    outfile="$TMPDIR_BASE/out_$i"
    outfiles+=("$outfile")
    (
        echo "$PROMPT" | timeout "$TIMEOUT" "$FINCH" --direct --no-tui \
            --initial-prompt "$PROMPT" \
            > "$outfile" 2>&1
        echo $? > "${outfile}.exit"
    ) &
    pids+=($!)
done

# Wait for all
ok=0
fail=0
timeout_count=0
for i in "${!pids[@]}"; do
    pid=${pids[$i]}
    outfile=${outfiles[$i]}
    wait "$pid" 2>/dev/null || true
    exitfile="${outfile}.exit"
    code=0
    [[ -f "$exitfile" ]] && code=$(cat "$exitfile")
    if [[ "$code" == "0" ]]; then
        (( ok++ )) || true
    elif [[ "$code" == "124" ]]; then
        (( timeout_count++ )) || true
    else
        (( fail++ )) || true
        if [[ "$fail" -le 5 ]]; then
            echo "--- FAILURE (session $((i+1)), exit=$code) ---"
            tail -5 "$outfile" 2>/dev/null || true
            echo ""
        fi
    fi
done

total=$((ok + fail + timeout_count))
echo "Results: $total sessions"
echo "  ok:      $ok"
echo "  failed:  $fail"
echo "  timeout: $timeout_count"

[[ "$fail" -gt 0 ]] && exit 1 || exit 0
