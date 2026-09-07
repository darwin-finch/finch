#!/usr/bin/env bash
# Install a reviewed fixed-digest sccache and enable its bounded local backend.
# This helper intentionally has no GitHub Actions cache-service credentials.

set -euo pipefail

if [[ -z "${RUNNER_TEMP:-}" || -z "${GITHUB_WORKSPACE:-}" || -z "${GITHUB_ENV:-}" || \
      -z "${GITHUB_PATH:-}" || -z "${GITHUB_OUTPUT:-}" ]]; then
  echo "RUNNER_TEMP, GITHUB_WORKSPACE, GITHUB_ENV, GITHUB_PATH, and GITHUB_OUTPUT are required for local sccache" >&2
  exit 1
fi

if [[ "${GITHUB_EVENT_NAME:-}" != "push" || "${GITHUB_REF:-}" != "refs/heads/main" ]]; then
  echo "compiler objects are disabled outside trusted main pushes"
  exit 0
fi

case "${RUNNER_OS:-}/${RUNNER_ARCH:-}" in
  Linux/X64)
    platform=x86_64-unknown-linux-musl
    expected_sha256=67c4a96dd237c1f518f6b36083f270f9976d516f1e57fce891755ea782e50006
    ;;
  macOS/ARM64)
    platform=aarch64-apple-darwin
    expected_sha256=0c560bfba31aef5bdfb4fb3d2677f6e61d71c5c00952f2a83344f47aa31f00f1
    ;;
  *)
    echo "no reviewed sccache binary for ${RUNNER_OS:-unset}/${RUNNER_ARCH:-unset}; using ordinary rustc"
    exit 0
    ;;
esac

version=v0.17.0
asset="sccache-${version}-${platform}.tar.gz"
archive="${RUNNER_TEMP}/${asset}"
install_root="${RUNNER_TEMP}/finch-sccache-bin-${version}"
member="sccache-${version}-${platform}/sccache"
url="https://github.com/mozilla/sccache/releases/download/${version}/${asset}"

# The reviewed Linux v0.17.0 archive is 9,561,816 bytes. Keep a narrow 12 MiB
# transport ceiling while leaving enough room for that larger platform asset.
if ! curl --fail --location --retry 3 --max-filesize 12582912 --proto '=https' --tlsv1.2 --output "${archive}" "${url}"; then
  echo "fixed-digest sccache download failed; using ordinary rustc" >&2
  exit 0
fi
if ! printf '%s  %s\n' "${expected_sha256}" "${archive}" | shasum -a 256 --check --status; then
  rm -f "${archive}"
  echo "fixed-digest sccache verification failed; using ordinary rustc" >&2
  exit 0
fi

archive_members=$(tar -tzf "${archive}") || {
  echo "verified sccache archive could not be listed; using ordinary rustc" >&2
  exit 0
}
expected_members=$(printf '%s\n' \
  "sccache-${version}-${platform}/" \
  "${member}" \
  "sccache-${version}-${platform}/LICENSE" \
  "sccache-${version}-${platform}/README.md")
if [[ "${archive_members}" != "${expected_members}" ]]; then
  echo "verified sccache archive has unexpected members; using ordinary rustc" >&2
  exit 0
fi
if tar -tvzf "${archive}" | awk '$1 !~ /^[d-]/ { exit 1 }'; then
  :
else
  echo "verified sccache archive contains a link or special file; using ordinary rustc" >&2
  exit 0
fi

mkdir -p "${install_root}"
sccache_path="${install_root}/sccache"
if ! tar -xOzf "${archive}" "${member}" >"${sccache_path}"; then
  rm -f "${sccache_path}"
  echo "verified sccache executable could not be extracted; using ordinary rustc" >&2
  exit 0
fi
chmod 0755 "${sccache_path}"

wrapper_path="${GITHUB_WORKSPACE}/scripts/ci_rustc_cache_wrapper.sh"
if [[ ! -x "${wrapper_path}" ]]; then
  echo "reviewed rustc cache wrapper is missing or not executable" >&2
  exit 1
fi

cache_dir="${RUNNER_TEMP}/finch-sccache-cache"
mkdir -p "${cache_dir}"
{
  echo "RUSTC_WRAPPER=${wrapper_path}"
  echo "CARGO_INCREMENTAL=0"
  echo "SCCACHE_DIR=${cache_dir}"
  echo "SCCACHE_CACHE_SIZE=256M"
  echo "SCCACHE_LOCAL_RW_MODE=READ_WRITE"
  echo "SCCACHE_PATH=${sccache_path}"
} >>"${GITHUB_ENV}"
echo "${install_root}" >>"${GITHUB_PATH}"
echo "enabled=true" >>"${GITHUB_OUTPUT}"

echo "configured fixed-digest dependency-oriented sccache with a 256 MiB cap"
