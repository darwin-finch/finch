#!/usr/bin/env bash
set -euo pipefail

if [[ "${1:-}" != "--apply" ]]; then
  echo "Refusing to rewrite Rust files without --apply." >&2
  echo "Run only after the active Rust frontier has merged and issue #191 is authorized." >&2
  exit 2
fi

repository_root=$(git rev-parse --show-toplevel)
cd "$repository_root"

expected_toolchain="1.98.0"
manifest="docs/RUSTFMT_1_98_MIGRATION_MANIFEST.txt"
expected_manifest_sha256="16ea6402375aeffd18c972dbcd33d7777065d8f8be533fb8a1db368b6ebdbd13"

actual_manifest_sha256=$(shasum -a 256 "$manifest" | awk '{print $1}')
if [[ "$actual_manifest_sha256" != "$expected_manifest_sha256" ]]; then
  echo "The audited issue #191 manifest hash does not match" >&2
  exit 1
fi

if [[ $(rustc +"$expected_toolchain" --version) != "rustc $expected_toolchain "* ]]; then
  echo "Rust $expected_toolchain is required for the mechanical migration" >&2
  exit 1
fi

if [[ -n $(git status --short) ]]; then
  echo "The formatting migration must start from a clean checkout" >&2
  exit 1
fi

cargo +"$expected_toolchain" fmt --all

actual_manifest=$(mktemp)
trap 'rm -f "$actual_manifest"' EXIT
git diff --name-only -- '*.rs' | sort > "$actual_manifest"

if ! diff -u "$manifest" "$actual_manifest"; then
  echo "The Rust 1.98 formatting footprint differs from the audited issue #191 manifest" >&2
  exit 1
fi

unexpected_files=$(git diff --name-only | grep -Ev '\.rs$' || true)
if [[ -n "$unexpected_files" ]]; then
  echo "The mechanical migration changed non-Rust files:" >&2
  echo "$unexpected_files" >&2
  exit 1
fi

git diff --check
echo "Audited Rust 1.98 formatting migration is ready for its isolated commit."
