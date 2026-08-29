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
expected_manifest_sha256="a9e4cc6250f6fcfe89fc4c5a45f2f871eb03753838c42026d0faae0d1cdddea4"
expected_rust_preimage_sha256="db2911ff9c8e252898363757cfadd2bde3350f2c9a10178914fdc2b8e330d5fa"

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

preimage=$(mktemp)
actual_manifest=$(mktemp)
trap 'rm -f "$preimage" "$actual_manifest"' EXIT
while IFS= read -r rust_file; do
  printf '%s  %s\n' "$(git hash-object "$rust_file")" "$rust_file"
done < <(git ls-files '*.rs' | sort) > "$preimage"
actual_rust_preimage_sha256=$(shasum -a 256 "$preimage" | awk '{print $1}')
if [[ "$actual_rust_preimage_sha256" != "$expected_rust_preimage_sha256" ]]; then
  echo "The Rust source preimage changed; re-audit issue #191 after rebasing before formatting" >&2
  exit 1
fi

cargo +"$expected_toolchain" fmt --all

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
