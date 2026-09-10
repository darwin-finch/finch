#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
slot_wrapper="$repo_root/.agents/skills/finch-backlog/scripts/with-cargo-slot"
# shellcheck source=scripts/lib/brain_test_isolation.sh
source "$repo_root/scripts/lib/brain_test_isolation.sh"

if [[ "$#" -eq 0 ]]; then
  set -- cargo test --lib
fi

# Freshness, immutable publication, and the entire supervised process tree are
# one repository-wide Cargo operation. The wrapper sets its canonical marker
# only after the kernel lock is held; re-entry through an already-wrapped
# caller is therefore idempotent.
cd "$repo_root"
git_common_dir="$(git -C "$repo_root" rev-parse --path-format=absolute --git-common-dir 2>/dev/null)" || {
  echo "Brain test launcher could not resolve its Git common directory: repo=$repo_root" >&2
  exit 69
}
git_common_dir="$(cd "$git_common_dir" && pwd -P)"
if [[ "${FINCH_CARGO_SLOT_HELD:-}" != "$git_common_dir" ]]; then
  exec "$slot_wrapper" "$repo_root/scripts/test_brains.sh" "$@"
fi

cargo_target_arg=''
cargo_target_dir_arg=''
cargo_profile_arg=''
cargo_profile_dir=debug
cargo_profile_build_style=''

brain_test_cargo_selector_error() {
  echo "Brain test launcher cannot safely select the supervisor artifact: $1" >&2
  return 64
}

brain_test_parse_cargo_selectors() {
  local -a command=("$@")
  local index argument option value current subcommand=''
  [[ "${command[0]##*/}" == cargo ]] || return 0

  index=1
  while [[ "$index" -lt "${#command[@]}" ]]; do
    argument="${command[$index]}"
    [[ "$argument" != -- ]] || break
    case "$argument" in
      --config|--config=*)
        brain_test_cargo_selector_error "Cargo --config is unsupported: argument=$argument"
        return 64
        ;;
      --target-dir|--target|--profile|--target-dir=*|--target=*|--profile=*)
        option="${argument%%=*}"
        if [[ "$argument" == "$option" ]]; then
          index=$((index + 1))
          [[ "$index" -lt "${#command[@]}" ]] || {
            brain_test_cargo_selector_error "missing value after $option"
            return 64
          }
          value="${command[$index]}"
        else
          value="${argument#*=}"
        fi
        [[ -n "$value" ]] || {
          brain_test_cargo_selector_error "empty Cargo selector: argument=$argument"
          return 64
        }
        case "$option" in
          --target-dir)
            current="$cargo_target_dir_arg"
            [[ -z "$current" ]] || {
              brain_test_cargo_selector_error "repeated --target-dir selection: previous=$current next=$value"
              return 64
            }
            cargo_target_dir_arg="$value"
            ;;
          --target)
            current="$cargo_target_arg"
            [[ -z "$current" ]] || {
              brain_test_cargo_selector_error "repeated --target selection: previous=$current next=$value"
              return 64
            }
            cargo_target_arg="$value"
            ;;
          --profile)
            current="$cargo_profile_arg"
            [[ -z "$current" ]] || {
              brain_test_cargo_selector_error "repeated/conflicting profile selection: previous=$current next=$value"
              return 64
            }
            cargo_profile_arg="$value"
            cargo_profile_build_style=profile
            ;;
        esac
        ;;
      --release)
        [[ -z "$cargo_profile_arg" ]] || {
          brain_test_cargo_selector_error "repeated/conflicting profile selection: previous=$cargo_profile_arg next=release"
          return 64
        }
        cargo_profile_arg=release
        cargo_profile_build_style=release
        ;;
      +*) ;;
      -*) ;;
      *)
        if [[ -z "$subcommand" ]]; then
          subcommand="$argument"
          [[ "$subcommand" == test ]] || {
            brain_test_cargo_selector_error "Cargo aliases and non-test subcommands are unsupported: subcommand=$subcommand"
            return 64
          }
        fi
        ;;
    esac
    index=$((index + 1))
  done
  [[ "$subcommand" == test ]] || {
    brain_test_cargo_selector_error 'direct Cargo invocation has no supported test subcommand before --'
    return 64
  }

  case "$cargo_profile_arg" in
    '') cargo_profile_dir=debug ;;
    dev) cargo_profile_dir=debug ;;
    *) cargo_profile_dir="$cargo_profile_arg" ;;
  esac
}

brain_test_select_cargo_target_dir() {
  local metadata metadata_target_override=''
  local -a metadata_args=(metadata --format-version 1 --no-deps)
  if [[ -n "$cargo_target_dir_arg" ]]; then
    metadata_target_override="$cargo_target_dir_arg"
  elif [[ -n "${FINCH_TEST_SUPERVISOR_BUILD_TARGET_DIR:-}" ]]; then
    metadata_target_override="$FINCH_TEST_SUPERVISOR_BUILD_TARGET_DIR"
  fi
  if [[ -n "$metadata_target_override" ]]; then
    if ! metadata="$(CARGO_TARGET_DIR="$metadata_target_override" cargo "${metadata_args[@]}")"; then
      echo "Cargo metadata failed while resolving the effective Brain test target: CARGO_TARGET_DIR=$metadata_target_override argv=${metadata_args[*]}" >&2
      return 74
    fi
  elif ! metadata="$(cargo "${metadata_args[@]}")"; then
    echo "Cargo metadata failed while resolving the effective Brain test target: argv=${metadata_args[*]}" >&2
    return 74
  fi
  cargo_target_dir="$(printf '%s' "$metadata" | perl -MJSON::PP -0777 -e '
    use strict;
    use warnings;
    my $document = eval { decode_json(<STDIN>) };
    die "Cargo metadata was not valid JSON: $@" unless defined $document;
    my $target = $document->{target_directory};
    die "Cargo metadata did not report an absolute target_directory\n"
      unless defined($target) && $target =~ m{^/};
    print "$target\n";
  ')" || return 74
}

brain_test_validate_artifact_path() {
  local reported_supervisor="$1" canonical_parent canonical_supervisor expected_parent artifact_target
  [[ -f "$reported_supervisor" && ! -L "$reported_supervisor" && -x "$reported_supervisor" ]] || {
    echo "Cargo did not report a regular host-runnable Brain test supervisor: artifact=$reported_supervisor" >&2
    return 74
  }
  canonical_parent="$(cd "$(dirname "$reported_supervisor")" 2>/dev/null && pwd -P)" || {
    echo "Cargo-reported Brain test supervisor parent could not be resolved: artifact=$reported_supervisor" >&2
    return 74
  }
  canonical_supervisor="$canonical_parent/$(basename "$reported_supervisor")"
  if [[ -n "$cargo_target_arg" ]]; then
    artifact_target="$cargo_target_arg"
    if [[ "$artifact_target" == */* || "$artifact_target" == *.json ]]; then
      artifact_target="$(basename "$artifact_target")"
      artifact_target="${artifact_target%.json}"
    fi
    expected_parent="$cargo_target_dir/$artifact_target/$cargo_profile_dir"
    [[ "$canonical_parent" == "$expected_parent" ]] || {
      echo "Cargo reported the Brain test supervisor outside the selected target/profile: expected=$expected_parent/finch-test-supervisor actual=$canonical_supervisor" >&2
      return 74
    }
  else
    case "$canonical_parent" in
      "$cargo_target_dir/$cargo_profile_dir") ;;
      "$cargo_target_dir/"*"/$cargo_profile_dir")
        artifact_target="${canonical_parent#"$cargo_target_dir/"}"
        artifact_target="${artifact_target%"/$cargo_profile_dir"}"
        [[ -n "$artifact_target" && "$artifact_target" != */* ]] || {
          echo "Cargo reported the Brain test supervisor outside one effective target/profile: target=$cargo_target_dir profile=$cargo_profile_dir actual=$canonical_supervisor" >&2
          return 74
        }
        ;;
      *)
        echo "Cargo reported the Brain test supervisor outside its effective target/profile: target=$cargo_target_dir profile=$cargo_profile_dir actual=$canonical_supervisor" >&2
        return 74
        ;;
    esac
  fi
  brain_isolation_require_private_path "$canonical_supervisor" strict
  if ! perl -e '
    open STDOUT, ">", "/dev/null" or die $!;
    open STDERR, ">", "/dev/null" or die $!;
    system {$ARGV[0]} $ARGV[0], "--verify-inherited-proof";
    exit(($? == -1 || ($? & 127)) ? 1 : 0);
  ' "$canonical_supervisor"; then
    echo "Cargo-reported Brain test supervisor is not host-runnable: artifact=$canonical_supervisor" >&2
    return 74
  fi
  built_supervisor="$canonical_supervisor"
}

brain_test_build_supervisor() {
  local build_messages
  local -a build_args=(build --bin finch-test-supervisor --message-format=json-render-diagnostics)
  if [[ -n "$cargo_target_dir_arg" ]]; then
    build_args+=(--target-dir "$cargo_target_dir_arg")
  elif [[ -n "${FINCH_TEST_SUPERVISOR_BUILD_TARGET_DIR:-}" ]]; then
    build_args+=(--target-dir "$FINCH_TEST_SUPERVISOR_BUILD_TARGET_DIR")
  fi
  [[ -z "$cargo_target_arg" ]] || build_args+=(--target "$cargo_target_arg")
  case "$cargo_profile_build_style" in
    release) build_args+=(--release) ;;
    profile) build_args+=(--profile "$cargo_profile_arg") ;;
  esac

  if ! build_messages="$(umask 077; cargo "${build_args[@]}")"; then
    echo "Cargo failed to freshness-build the Brain test supervisor: target=$cargo_target_dir profile=$cargo_profile_dir argv=${build_args[*]}" >&2
    return 74
  fi
  cargo_target_dir="$(cd "$cargo_target_dir" 2>/dev/null && pwd -P)" || {
    echo "Cargo effective target directory does not exist after the supervisor build: $cargo_target_dir" >&2
    return 74
  }
  brain_isolation_require_private_path "$cargo_target_dir" strict
  built_supervisor="$(printf '%s\n' "$build_messages" | perl -MJSON::PP -ne '
    use strict;
    use warnings;
    our @found;
    next if /^\s*$/;
    my $message = eval { decode_json($_) };
    die "Cargo build emitted invalid JSON: $@" unless defined $message;
    next unless ($message->{reason} // "") eq "compiler-artifact";
    my $target = $message->{target} // {};
    next unless ($target->{name} // "") eq "finch-test-supervisor";
    next unless grep { $_ eq "bin" } @{$target->{kind} // []};
    push @found, $message->{executable} if defined $message->{executable};
    END {
      die sprintf("Cargo build reported %d executable finch-test-supervisor artifacts; expected exactly one\n", scalar @found)
        unless @found == 1;
      print "$found[0]\n";
    }
  ')" || return 74
  brain_test_validate_artifact_path "$built_supervisor"
}

brain_test_cleanup_staging() {
  [[ -z "${brain_test_staging:-}" ]] || rm -f -- "$brain_test_staging"
}

brain_test_pin_supervisor() {
  local publication_dir built_digest pinned_supervisor
  publication_dir="$(dirname "$built_supervisor")"
  brain_test_staging="$(mktemp "$publication_dir/.finch-test-supervisor-staging.XXXXXX")" || {
    echo "could not create private Brain test supervisor staging in $publication_dir" >&2
    return 74
  }
  trap brain_test_cleanup_staging EXIT
  install -m 0700 "$built_supervisor" "$brain_test_staging"
  if [[ "$(uname -s)" == Linux ]]; then
    strip "$brain_test_staging"
  fi
  chmod 0555 "$brain_test_staging"
  built_digest="$(shasum -a 256 "$brain_test_staging" | awk '{print $1}')"
  [[ "$built_digest" =~ ^[0-9a-f]{64}$ ]] || {
    echo "could not compute the selected supervisor image digest: staging=$brain_test_staging" >&2
    return 74
  }
  pinned_supervisor="$publication_dir/finch-test-supervisor-pinned-sha256-$built_digest"

  # The old publication probe rendezvoused concurrent launchers. The repository
  # slot now deliberately serializes them, so retain its observation without a
  # continuation wait for the existing isolation self-test.
  if [[ -n "${FINCH_TEST_SUPERVISOR_PIN_READY_DIR:-}" ||
    -n "${FINCH_TEST_SUPERVISOR_PIN_CONTINUE_FILE:-}" ]]; then
    if [[ ! -d "${FINCH_TEST_SUPERVISOR_PIN_READY_DIR:-}" ||
      -z "${FINCH_TEST_SUPERVISOR_PIN_CONTINUE_FILE:-}" ]]; then
      echo "supervisor pin publication probe requires a ready directory and continuation file: ready=${FINCH_TEST_SUPERVISOR_PIN_READY_DIR:-<unset>} continue=${FINCH_TEST_SUPERVISOR_PIN_CONTINUE_FILE:-<unset>}" >&2
      return 64
    fi
    printf '%s\n' "$pinned_supervisor" >"$FINCH_TEST_SUPERVISOR_PIN_READY_DIR/$$"
  fi

  if ln "$brain_test_staging" "$pinned_supervisor" 2>/dev/null; then
    rm -f -- "$brain_test_staging"
    brain_test_staging=''
  elif [[ -f "$pinned_supervisor" && ! -L "$pinned_supervisor" && -x "$pinned_supervisor" ]] &&
    brain_isolation_require_private_path "$pinned_supervisor" strict immutable &&
    cmp -s "$brain_test_staging" "$pinned_supervisor"; then
    rm -f -- "$brain_test_staging"
    brain_test_staging=''
  else
    echo "content-addressed supervisor path is not the expected owned immutable image: expected=$pinned_supervisor staging=$brain_test_staging" >&2
    return 74
  fi
  trap - EXIT
  supervisor="$pinned_supervisor"
}

supervisor="${FINCH_TEST_SUPERVISOR_BIN:-}"
brain_test_staging=''
if [[ -z "$supervisor" ]]; then
  brain_test_parse_cargo_selectors "$@"
  brain_test_select_cargo_target_dir
  brain_test_build_supervisor
  brain_test_pin_supervisor
  export CARGO_TARGET_DIR="$cargo_target_dir"
  unset FINCH_TEST_SUPERVISOR_BUILD_TARGET_DIR
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
