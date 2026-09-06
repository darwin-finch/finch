#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
# shellcheck source=scripts/lib/brain_test_isolation.sh
source "$repo_root/scripts/lib/brain_test_isolation.sh"

brain_test_require_private_lexical_path() {
  perl -MFcntl=:mode -e '
    use strict;
    use warnings;
    my ($input) = @ARGV;
    die "Brain test supervisor target must be absolute before lexical validation: $input\n"
      unless $input =~ m{^/};
    my @pending = grep { $_ ne "" } split m{/+}, $input;
    my $euid = $<;
    my $path = "/";
    my $symlink_count = 0;
    while (@pending) {
      my $part = shift @pending;
      next if $part eq ".";
      if ($part eq "..") {
        $path =~ s{/+$}{};
        $path =~ s{/[^/]+$}{};
        $path = "/" if $path eq "";
        next;
      }
      my $next = $path eq "/" ? "/$part" : "$path/$part";
      my @metadata = lstat($next);
      last unless @metadata;
      my $owner = $metadata[4];
      my $mode = $metadata[2] & 07777;
      my $symlink = S_ISLNK($metadata[2]);
      my $sticky_directory = S_ISDIR($metadata[2]) && ($mode & 01000);
      my $wrong_owner = $owner != 0 && $owner != $euid;
      my $unsafe_write = !$symlink && ($mode & 0022) && !$sticky_directory;
      if ($wrong_owner || $unsafe_write) {
        printf STDERR "Brain test supervisor target is not private: path=%s owner=%d mode=%04o expected_owner=%d\n",
          $next, $owner, $mode, $euid;
        exit 1;
      }
      if ($symlink) {
        die "Brain test supervisor target has too many symlink expansions: $next\n"
          if ++$symlink_count > 40;
        my $target = readlink($next);
        die "Brain test supervisor target symlink could not be read: $next\n"
          unless defined $target;
        my @target_parts = grep { $_ ne "" } split m{/+}, $target;
        $path = "/" if $target =~ m{^/};
        unshift @pending, @target_parts;
      } else {
        $path = $next;
      }
    }
  ' "$1"
}

brain_test_resolve_target_dir() {
  local metadata target_override="${FINCH_TEST_SUPERVISOR_BUILD_TARGET_DIR:-}"
  if [[ -n "$target_override" ]]; then
    metadata="$(CARGO_TARGET_DIR="$target_override" cargo metadata --format-version 1 --no-deps)" || {
      echo "could not resolve the explicit Brain test supervisor target with Cargo metadata: $target_override" >&2
      return 74
    }
  else
    metadata="$(cargo metadata --format-version 1 --no-deps)" || {
      echo 'could not resolve Cargo effective target directory for the Brain test supervisor' >&2
      return 74
    }
  fi
  printf '%s' "$metadata" | perl -MJSON::PP -0777 -e '
    use strict;
    use warnings;
    my $metadata = eval { decode_json(<STDIN>) };
    die "Cargo metadata was not valid JSON: $@" unless defined $metadata;
    my $target = $metadata->{target_directory};
    die "Cargo metadata did not report an absolute target_directory\n"
      unless defined($target) && $target =~ m{^/};
    print "$target\n";
  '
}

brain_test_build_and_pin_supervisor() {
  local cargo_target_dir="$1" build_messages built_supervisor publication_dir
  local staging built_digest pinned_supervisor

  build_messages="$(
    umask 077
    cargo build --quiet --target-dir "$cargo_target_dir" --bin finch-test-supervisor \
      --message-format=json-render-diagnostics
  )" || {
    echo "Cargo failed to build the Brain test supervisor in $cargo_target_dir" >&2
    return 74
  }
  built_supervisor="$(printf '%s\n' "$build_messages" | perl -MJSON::PP -ne '
    my $message = eval { decode_json($_) };
    next unless defined($message) && ($message->{reason} // "") eq "compiler-artifact";
    next unless (($message->{target} // {})->{name} // "") eq "finch-test-supervisor";
    next unless grep { $_ eq "bin" } @{($message->{target} // {})->{kind} // []};
    $found = $message->{executable} if defined $message->{executable};
    END {
      die "Cargo build did not report the finch-test-supervisor executable artifact\n"
        unless defined $found;
      print "$found\n";
    }
  ')" || return 74
  case "$built_supervisor" in
    "$cargo_target_dir/"*) ;;
    *)
      echo "Cargo reported the Brain test supervisor outside its target: target=$cargo_target_dir artifact=$built_supervisor" >&2
      return 74
      ;;
  esac
  [[ -f "$built_supervisor" && ! -L "$built_supervisor" ]] || {
    echo "Cargo did not produce a regular, non-symlink supervisor image: $built_supervisor" >&2
    return 74
  }
  publication_dir="$(dirname "$built_supervisor")"
  [[ "$(basename "$publication_dir")" == debug && ! -L "$publication_dir" ]] || {
    echo "Cargo reported the Brain test supervisor outside a non-symlink debug profile: $built_supervisor" >&2
    return 74
  }
  brain_isolation_require_private_path "$publication_dir" strict
  brain_isolation_require_private_path "$built_supervisor" strict

  # Prepare a private complete copy before naming or publishing it. The build
  # lock remains held until publication, so another worktree cannot replace
  # the shared Cargo artifact between Cargo's freshness decision and this copy.
  staging="$publication_dir/.finch-test-supervisor-staging.$$"
  trap 'rm -f "$staging"' EXIT
  install -m 0755 "$built_supervisor" "$staging"
  if [[ "$(uname -s)" == Linux ]]; then
    strip "$staging"
  fi
  chmod 0555 "$staging"
  built_digest="$(shasum -a 256 "$staging" | awk '{print $1}')"
  if [[ ! "$built_digest" =~ ^[0-9a-f]{64}$ ]]; then
    echo "could not compute the selected supervisor image digest for $staging" >&2
    return 74
  fi
  pinned_supervisor="$publication_dir/finch-test-supervisor-pinned-sha256-$built_digest"

  # Deterministic production-boundary concurrency probe. It is inert unless
  # the maintained isolation regression supplies both private paths.
  if [[ -n "${FINCH_TEST_SUPERVISOR_PIN_READY_DIR:-}" || \
    -n "${FINCH_TEST_SUPERVISOR_PIN_CONTINUE_FILE:-}" ]]; then
    if [[ ! -d "${FINCH_TEST_SUPERVISOR_PIN_READY_DIR:-}" || \
      -z "${FINCH_TEST_SUPERVISOR_PIN_CONTINUE_FILE:-}" ]]; then
      echo 'supervisor pin publication probe requires a ready directory and continuation file' >&2
      return 64
    fi
    printf '%s\n' "$pinned_supervisor" >"$FINCH_TEST_SUPERVISOR_PIN_READY_DIR/$$"
    for _ in {1..500}; do
      [[ -e "$FINCH_TEST_SUPERVISOR_PIN_CONTINUE_FILE" ]] && break
      sleep 0.01
    done
    if [[ ! -e "$FINCH_TEST_SUPERVISOR_PIN_CONTINUE_FILE" ]]; then
      echo "supervisor pin publication probe timed out for $pinned_supervisor" >&2
      return 70
    fi
  fi

  if ln "$staging" "$pinned_supervisor" 2>/dev/null; then
    rm -f "$staging"
  elif [[ -f "$pinned_supervisor" && ! -L "$pinned_supervisor" && \
    -x "$pinned_supervisor" ]] && \
    brain_isolation_require_private_path "$pinned_supervisor" strict immutable && \
    cmp -s "$staging" "$pinned_supervisor"; then
    rm -f "$staging"
  else
    echo "content-addressed supervisor path is not the expected immutable image: $pinned_supervisor" >&2
    return 74
  fi
  trap - EXIT
  printf '%s\n' "$pinned_supervisor"
}

brain_test_build_with_lock() {
  local cargo_target_dir="$1" lock_path="$1/.finch-test-supervisor-build.lock"
  shift
  (umask 077 && : >>"$lock_path")
  brain_isolation_require_private_path "$lock_path" strict
  case "$(uname -s)" in
    Darwin) /usr/bin/lockf -k "$lock_path" "$@" ;;
    Linux)
      local flock_path
      flock_path="$(command -v flock)" || {
        echo 'Brain test supervisor build lock requires flock on Linux' >&2
        return 69
      }
      "$flock_path" "$lock_path" "$@"
      ;;
    *)
      echo "Brain test supervisor build lock does not support $(uname -s)" >&2
      return 69
      ;;
  esac
}

if [[ "$#" -eq 0 ]]; then
  set -- cargo test --lib
fi

cd "$repo_root"

if [[ -n "${FINCH_TEST_SUPERVISOR_INTERNAL_TARGET:-}" ]]; then
  internal_target="$FINCH_TEST_SUPERVISOR_INTERNAL_TARGET"
  [[ "$internal_target" == "${CARGO_TARGET_DIR:-}" && \
    "$(cd "$internal_target" 2>/dev/null && pwd -P)" == "$internal_target" ]] || {
    echo "invalid internal Brain test supervisor target: $internal_target" >&2
    exit 64
  }
  brain_isolation_require_private_path "$internal_target" strict
  brain_test_build_and_pin_supervisor "$internal_target"
  exit
fi

# Pin the supervisor image before running anything under it.
#
# `finch-test-supervisor` is a workspace binary target, so the supervised
# `cargo test` relinks it. Cargo replaces a binary by writing a new file and
# renaming it into place, which allocates a new inode — and the wrapper proof
# records the supervisor's inode, so the check fired against the supervisor's
# own rebuild and reported `supervisor executable identity changed` (#259).
#
# CI has pinned since the isolation workflow was written
# (.github/workflows/issue-56-brain-isolation.yml installs both the debug and
# release copies at 0555). Local runs never did, which is why #259 reproduced
# only locally. This closes that divergence. On Linux the authority image is
# stripped before publication because every production-boundary validation
# hashes it; ELF debug sections otherwise turn daemon startup into several
# serial hashes of a needlessly large file. Apple's `strip` rewrites Mach-O
# metadata nondeterministically, so macOS retains the already-small original.
# The selected image is then made 0555 so the launcher never publishes an
# owner-writable authority executable.
#
if [[ -z "${FINCH_TEST_SUPERVISOR_BIN:-}" ]]; then
  # Cargo's fingerprint is the maintained source/build freshness contract.
  # Merely finding an executable here is not enough: after switching
  # revisions, both the plain and pinned paths can still contain the previous
  # checkout's supervisor. In particular, that stale image can predate proof
  # fields or other authority fixes. Never fall back to it when the current
  # supervisor does not build; fail before granting test authority instead.
  # Ask Cargo for its effective target directory so environment variables and
  # user/workspace configuration are interpreted with Cargo's own precedence
  # and relative-path rules. The private override remains available to the
  # maintained stale-cache regression and takes precedence when supplied.
  cargo_target_dir="$(brain_test_resolve_target_dir)"
  brain_test_require_private_lexical_path "$cargo_target_dir"
  if [[ ! -e "$cargo_target_dir" ]]; then
    cargo_target_parent="$cargo_target_dir"
    while [[ ! -e "$cargo_target_parent" ]]; do
      next_parent="$(dirname "$cargo_target_parent")"
      [[ "$next_parent" != "$cargo_target_parent" ]] || break
      cargo_target_parent="$next_parent"
    done
    brain_isolation_require_private_path "$cargo_target_parent"
    cargo_target_current="$(cd "$cargo_target_parent" && pwd -P)"
    cargo_target_missing="${cargo_target_dir#"$cargo_target_parent"}"
    cargo_target_missing="${cargo_target_missing#/}"
    IFS='/' read -r -a cargo_target_components <<<"$cargo_target_missing"
    for cargo_target_component in "${cargo_target_components[@]}"; do
      [[ -n "$cargo_target_component" && "$cargo_target_component" != . ]] || continue
      [[ "$cargo_target_component" != .. ]] || {
        echo "Cargo metadata returned an unresolved parent component: $cargo_target_dir" >&2
        exit 74
      }
      cargo_target_next="$cargo_target_current/$cargo_target_component"
      if ! (umask 077 && mkdir "$cargo_target_next") 2>/dev/null; then
        if [[ ! -d "$cargo_target_next" || -L "$cargo_target_next" ]]; then
          echo "Brain test supervisor target component could not be privately claimed: $cargo_target_next" >&2
          exit 74
        fi
      fi
      brain_isolation_require_private_path "$cargo_target_next" strict
      cargo_target_current="$(cd "$cargo_target_next" && pwd -P)"
    done
    cargo_target_dir="$cargo_target_current"
  fi
  [[ -d "$cargo_target_dir" && ! -L "$cargo_target_dir" ]] || {
    echo "Brain test supervisor target must be a non-symlink directory: $cargo_target_dir" >&2
    exit 74
  }
  cargo_target_dir="$(cd "$cargo_target_dir" && pwd -P)"
  brain_isolation_require_private_path "$cargo_target_dir" strict
  if [[ -L "$cargo_target_dir/debug" ]]; then
    echo "Brain test supervisor publication directory must not be a symlink: $cargo_target_dir/debug" >&2
    exit 74
  fi
  if [[ -e "$cargo_target_dir/debug" ]]; then
    [[ -d "$cargo_target_dir/debug" ]] || {
      echo "Brain test supervisor publication path must be a directory: $cargo_target_dir/debug" >&2
      exit 74
    }
    brain_isolation_require_private_path "$cargo_target_dir/debug" strict
  fi
  export CARGO_TARGET_DIR="$cargo_target_dir"
  supervisor="$(brain_test_build_with_lock "$cargo_target_dir" \
    env CARGO_TARGET_DIR="$cargo_target_dir" \
      FINCH_TEST_SUPERVISOR_INTERNAL_TARGET="$cargo_target_dir" \
      "$repo_root/scripts/test_brains.sh")" || {
    echo "failed to build and pin the Brain test supervisor in $cargo_target_dir" >&2
    exit 74
  }
  unset FINCH_TEST_SUPERVISOR_BUILD_TARGET_DIR
else
  # Explicit callers retain the established plain or fixed-pinned launcher
  # contract. Their artifact lifecycle is deliberately caller-owned.
  supervisor="$FINCH_TEST_SUPERVISOR_BIN"
fi

if [[ -z "${FINCH_TEST_TMP_PARENT:-}" ]]; then
  FINCH_TEST_TMP_PARENT="$(cd "${TMPDIR:-/tmp}" && pwd -P)"
  export FINCH_TEST_TMP_PARENT
fi
if [[ -x "$supervisor" ]]; then
  exec "$supervisor" "$@"
fi
echo "test supervisor is not executable after freshness validation: $supervisor" >&2
exit 69
