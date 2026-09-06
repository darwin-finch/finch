#!/usr/bin/env bash
#
# Repeatable before/after startup benchmark for #364.
#
# This is NOT a correctness gate and must never become one. Issue #242 spent
# four assertions on wall-clock startup properties; three were wrong and one
# reached green CI while depending on the machine being busy. The gate is
# `tests/startup_time_to_ready.rs`, which asserts structure and never a
# duration. This script exists to answer "did it get faster", which is a
# question about a machine and has to be reported with the machine attached.
#
# Usage:
#   scripts/bench_startup_time_to_ready.sh [runs] [brain_count]
#
# Defaults: 15 runs, 113 Brains (the reference dogfood inventory as of
# 2026-09-06). Reports median, p90 and max, because a startup that is usually
# fast and occasionally two seconds is experienced as slow.
#
# Never run against the real ~/.finch: every run gets a disposable HOME, the
# daemon is disabled in its config, and no provider credential is inherited.

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

echo "building finch (release) through the cargo slot..." >&2
"$slot" cargo build --release --bin finch >&2
binary="$repo_root/target/release/finch"
[[ -x "$binary" ]] || { echo "no binary at $binary" >&2; exit 70; }

commit="$(git rev-parse HEAD)"
dirty=""
git diff --quiet || dirty=" (dirty worktree)"

# One pty invocation, portable between macOS and Linux `script`.
run_under_pty() {
  local home="$1"
  if [[ "$(uname -s)" == Darwin ]]; then
    HOME="$home" \
    XDG_CONFIG_HOME="$home/.config" XDG_CACHE_HOME="$home/.cache" \
    XDG_DATA_HOME="$home/.local/share" HF_HOME="$home/.cache/huggingface" \
    TERM=xterm-256color \
    FINCH_STARTUP_TIMINGS="$home/startup-timings.txt" \
    FINCH_BRAIN_TEST_NO_AUTO_SPAWN=1 \
    script -q /dev/null "$binary" >/dev/null 2>&1 <<<'/exit' || true
  else
    HOME="$home" \
    XDG_CONFIG_HOME="$home/.config" XDG_CACHE_HOME="$home/.cache" \
    XDG_DATA_HOME="$home/.local/share" HF_HOME="$home/.cache/huggingface" \
    TERM=xterm-256color \
    FINCH_STARTUP_TIMINGS="$home/startup-timings.txt" \
    FINCH_BRAIN_TEST_NO_AUTO_SPAWN=1 \
    script -qec "$binary" /dev/null >/dev/null 2>&1 <<<'/exit' || true
  fi
}

seed_home() {
  local home="$1" count="$2"
  mkdir -p "$home/.finch/brains"
  cat >"$home/.finch/config.toml" <<'CFG'
[client]
use_daemon = false
auto_spawn = false

[backend]
enabled = false

[memory]
enabled = true
use_neural_embeddings = false
CFG
  local i
  for ((i = 0; i < count; i++)); do
    local dir
    dir="$(printf '%s/.finch/brains/brain-%04d' "$home" "$i")"
    mkdir -p "$dir"
    {
      for seq in 1 2 3 4 5 6 7 8; do
        printf '{"schema_version":1,"seq":%d,"sender":"seed","created_ms":%d}\n' "$seq" "$seq"
      done
    } >"$dir/events.jsonl"
  done
}

scratch="$(mktemp -d "${TMPDIR:-/tmp}/finch-bench-364.XXXXXX")"
trap 'rm -rf "$scratch"' EXIT

samples=()
failed=0
for ((run = 1; run <= runs; run++)); do
  home="$scratch/run-$run"
  seed_home "$home" "$brains"
  run_under_pty "$home"
  if [[ -f "$home/startup-timings.txt" ]]; then
    value="$(awk -F= '/^time_to_ready_ms=/{print $2}' "$home/startup-timings.txt")"
    if [[ "$value" =~ ^[0-9.]+$ ]]; then
      samples+=("$value")
      printf 'run %2d/%d: %s ms\n' "$run" "$runs" "$value" >&2
    else
      failed=$((failed + 1))
      printf 'run %2d/%d: no total reported\n' "$run" "$runs" >&2
    fi
  else
    failed=$((failed + 1))
    printf 'run %2d/%d: no report written\n' "$run" "$runs" >&2
  fi
  rm -rf "$home"
done

if [[ ${#samples[@]} -eq 0 ]]; then
  echo "no successful runs; nothing to report" >&2
  exit 70
fi

# Phase breakdown from the final surviving run, for "what is it spending it on".
last_home="$scratch/phases"
seed_home "$last_home" "$brains"
run_under_pty "$last_home"

printf '%s\n' "${samples[@]}" | sort -g | awk -v runs="$runs" -v failed="$failed" \
  -v commit="$commit" -v dirty="$dirty" -v brains="$brains" '
  { v[NR] = $1 }
  END {
    n = NR
    med = (n % 2) ? v[(n + 1) / 2] : (v[n / 2] + v[n / 2 + 1]) / 2
    p90i = int(n * 0.9); if (p90i < 1) p90i = 1
    printf "\n#364 startup time-to-ready\n"
    printf "commit          %s%s\n", commit, dirty
    printf "runs            %d succeeded, %d failed\n", n, failed
    printf "brain inventory %d synthetic Brains, 8 events each\n", brains
    printf "median          %.1f ms\n", med
    printf "p90             %.1f ms\n", v[p90i]
    printf "min / max       %.1f / %.1f ms\n", v[1], v[n]
  }'

printf '\nmachine\n'
printf '  uname           %s\n' "$(uname -srm)"
if [[ "$(uname -s)" == Darwin ]]; then
  printf '  cpu             %s\n' "$(sysctl -n machdep.cpu.brand_string 2>/dev/null || echo unknown)"
  printf '  cores           %s\n' "$(sysctl -n hw.ncpu 2>/dev/null || echo unknown)"
  printf '  memory          %s bytes\n' "$(sysctl -n hw.memsize 2>/dev/null || echo unknown)"
fi
printf '  load            %s\n' "$(uptime | sed 's/.*load average[s]*: //')"

if [[ -f "$last_home/startup-timings.txt" ]]; then
  printf '\nphase breakdown (one representative run)\n'
  sed 's/^/  /' "$last_home/startup-timings.txt"
fi
