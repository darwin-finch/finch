#!/usr/bin/env bash

# Shell launchers never own or signal test processes. The Rust supervisor is
# the sole process-group authority and removes filesystem state only after the
# owned group is terminated, quiescent, and reaped.
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

brain_test_isolation_is_active() {
  local proof token home root home_identity root_identity brain_addr daemon_addr
  local password_digest socket socket_root socket_root_identity supervisor_pid supervisor_executable supervisor_identity signature
  local links proof_uid proof_mode proof_type actual_password_digest ancestor actual_supervisor_executable
  local library_root expected_supervisor
  [[ "${FINCH_BRAIN_TEST_ISOLATED:-}" == 1 ]] || return 1
  [[ "${FINCH_BRAIN_TEST_PROOF_FD:-}" == 9 ]] || return 1
  [[ "${FINCH_BRAIN_TEST_PROOF_BACKUP_FD:-}" == 108 ]] || return 1
  [[ -n "${FINCH_TEST_SUPERVISOR_BIN:-}" ]] || return 1
  proof="$("$FINCH_TEST_SUPERVISOR_BIN" --verify-inherited-proof 2>/dev/null)" || return 1
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
  supervisor_pid="$(printf '%s\n' "$proof" | sed -n '12p')"
  supervisor_executable="$(printf '%s\n' "$proof" | sed -n '13p')"
  supervisor_identity="$(printf '%s\n' "$proof" | sed -n '14p')"
  signature="$(printf '%s\n' "$proof" | sed -n '15p')"
  [[ "$signature" =~ ^[0-9a-f]{128}$ ]] || return 1
  [[ "$(printf '%s\n' "$proof" | sed -n '16p')" == '' ]] || return 1
  [[ "$token" == "${FINCH_BRAIN_TEST_TOKEN:-}" ]] || return 1
  [[ "$home" == "${HOME:-}" && "$home" == "${FINCH_BRAIN_TEST_HOME:-}" ]] || return 1
  [[ "$root" == "$home/.finch/brains" && "$root" == "${FINCH_BRAIN_TEST_ROOT:-}" ]] || return 1
  [[ "$home_identity" == "$(brain_isolation_file_identity "$home")" ]] || return 1
  [[ "$root_identity" == "$(brain_isolation_file_identity "$root")" ]] || return 1
  [[ "$brain_addr" == "${FINCH_TEST_BRAIN_ADDR:-}" && -n "$brain_addr" ]] || return 1
  [[ "$daemon_addr" == "${FINCH_TEST_DAEMON_ADDR:-}" && -n "$daemon_addr" ]] || return 1
  actual_password_digest="$(printf '%s' "${FINCH_TEST_BRAIN_PASSWORD:-}" | shasum -a 256 | awk '{print $1}')" || return 1
  [[ "$password_digest" == "$actual_password_digest" ]] || return 1
  [[ "$socket" == "${FINCH_TEST_IPC_SOCKET:-}" ]] || return 1
  [[ "$socket_root" == "${FINCH_TEST_SOCKET_ROOT:-}" && "$socket" == "$socket_root/daemon.sock" ]] || return 1
  [[ "$socket_root_identity" == "$(brain_isolation_file_identity "$socket_root")" ]] || return 1
  [[ "${FINCH_TEST_BRAIN_LISTENER_FD:-}" == 10 && "${FINCH_TEST_DAEMON_LISTENER_FD:-}" == 11 ]] || return 1
  [[ "${FINCH_TEST_BRAIN_LISTENER_BACKUP_FD:-}" == 110 && "${FINCH_TEST_DAEMON_LISTENER_BACKUP_FD:-}" == 111 ]] || return 1
  # The trusted Rust verifier above restores and authenticates FD10/FD11 from
  # the sealed backups. Bash may use a low descriptor while reading a script,
  # so the parent shell independently checks the backups that production will
  # restore instead of treating Bash's transient FD10/FD11 as authority.
  perl -MSocket=SOL_SOCKET,SO_TYPE,SOCK_STREAM,sockaddr_in,inet_ntoa -e '
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
    exit(verify_listener(110, $ARGV[0]) && verify_listener(111, $ARGV[1]) ? 0 : 1);
  ' "$brain_addr" "$daemon_addr" || return 1
  perl -MFcntl=F_GETFL,O_ACCMODE,O_RDONLY -e '
    my $flags = fcntl(STDIN, F_GETFL, 0); exit 1 unless defined $flags;
    exit(($flags & O_ACCMODE) == O_RDONLY ? 0 : 1)
  ' <&108 || return 1
  [[ "$supervisor_pid" == "${FINCH_TEST_SUPERVISOR_PID:-}" ]] || return 1
  [[ "$supervisor_executable" == "${FINCH_TEST_SUPERVISOR_BIN:-}" ]] || return 1
  ancestor="$$"
  while [[ "$ancestor" -gt 1 && "$ancestor" != "$supervisor_pid" ]]; do
    ancestor="$(/bin/ps -o ppid= -p "$ancestor" 2>/dev/null | tr -d ' ')" || return 1
  done
  [[ "$ancestor" == "$supervisor_pid" ]] || return 1
  case "$(uname -s)" in
    Darwin)
      actual_supervisor_executable="$(
        /usr/sbin/lsof -a -p "$supervisor_pid" -d txt -Fn 2>/dev/null |
          sed -n 's/^n//p' | head -n 1
      )" || return 1
      ;;
    Linux) actual_supervisor_executable="$(readlink "/proc/$supervisor_pid/exe" 2>/dev/null)" || return 1 ;;
    *) return 1 ;;
  esac
  library_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." 2>/dev/null && pwd -P)" || return 1
  case "$supervisor_executable" in
    "$library_root/target/debug/finch-test-supervisor"|"$library_root/target/release/finch-test-supervisor")
      expected_supervisor="$supervisor_executable" ;;
    *) return 1 ;;
  esac
  [[ -x "$expected_supervisor" ]] || return 1
  [[ "$actual_supervisor_executable" == "$supervisor_executable" ]] || return 1
  [[ "$(brain_isolation_file_identity "$supervisor_executable")" == "$supervisor_identity" ]] || return 1
  if [[ "$(uname -s)" == Darwin ]]; then
    links="$(stat -f '%l' /dev/fd/108)" || return 1
    proof_uid="$(stat -f '%u' /dev/fd/108)" || return 1
    proof_mode="$(stat -f '%Lp' /dev/fd/108)" || return 1
    proof_type="$(stat -f '%HT' /dev/fd/108)" || return 1
  else
    links="$(stat -c '%h' /dev/fd/108)" || return 1
    proof_uid="$(stat -c '%u' /dev/fd/108)" || return 1
    proof_mode="$(stat -c '%a' /dev/fd/108)" || return 1
    proof_type="$(stat -c '%F' /dev/fd/108)" || return 1
  fi
  [[ "$links" == 0 && "$proof_uid" == "$(id -u)" && "$proof_mode" == 400 ]] || return 1
  [[ "$proof_type" == 'Regular File' || "$proof_type" == 'regular file' ]] || return 1
  [[ "$(cd "$home" 2>/dev/null && pwd -P)" == "$home" ]] || return 1
  [[ "$(brain_isolation_resolve_store "$home" 2>/dev/null)" == "$root" ]] || return 1
  [[ "${FINCH_BRAIN_TEST_AUTH_FD:-}" == 109 ]] || return 1
}

brain_test_isolation_require_finch_profile() {
  local finch_bin="$1" finch_path supervisor_path finch_profile supervisor_profile
  finch_path="$(cd "$(dirname "$finch_bin")" 2>/dev/null && pwd -P)/$(basename "$finch_bin")" || return 1
  supervisor_path="${FINCH_TEST_SUPERVISOR_BIN:-}";
  [[ -n "$supervisor_path" ]] || return 1
  supervisor_path="$(cd "$(dirname "$supervisor_path")" 2>/dev/null && pwd -P)/$(basename "$supervisor_path")" || return 1
  finch_profile="$(basename "$(dirname "$finch_path")")"
  supervisor_profile="$(basename "$(dirname "$supervisor_path")")"
  [[ "$finch_profile" == debug || "$finch_profile" == release ]] || return 1
  [[ "$finch_profile" == "$supervisor_profile" ]] || return 1
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
