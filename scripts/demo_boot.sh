#!/usr/bin/env bash
# demo_boot.sh — show finch compiling and booting from scratch.
# Run this to show someone what Co-Forth looks like when it starts.
#
# Usage:
#   ./scripts/demo_boot.sh            # compile + boot demo
#   ./scripts/demo_boot.sh --no-build # skip build, just show boot demo

set -euo pipefail

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

# ── 2. Vocabulary ─────────────────────────────────────────────────────────────

echo "${BOLD}vocabulary loaded:${RESET}"
WORD_COUNT=$("$BINARY" library list 2>/dev/null | head -1 | grep -oE '[0-9]+')
echo "  ${WORD_COUNT} words  (english · 中文 · more in vocabulary/)"
echo

# ── 3. Stack machine ──────────────────────────────────────────────────────────

echo "${BOLD}stack machine:${RESET}"
for EXPR in '2 3 +' '10 4 -' '6 7 *'; do
  RESULT=$("$BINARY" coforth run --code "${EXPR} . cr" 2>/dev/null | tr -d '[:space:]')
  echo "  ${DIM}${EXPR} .${RESET}  →  ${CYAN}${RESULT}${RESET}"
done
echo

# ── 4. Define a word ──────────────────────────────────────────────────────────

echo "${BOLD}define a word:${RESET}"
echo "  ${DIM}: square dup * ;${RESET}"
RESULT=$("$BINARY" coforth run --code ': square dup * ; 7 square . cr' 2>/dev/null | tr -d '[:space:]')
echo "  7 square .  →  ${CYAN}${RESULT}${RESET}"
echo

# ── 5. Chinese vocabulary ─────────────────────────────────────────────────────

echo "${BOLD}chinese vocabulary:${RESET}"
for WORD in '你好' '道' '空'; do
  OUT=$("$BINARY" library show "${WORD}" 2>/dev/null | grep '^output:' | sed 's/^output:[[:space:]]*//')
  FORTH=$("$BINARY" library show "${WORD}" 2>/dev/null | grep '^forth:' | sed 's/^forth:[[:space:]]*//')
  echo "  ${CYAN}${WORD}${RESET}  ${DIM}→ ${FORTH}${RESET}"
  echo "       ${OUT}"
done
echo

# ── 6. Distributed ────────────────────────────────────────────────────────────

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
echo "  ${CYAN}finch coforth run --code '...'${RESET}   run Forth directly"
echo
