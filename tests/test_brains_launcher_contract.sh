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

printf '%s\n' '#!/usr/bin/env bash' \
  'set -euo pipefail' \
  '[[ "$#" -eq 1 && "$1" == -vV ]] || { printf "unexpected rustc arguments:" >&2; printf " <%s>" "$@" >&2; printf "\n" >&2; exit 64; }' \
  'printf "%s\n" "rustc 1.98.0 (launcher contract)" "host: fake-host"' \
  >"$scratch/fake-bin/rustc"
chmod 0755 "$scratch/fake-bin/rustc"

printf '%s\n' '#!/usr/bin/env bash' \
  'set -euo pipefail' \
  '[[ "$#" -eq 1 ]] || { printf "unexpected strip arguments:" >&2; printf " <%s>" "$@" >&2; printf "\n" >&2; exit 64; }' \
  'printf "%s\n" "$1" >>"$FINCH_FAKE_STRIP_CALLS"' \
  >"$scratch/fake-bin/strip"
chmod 0755 "$scratch/fake-bin/strip"

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
  '    "$FINCH_REAL_CARGO" "$@"' \
  '    ;;' \
  '  build)' \
  '    [[ "$#" -eq 9 && "$2" == --quiet && "$3" == --target-dir && "$4" == "${CARGO_TARGET_DIR:-<unset>}" && "$5" == --target && "$6" == fake-host && "$7" == --bin && "$8" == finch-test-supervisor && "$9" == --message-format=json-render-diagnostics ]] || {' \
  '      printf "unexpected Cargo build arguments:" >&2; printf " <%s>" "$@" >&2; printf "\n" >&2; exit 64;' \
  '    }' \
  '    [[ -n "${CARGO_TARGET_DIR:-}" ]] || { echo "Cargo build target was not pinned to metadata result" >&2; exit 64; }' \
  '    if [[ "${FINCH_FAKE_CARGO_FAIL_BUILD:-}" == 1 ]]; then echo "injected Cargo build failure for $CARGO_TARGET_DIR" >&2; exit 42; fi' \
  '    [[ -z "${FINCH_FAKE_BUILD_ATTEMPT:-}" ]] || : >"$FINCH_FAKE_BUILD_ATTEMPT"' \
  '    if [[ -n "${FINCH_FAKE_REQUIRE_PIN:-}" && ! -x "$FINCH_FAKE_REQUIRE_PIN" ]]; then echo "build entered before the preceding source-specific pin was published: $FINCH_FAKE_REQUIRE_PIN" >&2; exit 73; fi' \
  '    profile_dir="$CARGO_TARGET_DIR/fake-host/debug"' \
  '    mkdir -p "$profile_dir"' \
  '    install -m 0755 "$FINCH_FAKE_SUPERVISOR_SOURCE" "$profile_dir/finch-test-supervisor"' \
  '    if [[ -n "${FINCH_FAKE_BUILD_READY:-}" ]]; then' \
  '      : >"$FINCH_FAKE_BUILD_READY"' \
  '      for _ in {1..1000}; do [[ -e "$FINCH_FAKE_BUILD_CONTINUE" ]] && break; sleep 0.01; done' \
  '      [[ -e "$FINCH_FAKE_BUILD_CONTINUE" ]] || { echo "fake Cargo timed out awaiting $FINCH_FAKE_BUILD_CONTINUE" >&2; exit 70; }' \
  '    fi' \
  '    printf '\''{"reason":"compiler-artifact","target":{"name":"finch-test-supervisor","kind":["bin"]},"executable":"%s"}\n'\'' "$profile_dir/finch-test-supervisor"' \
  '    ;;' \
  '  *) echo "unexpected Cargo command: $1" >&2; exit 64 ;;' \
  'esac' \
  >"$scratch/fake-bin/cargo"
chmod 0755 "$scratch/fake-bin/cargo"

run_case_from_repo() {
  local label="$1" expected_target="$2" case_repo="$3"
  shift 3
  local calls="$scratch/$label-cargo-calls"
  local observed_path="$scratch/$label-supervisor-path"
  local observed_args="$scratch/$label-supervisor-args"
  local observed_target="$scratch/$label-supervisor-target"

  env -u FINCH_TEST_SUPERVISOR_BIN \
    PATH="$scratch/fake-bin:/usr/bin:/bin" \
    CARGO_HOME="$scratch/cargo-home" \
    FINCH_REAL_CARGO="$real_cargo" \
    FINCH_FAKE_CARGO_CALLS="$calls" \
    FINCH_FAKE_STRIP_CALLS="$scratch/strip-calls" \
    FINCH_FAKE_SUPERVISOR_SOURCE="$fake_supervisor" \
    FINCH_FAKE_SUPERVISOR_PATH="$observed_path" \
    FINCH_FAKE_SUPERVISOR_ARGS="$observed_args" \
    FINCH_FAKE_SUPERVISOR_TARGET="$observed_target" \
    FINCH_STALE_MARKER="$scratch/stale-supervisor-ran" \
    "$@" "$case_repo/scripts/test_brains.sh" sentinel "two words"

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
  local expected_profile="$expected_target/fake-host/debug"
  case "$(cat "$observed_path")" in
    "$expected_profile/finch-test-supervisor-pinned-sha256-"*) ;;
    *)
      echo "$label: launcher did not execute a content-addressed supervisor under $expected_target; actual=$(cat "$observed_path")" >&2
      exit 1
      ;;
  esac
  case "$(sed -n '1p' "$calls")" in
    metadata\|*\|metadata\|--format-version\|1\|--no-deps) ;;
    *)
      echo "$label: Cargo metadata arguments were not exact" >&2
      sed 's/^/cargo call: /' "$calls" >&2
      exit 1
      ;;
  esac
  if [[ "$(sed -n '2p' "$calls")" != build* || \
    "$(wc -l <"$calls" | tr -d ' ')" != 2 ]]; then
    echo "$label: expected one metadata query followed by one freshness build" >&2
    sed 's/^/cargo call: /' "$calls" >&2
    exit 1
  fi
}

run_case() {
  local label="$1" expected_target="$2"
  shift 2
  run_case_from_repo "$label" "$expected_target" "$scratch/repo" "$@"
}

configured_target="$scratch/configured-target"
printf '[build]\ntarget-dir = "%s"\n' "$configured_target" >"$scratch/cargo-home/config.toml"
mkdir -p "$configured_target/fake-host/debug"
stale_marker="$scratch/stale-supervisor-ran"
printf '%s\n' '#!/bin/sh' 'printf "stale\n" >"$FINCH_STALE_MARKER"' 'exit 88' \
  >"$configured_target/fake-host/debug/finch-test-supervisor"
chmod 0555 "$configured_target/fake-host/debug/finch-test-supervisor"
cp "$configured_target/fake-host/debug/finch-test-supervisor" \
  "$configured_target/fake-host/debug/finch-test-supervisor-pinned"
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

build_target_dir="$scratch/build-target-dir"
run_case build-target "$build_target_dir" env -u CARGO_TARGET_DIR \
  -u FINCH_TEST_SUPERVISOR_BUILD_TARGET_DIR CARGO_BUILD_TARGET_DIR="$build_target_dir" \
  CARGO_BUILD_TARGET=fake-cross-target

multiple_target_home="$scratch/multiple-target-home"
multiple_target_dir="$scratch/multiple-target-dir"
mkdir "$multiple_target_home"
printf '[build]\ntarget-dir = "%s"\ntarget = ["fake-one", "fake-two"]\n' \
  "$multiple_target_dir" >"$multiple_target_home/config.toml"
run_case multiple-build-target "$multiple_target_dir" env -u CARGO_TARGET_DIR \
  -u CARGO_BUILD_TARGET -u CARGO_BUILD_TARGET_DIR \
  -u FINCH_TEST_SUPERVISOR_BUILD_TARGET_DIR CARGO_HOME="$multiple_target_home"

relative_target="relative-target"
relative_expected="$scratch/repo/$relative_target"
run_case relative-env "$relative_expected" env -u FINCH_TEST_SUPERVISOR_BUILD_TARGET_DIR CARGO_TARGET_DIR="$relative_target"

both_env_target="$scratch/cargo-target-wins"
ignored_build_target="$scratch/build-target-must-not-win"
run_case target-env-precedence "$both_env_target" env -u FINCH_TEST_SUPERVISOR_BUILD_TARGET_DIR \
  CARGO_TARGET_DIR="$both_env_target" CARGO_BUILD_TARGET_DIR="$ignored_build_target"
if [[ -e "$ignored_build_target" ]]; then
  echo "target-env-precedence: CARGO_BUILD_TARGET_DIR overrode CARGO_TARGET_DIR" >&2
  exit 1
fi

override_target="$scratch/override-target"
unused_target="$scratch/must-not-be-used"
run_case explicit-override "$override_target" env CARGO_TARGET_DIR="$unused_target" FINCH_TEST_SUPERVISOR_BUILD_TARGET_DIR="$override_target"
if [[ -e "$unused_target" ]]; then
  echo "explicit-override: lower-precedence CARGO_TARGET_DIR was unexpectedly used: $unused_target" >&2
  exit 1
fi

spoof_target="$scratch/internal-mode-spoof-target"
run_case internal-mode-spoof "$spoof_target" env -u FINCH_TEST_SUPERVISOR_BUILD_TARGET_DIR \
  CARGO_TARGET_DIR="$spoof_target" FINCH_TEST_SUPERVISOR_INTERNAL_TARGET="$scratch/spoofed"

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

failed_target="$scratch/failed-build-target"
failed_calls="$scratch/failed-build-calls"
failed_diagnostic="$scratch/failed-build-diagnostic"
failed_execution="$scratch/failed-build-executed"
mkdir -p "$failed_target/fake-host/debug"
printf '%s\n' '#!/bin/sh' 'printf "stale executed\n" >"$FINCH_FAILED_BUILD_EXECUTED"' \
  >"$failed_target/fake-host/debug/finch-test-supervisor"
chmod 0555 "$failed_target/fake-host/debug/finch-test-supervisor"
cp "$failed_target/fake-host/debug/finch-test-supervisor" \
  "$failed_target/fake-host/debug/finch-test-supervisor-pinned"
if env -u FINCH_TEST_SUPERVISOR_BIN -u FINCH_TEST_SUPERVISOR_BUILD_TARGET_DIR \
  PATH="$scratch/fake-bin:/usr/bin:/bin" CARGO_HOME="$scratch/cargo-home" \
  CARGO_TARGET_DIR="$failed_target" FINCH_REAL_CARGO="$real_cargo" \
  FINCH_FAILED_BUILD_EXECUTED="$failed_execution" \
  FINCH_FAKE_CARGO_CALLS="$failed_calls" FINCH_FAKE_CARGO_FAIL_BUILD=1 \
  FINCH_FAKE_STRIP_CALLS="$scratch/strip-calls" \
  FINCH_FAKE_SUPERVISOR_SOURCE="$fake_supervisor" \
  FINCH_FAKE_SUPERVISOR_PATH="$failed_execution" \
  "$scratch/repo/scripts/test_brains.sh" sentinel 2>"$failed_diagnostic"; then
  echo "failed-build: launcher succeeded after Cargo failed" >&2
  exit 1
fi
if [[ -e "$failed_execution" ]]; then
  echo "failed-build: launcher executed a supervisor after Cargo failed: $failed_execution" >&2
  exit 1
fi
if ! grep -Fq "injected Cargo build failure for $failed_target" "$failed_diagnostic" || \
  ! grep -Fq "Cargo failed to build the Brain test supervisor in $failed_target" "$failed_diagnostic" || \
  ! grep -Fq "failed to build and pin the Brain test supervisor in $failed_target" "$failed_diagnostic"; then
  echo "failed-build: rejection did not preserve Cargo and launcher target diagnostics" >&2
  sed 's/^/launcher diagnostic: /' "$failed_diagnostic" >&2
  exit 1
fi

symlink_target="$scratch/symlink-debug-target"
symlink_destination="$scratch/symlink-debug-destination"
symlink_calls="$scratch/symlink-debug-calls"
symlink_diagnostic="$scratch/symlink-debug-diagnostic"
mkdir "$symlink_target" "$symlink_destination"
ln -s "$symlink_destination" "$symlink_target/fake-host"
if env -u FINCH_TEST_SUPERVISOR_BIN -u FINCH_TEST_SUPERVISOR_BUILD_TARGET_DIR \
  PATH="$scratch/fake-bin:/usr/bin:/bin" CARGO_HOME="$scratch/cargo-home" \
  CARGO_TARGET_DIR="$symlink_target" FINCH_REAL_CARGO="$real_cargo" \
  FINCH_FAKE_CARGO_CALLS="$symlink_calls" FINCH_FAKE_STRIP_CALLS="$scratch/strip-calls" \
  "$scratch/repo/scripts/test_brains.sh" sentinel 2>"$symlink_diagnostic"; then
  echo "symlink-debug: launcher accepted a symlink publication directory" >&2
  exit 1
fi
if grep -q '^build|' "$symlink_calls"; then
  echo "symlink-debug: launcher rejected the symlink only after Cargo build executed" >&2
  exit 1
fi
if ! grep -Fq "Brain test supervisor publication directory must not be a symlink: $symlink_target/fake-host" \
  "$symlink_diagnostic"; then
  echo "symlink-debug: rejection did not name the unsafe publication path" >&2
  sed 's/^/launcher diagnostic: /' "$symlink_diagnostic" >&2
  exit 1
fi

trap_target="$scratch/trap-cleanup-target"
trap_profile="$trap_target/fake-host/debug"
trap_digest="$(shasum -a 256 "$fake_supervisor" | awk '{print $1}')"
trap_pin="$trap_profile/finch-test-supervisor-pinned-sha256-$trap_digest"
trap_unrelated="$scratch/unrelated-ambient-staging"
trap_diagnostic="$scratch/trap-cleanup-diagnostic"
mkdir -p "$trap_profile"
printf 'unrelated caller-owned sentinel\n' >"$trap_unrelated"
printf 'conflicting immutable image\n' >"$trap_pin"
chmod 0555 "$trap_pin"
if env -u FINCH_TEST_SUPERVISOR_BIN -u FINCH_TEST_SUPERVISOR_BUILD_TARGET_DIR \
  PATH="$scratch/fake-bin:/usr/bin:/bin" CARGO_HOME="$scratch/cargo-home" \
  CARGO_TARGET_DIR="$trap_target" FINCH_REAL_CARGO="$real_cargo" \
  FINCH_FAKE_CARGO_CALLS="$scratch/trap-cleanup-cargo-calls" \
  FINCH_FAKE_STRIP_CALLS="$scratch/strip-calls" FINCH_FAKE_SUPERVISOR_SOURCE="$fake_supervisor" \
  staging="$trap_unrelated" "$scratch/repo/scripts/test_brains.sh" sentinel \
  2>"$trap_diagnostic"; then
  echo "trap-cleanup: launcher accepted conflicting content at $trap_pin" >&2
  exit 1
fi
if [[ "$(cat "$trap_unrelated")" != 'unrelated caller-owned sentinel' ]]; then
  echo "trap-cleanup: EXIT cleanup altered caller-owned ambient staging path: $trap_unrelated" >&2
  exit 1
fi
if find "$trap_profile" -maxdepth 1 -name '.finch-test-supervisor-staging.*' -print -quit | grep -q .; then
  echo "trap-cleanup: failed publication left a private staging image in $trap_profile" >&2
  find "$trap_profile" -maxdepth 1 -name '.finch-test-supervisor-staging.*' -print >&2
  exit 1
fi
if ! grep -Fq "content-addressed supervisor path is not the expected immutable image: $trap_pin" \
  "$trap_diagnostic"; then
  echo "trap-cleanup: conflicting publication failure did not name the exact pin" >&2
  sed 's/^/launcher diagnostic: /' "$trap_diagnostic" >&2
  exit 1
fi

concurrent_parent="$scratch/concurrent-missing"
concurrent_target="$concurrent_parent/target"
concurrent_ready="$scratch/concurrent-component-ready"
concurrent_continue="$scratch/concurrent-component-continue"
mkdir -p "$concurrent_parent" "$concurrent_ready"
concurrent_status_one=0
concurrent_status_two=0
run_case concurrent-one "$concurrent_target" env -u CARGO_TARGET_DIR \
  -u FINCH_TEST_SUPERVISOR_BUILD_TARGET_DIR CARGO_BUILD_TARGET_DIR="$concurrent_target" \
  FINCH_TEST_TARGET_COMPONENT_READY_DIR="$concurrent_ready" \
  FINCH_TEST_TARGET_COMPONENT_CONTINUE_FILE="$concurrent_continue" &
concurrent_pid_one=$!
run_case concurrent-two "$concurrent_target" env -u CARGO_TARGET_DIR \
  -u FINCH_TEST_SUPERVISOR_BUILD_TARGET_DIR CARGO_BUILD_TARGET_DIR="$concurrent_target" \
  FINCH_TEST_TARGET_COMPONENT_READY_DIR="$concurrent_ready" \
  FINCH_TEST_TARGET_COMPONENT_CONTINUE_FILE="$concurrent_continue" &
concurrent_pid_two=$!
for _ in {1..1000}; do
  [[ "$(find "$concurrent_ready" -type f | wc -l | tr -d ' ')" == 2 ]] && break
  sleep 0.01
done
concurrent_ready_valid=1
if [[ "$(find "$concurrent_ready" -type f | wc -l | tr -d ' ')" == 2 ]]; then
  for ready_record in "$concurrent_ready"/*; do
    [[ "$(cat "$ready_record")" == "$concurrent_target" ]] || concurrent_ready_valid=0
  done
else
  concurrent_ready_valid=0
fi
if [[ "$concurrent_ready_valid" -ne 1 ]]; then
  echo "concurrent-missing: both launchers did not stop before claiming the same absent component: $concurrent_target" >&2
  find "$concurrent_ready" -type f -maxdepth 1 -exec sh -c 'printf "%s: " "$1"; cat "$1"' sh {} \; >&2 || true
  : >"$concurrent_continue"
  wait "$concurrent_pid_one" || true
  wait "$concurrent_pid_two" || true
  exit 1
fi
: >"$concurrent_continue"
wait "$concurrent_pid_one" || concurrent_status_one=$?
wait "$concurrent_pid_two" || concurrent_status_two=$?
if [[ "$concurrent_status_one" -ne 0 || "$concurrent_status_two" -ne 0 ]]; then
  echo "concurrent-missing: launchers did not safely converge on the absent target; first=$concurrent_status_one second=$concurrent_status_two target=$concurrent_target" >&2
  exit 1
fi

race_source_one="$scratch/race-supervisor-one"
race_source_two="$scratch/race-supervisor-two"
cp "$fake_supervisor" "$race_source_one"
cp "$fake_supervisor" "$race_source_two"
chmod 0755 "$race_source_one" "$race_source_two"
printf '# source one\n' >>"$race_source_one"
printf '# source two\n' >>"$race_source_two"
chmod 0555 "$race_source_one" "$race_source_two"
race_target="$scratch/cross-worktree-target"
race_ready="$scratch/cross-worktree-first-build-ready"
race_continue="$scratch/cross-worktree-first-build-continue"
race_second_attempt="$scratch/cross-worktree-second-build-attempt"
race_repo_one="$scratch/race-repo-one"
race_repo_two="$scratch/race-repo-two"
for race_repo in "$race_repo_one" "$race_repo_two"; do
  mkdir -p "$race_repo/scripts/lib" "$race_repo/src"
  cp "$repo_root/scripts/test_brains.sh" "$race_repo/scripts/test_brains.sh"
  cp "$repo_root/scripts/lib/brain_test_isolation.sh" "$race_repo/scripts/lib/brain_test_isolation.sh"
  printf '[package]\nname = "launcher-contract-%s"\nversion = "0.0.0"\nedition = "2021"\n' \
    "$(basename "$race_repo")" >"$race_repo/Cargo.toml"
  printf 'fn main() {}\n' >"$race_repo/src/main.rs"
done
race_digest_one="$(shasum -a 256 "$race_source_one" | awk '{print $1}')"
race_digest_two="$(shasum -a 256 "$race_source_two" | awk '{print $1}')"
race_pin_one="$race_target/fake-host/debug/finch-test-supervisor-pinned-sha256-$race_digest_one"
race_pin_two="$race_target/fake-host/debug/finch-test-supervisor-pinned-sha256-$race_digest_two"
race_status_one=0
race_status_two=0
run_case_from_repo race-one "$race_target" "$race_repo_one" env -u CARGO_TARGET_DIR \
  -u FINCH_TEST_SUPERVISOR_BUILD_TARGET_DIR CARGO_BUILD_TARGET_DIR="$race_target" \
  FINCH_FAKE_SUPERVISOR_SOURCE="$race_source_one" \
  FINCH_TEST_SUPERVISOR_BUILD_READY_FILE="$race_ready" \
  FINCH_TEST_SUPERVISOR_BUILD_CONTINUE_FILE="$race_continue" &
race_pid_one=$!
for _ in {1..1000}; do [[ -e "$race_ready" ]] && break; sleep 0.01; done
if [[ ! -e "$race_ready" ]]; then
  echo "cross-worktree-race: first build did not reach the post-artifact boundary" >&2
  wait "$race_pid_one" || true
  exit 1
fi
run_case_from_repo race-two "$race_target" "$race_repo_two" env -u CARGO_TARGET_DIR \
  -u FINCH_TEST_SUPERVISOR_BUILD_TARGET_DIR CARGO_BUILD_TARGET_DIR="$race_target" \
  FINCH_FAKE_SUPERVISOR_SOURCE="$race_source_two" \
  FINCH_FAKE_BUILD_ATTEMPT="$race_second_attempt" FINCH_FAKE_REQUIRE_PIN="$race_pin_one" &
race_pid_two=$!
for _ in {1..500}; do
  [[ -e "$race_second_attempt" ]] && break
  sleep 0.01
done
overlapped_before_pin=0
[[ ! -e "$race_second_attempt" ]] || overlapped_before_pin=1
: >"$race_continue"
wait "$race_pid_one" || race_status_one=$?
wait "$race_pid_two" || race_status_two=$?
if [[ "$overlapped_before_pin" -ne 0 || "$race_status_one" -ne 0 || "$race_status_two" -ne 0 ]]; then
  echo "cross-worktree-race: build-to-pin critical sections overlapped or failed; overlap=$overlapped_before_pin first=$race_status_one second=$race_status_two target=$race_target" >&2
  exit 1
fi
for expected_pin in "$race_pin_one" "$race_pin_two"; do
  if [[ ! -x "$expected_pin" ]]; then
    echo "cross-worktree-race: launcher did not preserve the source-specific pin: $expected_pin" >&2
    exit 1
  fi
done

case "$(uname -s)" in
  Linux)
    if [[ ! -s "$scratch/strip-calls" ]]; then
      echo "Linux launcher contract did not exercise supervisor stripping" >&2
      exit 1
    fi
    ;;
  Darwin)
    if [[ -e "$scratch/strip-calls" ]]; then
      echo "macOS launcher unexpectedly stripped its supervisor image" >&2
      sed 's/^/strip call: /' "$scratch/strip-calls" >&2
      exit 1
    fi
    ;;
esac

echo "test_brains launcher shared-target contract passed"
