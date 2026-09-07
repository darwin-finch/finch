#!/usr/bin/env bash
#
# Repeatable before/after startup benchmark for #364 ("Instrument and reduce
# Finch interactive TUI time-to-ready").
#
# This is NOT a correctness gate and must never become one. `AGENTS.md` forbids
# asserting a wall-clock startup property, and the lesson is concrete:
# `a0ea2c64` ("assert hydration state, not a wall-clock ratio") replaced the
# last of four such assertions on #242, one of which reached green CI while
# depending on the machine being busy. The gate is
# `tests/startup_time_to_ready.rs`, which asserts structure and never a
# duration. This answers "did it get faster", which is a question about a
# machine and has to be reported with the machine attached.
#
# Usage:
#   scripts/bench_startup_time_to_ready.sh [runs] [brain_count]
#
# Defaults: 15 runs, 113 Brains (the reference dogfood inventory as of
# 2026-09-06). Reports median, nearest-rank p90, min and max, because a startup
# that is usually fast and occasionally two seconds is experienced as slow.
#
# This script launches nothing itself. An earlier version ran
# `script -q /dev/null "$binary"` directly and cleaned up with
# `pkill -f "$binary"` -- which matches any process on the machine whose command
# line contains that path, and is exactly what `AGENTS.md` forbids: "Launchers
# never signal PIDs; the supervisor terminates, proves quiescence, and reaps the
# group." The benchmark is now `bench_startup_time_to_ready` in
# `tests/startup_time_to_ready.rs`, run here through the cargo slot and the
# mandated supervisor launcher. It reuses the same PTY harness as the
# regressions, so it inherits their process discipline, their constructed
# credential isolation (each child's provider-key variables are removed by
# name, and each run gets a disposable HOME with the daemon switched off), and
# their artifact-based synchronisation.
#
# SCOPE, because it is easy to over-read this number. Every benched run sets
# `use_daemon = false`, so `DaemonClient::connect` never runs and GET /health is
# never called. This measures **frontend** time-to-ready. It deliberately does
# not measure the /health Brain enumeration this work also fixes, because doing
# so would require a live daemon and would make the number depend on that
# daemon's warmth rather than on the code.
#
# The /health cost is measured separately and directly by
# `brain::store::tests::bench_list_versus_count_over_a_realistic_brain_root`:
#
#   cargo test --lib bench_list_versus_count -- --ignored --nocapture
#
# with FINCH_BENCH_BRAIN_ROOT pointed at a *copy* of a real Brain root.
# Together the two cover both halves; neither covers both.

set -euo pipefail

runs="${1:-15}"
brains="${2:-113}"

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"

slot="$repo_root/.agents/skills/finch-backlog/scripts/with-cargo-slot"
if [[ ! -x "$slot" ]]; then
  slot="$(cd "$repo_root/.." && pwd)/finch/.agents/skills/finch-backlog/scripts/with-cargo-slot"
fi
if [[ ! -x "$slot" ]]; then
  echo "cargo slot wrapper not found; refusing to build unlocked" >&2
  exit 69
fi

export FINCH_BENCH_STARTUP_RUNS="$runs"
export FINCH_BENCH_STARTUP_BRAINS="$brains"
export CARGO_BUILD_JOBS="${CARGO_BUILD_JOBS:-2}"

exec "$slot" ./scripts/test_brains.sh \
  cargo test --release --test startup_time_to_ready \
  bench_startup_time_to_ready -- --ignored --nocapture --test-threads=1
