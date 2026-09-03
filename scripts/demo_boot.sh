#!/usr/bin/env bash
# demo_boot.sh — show finch compiling and booting from scratch.
# Run this to show someone what Co-Forth looks like when it starts.
#
# Usage:
#   ./scripts/demo_boot.sh            # compile + boot demo
#   ./scripts/demo_boot.sh --no-build # skip build, just show boot demo

set -euo pipefail

script_path="$(cd "$(dirname "$0")" && pwd -P)/$(basename "$0")"
source "$(dirname "$script_path")/lib/brain_test_isolation.sh"
brain_test_isolation_reexec_launcher "$script_path" "$@"

BOLD=$'\033[1m'
DIM=$'\033[2m'
CYAN=$'\033[36m'
GREEN=$'\033[32m'
RESET=$'\033[0m'

BINARY="./target/release/finch"
SKIP_BUILD=false

for arg in "$@"; do
  [[ "$arg" == "--no-build" ]] && SKIP_BUILD=true
done

echo
echo "${BOLD}─── finch / co-forth ───────────────────────────────────────────────${RESET}"
echo

# ── 1. Compile ────────────────────────────────────────────────────────────────

if [[ "$SKIP_BUILD" == false ]]; then
  echo "${CYAN}▶ cargo build --release${RESET}"
  echo
  cargo build --release 2>&1 | \
    grep -E "^(   Compiling|   Finished|error:)" | \
    sed "s/^   Compiling/  compiling/" | \
    sed "s/^   Finished/${GREEN}  ✓ finished${RESET}/"
  echo
fi

VERSION=$("$BINARY" --version 2>/dev/null)
echo "${GREEN}✓ ${VERSION} — ready${RESET}"
echo

# Pull the first returned value out of the typed runtime's JSON envelope.
typed_value() {
  sed -n 's/.*"values":\[{"type":"[a-z]*","value":\([^}]*\)}.*/\1/p' | tr -d '"'
}

# ── 2. Stack machine ──────────────────────────────────────────────────────────

echo "${BOLD}stack machine:${RESET}"
for EXPR in '2 3 +' '10 4 -' '6 7 *'; do
  # `coforth run` has not been a public subcommand for some time, and with
  # 2>/dev/null this printed an empty result rather than failing. The typed VM
  # returns values instead of printing them, so the result comes out of --json.
  RESULT=$("$BINARY" --forth "${EXPR}" --json 2>/dev/null | typed_value)
  echo "  ${DIM}${EXPR} .${RESET}  →  ${CYAN}${RESULT}${RESET}"
done
echo

# ── 3. Define a word ──────────────────────────────────────────────────────────

echo "${BOLD}define a word:${RESET}"
echo "  ${DIM}: square ( S int -- S int ) dup * ;${RESET}"
RESULT=$("$BINARY" --forth ': square ( S int -- S int ) dup * ; 7 square' --json 2>/dev/null | typed_value)
echo "  7 square .  →  ${CYAN}${RESULT}${RESET}"
echo

# ── 4. Distributed ────────────────────────────────────────────────────────────

echo "${BOLD}distributed:${RESET}"
echo "  ${DIM}registry-list${RESET}       list live peers with cpu / ram / bench score"
echo "  ${DIM}slowest${RESET}             address of the slowest live peer → stack"
echo "  ${DIM}slowest on${RESET}          run the next program on that machine"
echo "  ${DIM}最慢 给它${RESET}           same thing, in chinese"
echo

echo "${BOLD}─── boot complete ──────────────────────────────────────────────────${RESET}"
echo
echo "  ${CYAN}finch${RESET}                    enter the REPL"
echo "  ${CYAN}finch daemon${RESET}             start a cluster node"
echo "  ${CYAN}finch --forth '...'${RESET}              run typed Co-Forth directly"
echo
