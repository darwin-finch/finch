#!/usr/bin/env bash
set -euo pipefail

repository_root=$(git rev-parse --show-toplevel)
cd "$repository_root"

check_format=true
if [[ "${1:-}" == "--metadata-only" ]]; then
  check_format=false
elif [[ $# -ne 0 ]]; then
  echo "usage: $0 [--metadata-only]" >&2
  exit 2
fi

expected_toolchain="1.98.0"
toolchain_file="rust-toolchain.toml"
authoritative_workflows=(
  ".github/workflows/ci.yml"
  ".github/workflows/release.yml"
)

declared_toolchain=$(sed -n 's/^channel = "\([^"]*\)"$/\1/p' "$toolchain_file")
if [[ "$declared_toolchain" != "$expected_toolchain" ]]; then
  echo "rust-toolchain.toml must pin Rust $expected_toolchain; found '${declared_toolchain:-missing}'" >&2
  exit 1
fi

if ! grep -Fxq 'components = ["clippy", "rustfmt"]' "$toolchain_file"; then
  echo "rust-toolchain.toml must install exactly the required clippy and rustfmt components" >&2
  exit 1
fi

for required_target in \
  aarch64-apple-darwin \
  x86_64-pc-windows-msvc \
  x86_64-unknown-linux-gnu
do
  if ! grep -Fxq "    \"$required_target\"," "$toolchain_file"; then
    echo "rust-toolchain.toml is missing supported target '$required_target'" >&2
    exit 1
  fi
done

for workflow in "${authoritative_workflows[@]}"; do
  if grep -Eq 'dtolnay/rust-toolchain@stable|toolchain:[[:space:]]*stable|rust:[[:space:]]*\[stable\]' "$workflow"; then
    echo "$workflow selects moving stable instead of Rust $expected_toolchain" >&2
    exit 1
  fi

  workflow_pins=$(grep -Eo 'dtolnay/rust-toolchain@[^[:space:]]+' "$workflow" || true)
  if [[ -z "$workflow_pins" ]]; then
    echo "$workflow does not install the repository Rust toolchain" >&2
    exit 1
  fi

  unexpected_pins=$(echo "$workflow_pins" \
    | grep -Fvx "dtolnay/rust-toolchain@$expected_toolchain" || true)
  if [[ -n "$unexpected_pins" ]]; then
    echo "$workflow has a toolchain pin that drifts from Rust $expected_toolchain:" >&2
    echo "$unexpected_pins" >&2
    exit 1
  fi

  cargo_jobs_without_pin=$(awk -v expected="dtolnay/rust-toolchain@$expected_toolchain" '
    function finish_job() {
      if (job != "" && invokes_cargo && !has_pin) {
        print job
      }
    }
    /^  [[:alnum:]_-]+:$/ {
      finish_job()
      job = $1
      sub(/:$/, "", job)
      invokes_cargo = 0
      has_pin = 0
      next
    }
    job != "" && $0 ~ /(^|[[:space:]])cargo([[:space:]]|$)/ {
      invokes_cargo = 1
    }
    job != "" && index($0, "uses: " expected) != 0 {
      has_pin = 1
    }
    END {
      finish_job()
    }
  ' "$workflow")
  if [[ -n "$cargo_jobs_without_pin" ]]; then
    echo "$workflow has Cargo jobs without an explicit Rust $expected_toolchain install:" >&2
    echo "$cargo_jobs_without_pin" >&2
    exit 1
  fi
done

actual_rustc=$(rustc --version)
if [[ "$actual_rustc" != "rustc $expected_toolchain "* ]]; then
  echo "expected rustc $expected_toolchain, found: $actual_rustc" >&2
  exit 1
fi

if [[ "$check_format" == true ]]; then
  git diff --quiet --exit-code
  cargo fmt --all -- --check
  git diff --quiet --exit-code
fi
