#!/usr/bin/env bash

# Shell launchers never own or signal test processes. The Rust supervisor is
# the sole process-group authority and removes filesystem state only after the
# owned group is terminated, quiescent, and reaped.
brain_isolation_require_private_path() {
  perl -MCwd=abs_path -MFcntl=:mode -MFile::Basename=dirname -e '
    use strict;
    use warnings;
    my ($input, $strict_leaf, $immutable_leaf) = @ARGV;
    my $path = abs_path($input);
    die "Brain test supervisor path could not be resolved: $input\n" unless defined $path;
    my $euid = $<;
    my $leaf = 1;
    while (1) {
      my @metadata = lstat($path);
      die "Brain test supervisor path disappeared during validation: $path\n"
        unless @metadata;
      my $owner = $metadata[4];
      my $mode = $metadata[2] & 07777;
      my $sticky_directory = S_ISDIR($metadata[2]) && ($mode & 01000);
      my $wrong_owner = $owner != 0 && $owner != $euid;
      $wrong_owner = 1 if $leaf && $strict_leaf && $owner != $euid;
      my $unsafe_write = ($mode & 0022) && (!$sticky_directory || ($leaf && $strict_leaf));
      $unsafe_write = 1 if $leaf && $immutable_leaf && ($mode & 0222);
      if ($wrong_owner || $unsafe_write) {
        printf STDERR "Brain test supervisor path is not private: path=%s owner=%d mode=%04o expected_owner=%d\n",
          $path, $owner, $mode, $euid;
        exit 1;
      }
      last if $path eq "/";
      $path = dirname($path);
      $leaf = 0;
    }
  ' "$1" "${2:-}" "${3:-}"
}

brain_test_isolation_run() {
  local supervisor="${FINCH_TEST_SUPERVISOR_BIN:-}"
  [[ "$#" -gt 0 ]] || { echo 'brain_test_isolation_run requires a command' >&2; return 64; }
  [[ -n "$supervisor" && -x "$supervisor" ]] || {
    echo 'Brain tests require the built finch-test-supervisor binary' >&2
    return 69
  }
  "$supervisor" "$@"
}

brain_isolation_file_identity() {
  local path="$1"
  case "$(uname -s)" in
    Darwin) stat -f '%d:%i' "$path" ;;
    Linux) stat -c '%d:%i' "$path" ;;
    *) return 1 ;;
  esac
}

brain_isolation_resolve_store() {
  local real_home="$1" finch_dir="$1/.finch" store
  [[ ! -L "$finch_dir" ]] || return 64
  if [[ -e "$finch_dir" ]]; then
    [[ -d "$finch_dir" ]] || return 64
    finch_dir="$(cd "$finch_dir" 2>/dev/null && pwd -P)" || return 64
  fi
  store="$finch_dir/brains"
  [[ ! -L "$store" ]] || return 64
  if [[ -e "$store" ]]; then
    [[ -d "$store" ]] || return 64
    store="$(cd "$store" 2>/dev/null && pwd -P)" || return 64
  fi
  printf '%s\n' "$store"
}

brain_isolation_proof_rejected() {
  if [[ "${FINCH_TEST_PROOF_DIAGNOSTICS:-}" == 1 ]]; then
    printf 'Brain test shell authority rejected: %s\n' "$1" >&2
    # Opt-in harness diagnostics: binding identities, never credentials.
    printf 'Brain test shell authority identity: brain_addr=%s daemon_addr=%s socket=%s socket_root=%s supervisor_pid=%s backup_fds=%s,%s,%s' \
      "${FINCH_TEST_BRAIN_ADDR:-}" \
      "${FINCH_TEST_DAEMON_ADDR:-}" \
      "${FINCH_TEST_IPC_SOCKET:-}" \
      "${FINCH_TEST_SOCKET_ROOT:-}" \
      "${FINCH_TEST_SUPERVISOR_PID:-}" \
      "${FINCH_TEST_BRAIN_LISTENER_BACKUP_FD:-}" \
      "${FINCH_TEST_DAEMON_LISTENER_BACKUP_FD:-}" \
      "${FINCH_TEST_IPC_LISTENER_BACKUP_FD:-}" >&2
    if [[ -n "${BRAIN_ISOLATION_ACTIVE_IDENTITY:-}" ]]; then
      printf ' %s' "$BRAIN_ISOLATION_ACTIVE_IDENTITY" >&2
    fi
    printf '\n' >&2
  fi
  return 1
}

# Validate the supervisor profile and bind a content-addressed filename to the
# bytes it names. The Rust verifier first proves that a content-addressed image
# is beside the current test executable; this shell predicate additionally
# requires that externally located authority to be owned, immutable, and on a
# private path. Kept separate so the shell layer can be tested independently.
brain_isolation_supervisor_digest_for_profile() {
  local library_root="$1" supervisor_executable="$2"
  local actual_supervisor_digest supervisor_name supervisor_path_digest=''
  local cargo_target_root='' supervisor_relative=''
  case "$supervisor_executable" in
    "$library_root/target/debug/finch-test-supervisor"|\
    "$library_root/target/debug/finch-test-supervisor-pinned"|\
    "$library_root/target/release/finch-test-supervisor"|\
    "$library_root/target/release/finch-test-supervisor-pinned") ;;
    */finch-test-supervisor-pinned-sha256-*)
      supervisor_name="$(basename "$supervisor_executable")"
      supervisor_path_digest="${supervisor_name#finch-test-supervisor-pinned-sha256-}"
      if [[ ! "$supervisor_path_digest" =~ ^[0-9a-f]{64}$ ]]; then
        brain_isolation_proof_rejected supervisor-content-path-shape
        return 1
      fi
      if [[ ! -f "$supervisor_executable" || -L "$supervisor_executable" ]] ||
        ! brain_isolation_require_private_path "$supervisor_executable" strict immutable; then
        brain_isolation_proof_rejected supervisor-image-authority
        return 1
      fi
      case "$supervisor_executable" in
        "$library_root/target/"*) cargo_target_root="$library_root/target" ;;
        *) cargo_target_root='' ;;
      esac
      if [[ -z "$cargo_target_root" && -n "${CARGO_TARGET_DIR:-}" ]]; then
        cargo_target_root="$(cd "$CARGO_TARGET_DIR" 2>/dev/null && pwd -P)" || {
          brain_isolation_proof_rejected supervisor-target-directory
          return 1
        }
      fi
      [[ -n "$cargo_target_root" ]] || {
        brain_isolation_proof_rejected supervisor-target-directory
        return 1
      }
      case "$supervisor_executable" in
        "$cargo_target_root/"*)
          supervisor_relative="${supervisor_executable#"$cargo_target_root/"}"
          ;;
        *)
          brain_isolation_proof_rejected supervisor-profile
          return 1
          ;;
      esac
      case "$supervisor_relative" in
        */finch-test-supervisor-pinned-sha256-*) ;;
        *)
          brain_isolation_proof_rejected supervisor-profile
          return 1
          ;;
      esac
      supervisor_relative="${supervisor_relative%/*}"
      [[ -n "$supervisor_relative" && "$supervisor_relative" != */*/* ]] || {
        brain_isolation_proof_rejected supervisor-profile
        return 1
      }
      ;;
    *)
      brain_isolation_proof_rejected supervisor-profile
      return 1
      ;;
  esac
  if [[ ! -x "$supervisor_executable" ]]; then
    brain_isolation_proof_rejected supervisor-not-executable
    return 1
  fi
  actual_supervisor_digest="$(set -o pipefail; shasum -a 256 "$supervisor_executable" | awk '{print $1}')" || {
    brain_isolation_proof_rejected supervisor-digest-tool
    return 1
  }
  if [[ ! "$actual_supervisor_digest" =~ ^[0-9a-f]{64}$ ]]; then
    brain_isolation_proof_rejected supervisor-digest-tool
    return 1
  fi
  if [[ -n "$supervisor_path_digest" && "$actual_supervisor_digest" != "$supervisor_path_digest" ]]; then
    brain_isolation_proof_rejected supervisor-content-path-binding
    return 1
  fi
  printf '%s\n' "$actual_supervisor_digest"
}

brain_test_isolation_is_active() {
  local proof token home root home_identity root_identity brain_addr daemon_addr
  local password_digest socket socket_root socket_root_identity ipc_listener_identity
  local supervisor_pid supervisor_executable supervisor_identity supervisor_digest signature
  local actual_supervisor_digest
  local links proof_uid proof_mode proof_type actual_password_digest ancestor actual_supervisor_executable
  local library_root
  local BRAIN_ISOLATION_ACTIVE_IDENTITY=''
  # Bash disables errexit while this function's result is tested as a
  # conditional (`if brain_test_isolation_is_active`). `predicate || helper`
  # therefore cannot stop the function: the helper returns 1 from itself,
  # later successful checks run, and the function returns 0. Every rejection
  # must `return` from this function (#516).
  [[ "${FINCH_BRAIN_TEST_ISOLATED:-}" == 1 ]] || { brain_isolation_proof_rejected isolated-marker; return 1; }
  [[ "${FINCH_BRAIN_TEST_PROOF_FD:-}" == 9 ]] || { brain_isolation_proof_rejected proof-target-fd; return 1; }
  [[ "${FINCH_BRAIN_TEST_PROOF_BACKUP_FD:-}" == 108 ]] || { brain_isolation_proof_rejected proof-backup-fd; return 1; }
  [[ -n "${FINCH_TEST_SUPERVISOR_BIN:-}" ]] || { brain_isolation_proof_rejected supervisor-binary; return 1; }
  if [[ "${FINCH_TEST_PROOF_DIAGNOSTICS:-}" == 1 ]]; then
    proof="$("$FINCH_TEST_SUPERVISOR_BIN" --verify-inherited-proof)" || { brain_isolation_proof_rejected rust-verifier; return 1; }
  else
    proof="$("$FINCH_TEST_SUPERVISOR_BIN" --verify-inherited-proof 2>/dev/null)" || return 1
  fi
  token="$(printf '%s\n' "$proof" | sed -n '1p')"
  home="$(printf '%s\n' "$proof" | sed -n '2p')"
  root="$(printf '%s\n' "$proof" | sed -n '3p')"
  home_identity="$(printf '%s\n' "$proof" | sed -n '4p')"
  root_identity="$(printf '%s\n' "$proof" | sed -n '5p')"
  brain_addr="$(printf '%s\n' "$proof" | sed -n '6p')"
  daemon_addr="$(printf '%s\n' "$proof" | sed -n '7p')"
  password_digest="$(printf '%s\n' "$proof" | sed -n '8p')"
  socket="$(printf '%s\n' "$proof" | sed -n '9p')"
  socket_root="$(printf '%s\n' "$proof" | sed -n '10p')"
  socket_root_identity="$(printf '%s\n' "$proof" | sed -n '11p')"
  ipc_listener_identity="$(printf '%s\n' "$proof" | sed -n '12p')"
  supervisor_pid="$(printf '%s\n' "$proof" | sed -n '13p')"
  supervisor_executable="$(printf '%s\n' "$proof" | sed -n '14p')"
  supervisor_identity="$(printf '%s\n' "$proof" | sed -n '15p')"
  supervisor_digest="$(printf '%s\n' "$proof" | sed -n '16p')"
  signature="$(printf '%s\n' "$proof" | sed -n '17p')"
  BRAIN_ISOLATION_ACTIVE_IDENTITY="home_identity=$home_identity root_identity=$root_identity socket_root_identity=$socket_root_identity ipc_listener_identity=$ipc_listener_identity supervisor_digest=$supervisor_digest"
  [[ "$supervisor_digest" =~ ^[0-9a-f]{64}$ ]] || { brain_isolation_proof_rejected supervisor-digest-shape; return 1; }
  [[ "$signature" =~ ^[0-9a-f]{128}$ ]] || { brain_isolation_proof_rejected signature-shape; return 1; }
  [[ "$(printf '%s\n' "$proof" | sed -n '18p')" == '' ]] || { brain_isolation_proof_rejected trailing-proof-fields; return 1; }
  [[ "$token" == "${FINCH_BRAIN_TEST_TOKEN:-}" ]] || { brain_isolation_proof_rejected token-binding; return 1; }
  [[ "$home" == "${HOME:-}" && "$home" == "${FINCH_BRAIN_TEST_HOME:-}" ]] || { brain_isolation_proof_rejected home-binding; return 1; }
  [[ "$root" == "$home/.finch/brains" && "$root" == "${FINCH_BRAIN_TEST_ROOT:-}" ]] || { brain_isolation_proof_rejected root-binding; return 1; }
  [[ "$home_identity" == "$(brain_isolation_file_identity "$home")" ]] || { brain_isolation_proof_rejected home-identity; return 1; }
  [[ "$root_identity" == "$(brain_isolation_file_identity "$root")" ]] || { brain_isolation_proof_rejected root-identity; return 1; }
  [[ "$brain_addr" == "${FINCH_TEST_BRAIN_ADDR:-}" && -n "$brain_addr" ]] || { brain_isolation_proof_rejected brain-address; return 1; }
  [[ "$daemon_addr" == "${FINCH_TEST_DAEMON_ADDR:-}" && -n "$daemon_addr" ]] || { brain_isolation_proof_rejected daemon-address; return 1; }
  actual_password_digest="$(printf '%s' "${FINCH_TEST_BRAIN_PASSWORD:-}" | shasum -a 256 | awk '{print $1}')" || { brain_isolation_proof_rejected password-digest-tool; return 1; }
  [[ "$password_digest" == "$actual_password_digest" ]] || { brain_isolation_proof_rejected password-binding; return 1; }
  [[ "$socket" == "${FINCH_TEST_IPC_SOCKET:-}" ]] || { brain_isolation_proof_rejected socket-binding; return 1; }
  [[ "$socket_root" == "${FINCH_TEST_SOCKET_ROOT:-}" && "$socket" == "$socket_root/daemon.sock" ]] || { brain_isolation_proof_rejected socket-root-binding; return 1; }
  [[ "$socket_root_identity" == "$(brain_isolation_file_identity "$socket_root")" ]] || { brain_isolation_proof_rejected socket-root-identity; return 1; }
  [[ "${FINCH_TEST_BRAIN_LISTENER_FD:-}" == 10 && "${FINCH_TEST_DAEMON_LISTENER_FD:-}" == 11 && "${FINCH_TEST_IPC_LISTENER_FD:-}" == 12 ]] || { brain_isolation_proof_rejected listener-target-fds; return 1; }
  [[ "${FINCH_TEST_BRAIN_LISTENER_BACKUP_FD:-}" == 110 && "${FINCH_TEST_DAEMON_LISTENER_BACKUP_FD:-}" == 111 && "${FINCH_TEST_IPC_LISTENER_BACKUP_FD:-}" == 112 ]] || { brain_isolation_proof_rejected listener-backup-fds; return 1; }
  # The trusted Rust verifier above restores and authenticates FD10/FD11/FD12 from
  # the sealed backups. Bash may use a low descriptor while reading a script,
  # so the parent shell independently checks the backups that production will
  # restore instead of treating Bash's transient low descriptors as authority.
  perl -MSocket=SOL_SOCKET,SO_TYPE,SOCK_STREAM,sockaddr_in,inet_ntoa,unpack_sockaddr_un -e '
    sub verify_listener {
      my ($fd, $expected) = @_;
      open(my $socket, "<&$fd") or return 0;
      my $type = getsockopt($socket, SOL_SOCKET, SO_TYPE);
      return 0 unless defined($type) && unpack("i", $type) == SOCK_STREAM;
      my $name = getsockname($socket);
      return 0 unless defined($name);
      my ($port, $address) = sockaddr_in($name);
      return inet_ntoa($address) . ":" . $port eq $expected;
    }
    sub verify_ipc_listener {
      my ($fd, $expected_path, $expected_identity) = @_;
      open(my $socket, "<&$fd") or return 0;
      my $type = getsockopt($socket, SOL_SOCKET, SO_TYPE);
      return 0 unless defined($type) && unpack("i", $type) == SOCK_STREAM;
      my $name = getsockname($socket);
      return 0 unless defined($name) && unpack_sockaddr_un($name) eq $expected_path;
      my @stat = stat($socket);
      # The proof producer formats the listener identity unsigned
      # (unsigned_identity: st_dev cast to u64). macOS sockets report
      # st_dev = -1, so a signed stringify can never equal that form and
      # this check could not pass on Darwin; normalize to the producer
      # representation. The binding itself is unchanged: exact equality on
      # the same device and inode (issue #432 finding).
      return @stat && sprintf("%u:%u", $stat[0], $stat[1]) eq $expected_identity;
    }
    exit(
      verify_listener(110, $ARGV[0]) &&
      verify_listener(111, $ARGV[1]) &&
      verify_ipc_listener(112, $ARGV[2], $ARGV[3]) ? 0 : 1
    );
  ' "$brain_addr" "$daemon_addr" "$socket" "$ipc_listener_identity" || { brain_isolation_proof_rejected listener-backup-authority; return 1; }
  perl -MFcntl=F_GETFL,O_ACCMODE,O_RDONLY -e '
    my $flags = fcntl(STDIN, F_GETFL, 0); exit 1 unless defined $flags;
    exit(($flags & O_ACCMODE) == O_RDONLY ? 0 : 1)
  ' <&108 || { brain_isolation_proof_rejected proof-backup-access; return 1; }
  [[ "$supervisor_pid" == "${FINCH_TEST_SUPERVISOR_PID:-}" ]] || { brain_isolation_proof_rejected supervisor-pid-binding; return 1; }
  [[ "$supervisor_executable" == "${FINCH_TEST_SUPERVISOR_BIN:-}" ]] || { brain_isolation_proof_rejected supervisor-path-binding; return 1; }
  ancestor="$$"
  while [[ "$ancestor" -gt 1 && "$ancestor" != "$supervisor_pid" ]]; do
    ancestor="$(/bin/ps -o ppid= -p "$ancestor" 2>/dev/null | tr -d ' ')" || return 1
  done
  [[ "$ancestor" == "$supervisor_pid" ]] || { brain_isolation_proof_rejected supervisor-ancestry; return 1; }
  case "$(uname -s)" in
    Darwin)
      actual_supervisor_executable="$(
        /usr/sbin/lsof -a -p "$supervisor_pid" -d txt -Fn 2>/dev/null |
          sed -n 's/^n//p' | head -n 1
      )" || return 1
      ;;
    Linux) actual_supervisor_executable="$(readlink "/proc/$supervisor_pid/exe" 2>/dev/null)" || return 1 ;;
    *) brain_isolation_proof_rejected unsupported-platform; return 1 ;;
  esac
  library_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." 2>/dev/null && pwd -P)" || return 1
  [[ "$actual_supervisor_executable" == "$supervisor_executable" ]] || { brain_isolation_proof_rejected supervisor-executable-binding; return 1; }
  # Same rule as `verify_supervisor_image` in src/brain/mod.rs: the image digest
  # is checked always, not only when the inode differs, because an in-place
  # overwrite keeps the inode. A byte-identical relink is accepted; anything
  # else is refused (#259).
  actual_supervisor_digest="$(brain_isolation_supervisor_digest_for_profile "$library_root" "$supervisor_executable")" || return 1
  [[ "$actual_supervisor_digest" == "$supervisor_digest" ]] || { brain_isolation_proof_rejected supervisor-executable-substituted; return 1; }
  # fstat the fd itself. Path-based stat differs per platform in ways that
  # do not track isolation: on Linux /dev/fd and /proc/self/fd are magic
  # links whose stat output describes the symlink (st_nlink=1, type
  # "symbolic link") rather than the open proof file, while Darwin's
  # /dev/fd/N are direct device entries. fstat(2) answers one question —
  # what object is sealed onto this fd — identically everywhere.
  local fstat_line
  fstat_line="$(python3 - << 'PYEOF'
import os
import stat as statmod
st = os.fstat(108)
kind = "regular" if statmod.S_ISREG(st.st_mode) else "other"
print(st.st_nlink, st.st_uid, oct(statmod.S_IMODE(st.st_mode))[2:], kind)
PYEOF
)" || return 1
  read -r links proof_uid proof_mode proof_type <<<"$fstat_line"
  [[ "$links" == 0 && "$proof_uid" == "$(id -u)" && "$proof_type" == "regular" ]] || { brain_isolation_proof_rejected proof-backup-metadata; return 1; }
  # Sealed read-only: no write bit may be set (Darwin 0400, Linux runner 0500).
  [[ $(( 8#$proof_mode & 8#222 )) -eq 0 ]] || { brain_isolation_proof_rejected proof-backup-writable; return 1; }
  [[ "$(cd "$home" 2>/dev/null && pwd -P)" == "$home" ]] || { brain_isolation_proof_rejected canonical-home; return 1; }
  [[ "$(brain_isolation_resolve_store "$home" 2>/dev/null)" == "$root" ]] || { brain_isolation_proof_rejected canonical-store; return 1; }
  [[ "${FINCH_BRAIN_TEST_AUTH_FD:-}" == 109 ]] || { brain_isolation_proof_rejected auth-fd; return 1; }
}

brain_test_isolation_require_finch_profile() {
  local finch_bin="$1" supervisor_path finch_parent supervisor_parent
  local finch_real_parent supervisor_real_parent
  finch_parent="$(cd "$(dirname "$finch_bin")" 2>/dev/null && pwd -P)" || return 1
  supervisor_path="${FINCH_TEST_SUPERVISOR_BIN:-}";
  [[ -n "$supervisor_path" ]] || return 1
  supervisor_parent="$(cd "$(dirname "$supervisor_path")" 2>/dev/null && pwd -P)" || return 1
  [[ "$(basename "$finch_parent")" == "$(basename "$supervisor_parent")" ]] || return 1
  if [[ ! -e "$finch_bin" && ! -L "$finch_bin" ]]; then
    [[ "$finch_parent" == "$supervisor_parent" ]] || return 1
    return 0
  fi
  finch_real_parent="$(perl -MCwd=abs_path -MFile::Basename=dirname -e '
    my $path = abs_path($ARGV[0]); exit 1 unless defined $path; print dirname($path)
  ' "$finch_bin")" || return 1
  supervisor_real_parent="$(perl -MCwd=abs_path -MFile::Basename=dirname -e '
    my $path = abs_path($ARGV[0]); exit 1 unless defined $path; print dirname($path)
  ' "$supervisor_path")" || return 1
  [[ "$finch_real_parent" == "$supervisor_real_parent" ]] || return 1
}

brain_test_isolation_reexec_launcher() {
  local launcher="$1" repo_root; shift
  if brain_test_isolation_is_active; then
    if [[ -n "${FINCH_TEST_LAUNCHER_PROBE_FILE:-}" ]]; then
      printf '%s\n' "$HOME" >"$FINCH_TEST_LAUNCHER_PROBE_FILE"
      [[ "${FINCH_TEST_LAUNCHER_PROBE_ONLY:-}" != 1 ]] || exit 0
    fi
    return 0
  fi
  repo_root="$(cd "$(dirname "$launcher")/.." && pwd -P)" || exit 64
  exec "$repo_root/scripts/test_brains.sh" "$launcher" "$@"
}

# Waits for a daemon to publish its bound address, on a wall-clock bound.
#
# Not monotonic: `date +%s` is CLOCK_REALTIME, so a clock step during the wait
# moves the deadline. #328 asked for monotonic deadlines and this is the one
# place that does not deliver one -- bash has no monotonic clock without
# reaching outside it, and the failure mode (a step large enough to matter
# during a two-minute daemon startup) is not worth that. Said plainly rather
# than left to be assumed.
#
# Replaces a bare `for _ in {1..100}; do ... sleep 0.05; done` that expired
# after five seconds and reported only "Daemon did not publish its bound
# address" (#328). On a loaded host that message was indistinguishable from a
# daemon that had crashed on startup, so it names the child's fate instead:
# whether the process is still running, its exit status if not, and where the
# address file was expected.
#
# The 120s default is a hang detector, not a latency claim (#858 sweep): the
# same loaded-runner starvation that measured a 20-30s supervisor spawn cadence
# exhausted the previous 30s default on bind publication alone. Expiry still
# says hung -- the daemon is named as still running or as exited with its
# status -- never "slow".
#
# Usage: await_bound_address <address_file> <daemon_pid> [bound_seconds]
await_bound_address() {
    local address_file="$1" daemon_pid="$2" bound="${3:-120}"
    local deadline=$(( $(date +%s) + bound ))

    while [[ ! -s "$address_file" ]]; do
        # A daemon that has already exited will never publish. Fail now with
        # its status rather than polling out the remaining window.
        if ! kill -0 "$daemon_pid" 2>/dev/null; then
            local status=0
            wait "$daemon_pid" 2>/dev/null || status=$?
            echo "Daemon (pid $daemon_pid) exited with status $status before publishing its bound address to $address_file" >&2
            return 1
        fi
        if (( $(date +%s) >= deadline )); then
            echo "Daemon (pid $daemon_pid) is still running but did not publish its bound address to $address_file within ${bound}s" >&2
            return 1
        fi
        sleep 0.05
    done
}
