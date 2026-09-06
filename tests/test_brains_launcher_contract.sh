#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
scratch="$(mktemp -d "${TMPDIR:-/tmp}/finch-launcher-contract.XXXXXX")"
scratch="$(cd "$scratch" && pwd -P)"
trap 'rm -rf -- "$scratch"' EXIT

mkdir -p "$scratch/repo/scripts/lib" "$scratch/fake-bin" "$scratch/cargo-home"
cp "$repo_root/scripts/test_brains.sh" "$scratch/repo/scripts/test_brains.sh"
cp "$repo_root/scripts/lib/brain_test_isolation.sh" "$scratch/repo/scripts/lib/brain_test_isolation.sh"
printf '[package]\nname = "launcher-contract"\nversion = "0.0.0"\nedition = "2021"\n' \
  >"$scratch/repo/Cargo.toml"
mkdir "$scratch/repo/src"
printf 'fn main() {}\n' >"$scratch/repo/src/main.rs"
real_cargo="$(rustup which cargo)"

fake_supervisor="$scratch/fake-supervisor"
printf '%s\n' '#!/usr/bin/env bash' \
  'set -euo pipefail' \
  'printf "%s\n" "$0" >"$FINCH_FAKE_SUPERVISOR_PATH"' \
  'printf "%s\n" "$@" >"$FINCH_FAKE_SUPERVISOR_ARGS"' \
  'printf "%s\n" "${CARGO_TARGET_DIR:-<unset>}" >"$FINCH_FAKE_SUPERVISOR_TARGET"' \
  >"$fake_supervisor"
chmod 0555 "$fake_supervisor"

printf '%s\n' '#!/usr/bin/env bash' \
  'set -euo pipefail' \
  'printf "%s|%s" "$1" "${CARGO_TARGET_DIR:-<unset>}" >>"$FINCH_FAKE_CARGO_CALLS"' \
  'printf "|%s" "$@" >>"$FINCH_FAKE_CARGO_CALLS"' \
  'printf "\n" >>"$FINCH_FAKE_CARGO_CALLS"' \
  'case "$1" in' \
  '  metadata)' \
  '    exec "$FINCH_REAL_CARGO" "$@"' \
  '    ;;' \
  '  build)' \
  '    [[ "$#" -eq 6 && "$2" == --quiet && "$3" == --target-dir && "$4" == "${CARGO_TARGET_DIR:-<unset>}" && "$5" == --bin && "$6" == finch-test-supervisor ]] || {' \
  '      printf "unexpected Cargo build arguments:" >&2; printf " <%s>" "$@" >&2; printf "\n" >&2; exit 64;' \
  '    }' \
  '    [[ -n "${CARGO_TARGET_DIR:-}" ]] || { echo "Cargo build target was not pinned to metadata result" >&2; exit 64; }' \
  '    mkdir -p "$CARGO_TARGET_DIR/debug"' \
  '    install -m 0755 "$FINCH_FAKE_SUPERVISOR_SOURCE" "$CARGO_TARGET_DIR/debug/finch-test-supervisor"' \
  '    ;;' \
  '  *) echo "unexpected Cargo command: $1" >&2; exit 64 ;;' \
  'esac' \
  >"$scratch/fake-bin/cargo"
chmod 0755 "$scratch/fake-bin/cargo"

run_case() {
  local label="$1" expected_target="$2"
  shift 2
  local calls="$scratch/$label-cargo-calls"
  local observed_path="$scratch/$label-supervisor-path"
  local observed_args="$scratch/$label-supervisor-args"
  local observed_target="$scratch/$label-supervisor-target"

  env -u FINCH_TEST_SUPERVISOR_BIN \
    PATH="$scratch/fake-bin:/usr/bin:/bin" \
    CARGO_HOME="$scratch/cargo-home" \
    FINCH_REAL_CARGO="$real_cargo" \
    FINCH_FAKE_CARGO_CALLS="$calls" \
    FINCH_FAKE_SUPERVISOR_SOURCE="$fake_supervisor" \
    FINCH_FAKE_SUPERVISOR_PATH="$observed_path" \
    FINCH_FAKE_SUPERVISOR_ARGS="$observed_args" \
    FINCH_FAKE_SUPERVISOR_TARGET="$observed_target" \
    FINCH_STALE_MARKER="$scratch/stale-supervisor-ran" \
    "$@" "$scratch/repo/scripts/test_brains.sh" sentinel "two words"

  if [[ "$(cat "$observed_target")" != "$expected_target" ]]; then
    echo "$label: launcher exported the wrong target; expected=$expected_target actual=$(cat "$observed_target")" >&2
    sed 's/^/cargo call: /' "$calls" >&2
    exit 1
  fi
  if [[ "$(sed -n '1p' "$observed_args")" != sentinel || \
    "$(sed -n '2p' "$observed_args")" != "two words" || \
    "$(wc -l <"$observed_args" | tr -d ' ')" != 2 ]]; then
    echo "$label: supervisor did not receive the exact launcher arguments" >&2
    sed 's/^/supervisor arg: /' "$observed_args" >&2
    exit 1
  fi
  case "$(cat "$observed_path")" in
    "$expected_target/debug/finch-test-supervisor-pinned-sha256-"*) ;;
    *)
      echo "$label: launcher did not execute a content-addressed supervisor under $expected_target; actual=$(cat "$observed_path")" >&2
      exit 1
      ;;
  esac
  if [[ "$(sed -n '1p' "$calls")" != metadata* || "$(sed -n '2p' "$calls")" != build* || \
    "$(wc -l <"$calls" | tr -d ' ')" != 2 ]]; then
    echo "$label: expected one metadata query followed by one freshness build" >&2
    sed 's/^/cargo call: /' "$calls" >&2
    exit 1
  fi
}

configured_target="$scratch/configured-target"
printf '[build]\ntarget-dir = "%s"\n' "$configured_target" >"$scratch/cargo-home/config.toml"
mkdir -p "$configured_target/debug"
stale_marker="$scratch/stale-supervisor-ran"
printf '%s\n' '#!/bin/sh' 'printf "stale\n" >"$FINCH_STALE_MARKER"' 'exit 88' \
  >"$configured_target/debug/finch-test-supervisor"
chmod 0555 "$configured_target/debug/finch-test-supervisor"
cp "$configured_target/debug/finch-test-supervisor" \
  "$configured_target/debug/finch-test-supervisor-pinned"
run_case user-config "$configured_target" env -u CARGO_TARGET_DIR -u FINCH_TEST_SUPERVISOR_BUILD_TARGET_DIR
if [[ -e "$stale_marker" ]]; then
  echo "user-config: launcher executed a stale cached supervisor instead of Cargo's fresh build" >&2
  exit 1
fi

if [[ -e "$scratch/repo/target" ]]; then
  echo "user-config: launcher created a worktree-local target instead of using $configured_target" >&2
  exit 1
fi

relative_config_home="$scratch/relative-config-home"
mkdir "$relative_config_home"
printf '[build]\ntarget-dir = "relative-config-target"\n' >"$relative_config_home/config.toml"
relative_config_target="$scratch/relative-config-target"
run_case relative-config "$relative_config_target" env -u CARGO_TARGET_DIR \
  -u FINCH_TEST_SUPERVISOR_BUILD_TARGET_DIR CARGO_HOME="$relative_config_home"

absolute_target="$scratch/absolute-target"
run_case absolute-env "$absolute_target" env -u FINCH_TEST_SUPERVISOR_BUILD_TARGET_DIR CARGO_TARGET_DIR="$absolute_target"

build_config_env_target="$scratch/build-config-env-target"
run_case build-config-env "$build_config_env_target" env -u CARGO_TARGET_DIR \
  -u FINCH_TEST_SUPERVISOR_BUILD_TARGET_DIR CARGO_BUILD_TARGET_DIR="$build_config_env_target"

relative_target="relative-target"
relative_expected="$scratch/repo/$relative_target"
run_case relative-env "$relative_expected" env -u FINCH_TEST_SUPERVISOR_BUILD_TARGET_DIR CARGO_TARGET_DIR="$relative_target"

override_target="$scratch/override-target"
unused_target="$scratch/must-not-be-used"
run_case explicit-override "$override_target" env CARGO_TARGET_DIR="$unused_target" FINCH_TEST_SUPERVISOR_BUILD_TARGET_DIR="$override_target"
if [[ -e "$unused_target" ]]; then
  echo "explicit-override: lower-precedence CARGO_TARGET_DIR was unexpectedly used: $unused_target" >&2
  exit 1
fi

source "$scratch/repo/scripts/lib/brain_test_isolation.sh"
external_supervisor="$(cat "$scratch/user-config-supervisor-path")"
(unset CARGO_TARGET_DIR; \
  brain_isolation_supervisor_digest_for_profile "$scratch/repo" "$external_supervisor" >/dev/null)
outside_target="$scratch/outside-target"
mkdir -p "$outside_target/debug"
outside_supervisor="$outside_target/debug/finch-test-supervisor"
install -m 0555 "$external_supervisor" "$outside_supervisor"
if CARGO_TARGET_DIR="$outside_target" \
  brain_isolation_supervisor_digest_for_profile "$scratch/repo" "$outside_supervisor" >/dev/null 2>&1; then
  echo "proof profile accepted an external plain supervisor based on mutable environment: $outside_supervisor" >&2
  exit 1
fi

unsafe_target="$scratch/unsafe-target"
mkdir "$unsafe_target"
chmod 0777 "$unsafe_target"
unsafe_calls="$scratch/unsafe-cargo-calls"
unsafe_diagnostic="$scratch/unsafe-diagnostic"
if env -u FINCH_TEST_SUPERVISOR_BIN -u FINCH_TEST_SUPERVISOR_BUILD_TARGET_DIR \
  PATH="$scratch/fake-bin:/usr/bin:/bin" CARGO_HOME="$scratch/cargo-home" \
  CARGO_TARGET_DIR="$unsafe_target" FINCH_REAL_CARGO="$real_cargo" \
  FINCH_FAKE_CARGO_CALLS="$unsafe_calls" \
  "$scratch/repo/scripts/test_brains.sh" sentinel 2>"$unsafe_diagnostic"; then
  echo "unsafe external target unexpectedly received supervisor authority: $unsafe_target" >&2
  exit 1
fi
if grep -q '^build|' "$unsafe_calls"; then
  echo "unsafe external target was rejected only after Cargo build executed: $unsafe_target" >&2
  exit 1
fi
if ! grep -Fq "Brain test supervisor target is not private: path=$unsafe_target" "$unsafe_diagnostic"; then
  echo "unsafe external target rejection did not name its path: $unsafe_target" >&2
  sed 's/^/launcher diagnostic: /' "$unsafe_diagnostic" >&2
  exit 1
fi

writable_target="$scratch/writable-target"
mkdir -p "$writable_target/debug"
install -m 0755 "$external_supervisor" "$writable_target/debug/$(basename "$external_supervisor")"
writable_supervisor="$writable_target/debug/$(basename "$external_supervisor")"
if brain_isolation_supervisor_digest_for_profile "$scratch/repo" "$writable_supervisor" >/dev/null 2>&1; then
  echo "proof profile accepted an owner-writable external supervisor: $writable_supervisor" >&2
  exit 1
fi

echo "test_brains launcher shared-target contract passed"
