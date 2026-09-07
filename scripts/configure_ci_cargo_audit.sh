#!/usr/bin/env bash
# Install RustSec's reviewed cargo-audit binary without compiling it in CI.

set -euo pipefail

if [[ -z "${RUNNER_TEMP:-}" || -z "${GITHUB_PATH:-}" ]]; then
  echo "RUNNER_TEMP and GITHUB_PATH are required for cargo-audit setup" >&2
  exit 1
fi
if [[ "${RUNNER_OS:-}" != "Linux" || "${RUNNER_ARCH:-}" != "X64" ]]; then
  echo "no reviewed cargo-audit binary for ${RUNNER_OS:-unset}/${RUNNER_ARCH:-unset}" >&2
  exit 1
fi

version=0.22.2
platform=x86_64-unknown-linux-gnu
root=cargo-audit-x86_64-unknown-linux-gnu-v0.22.2
asset=cargo-audit-x86_64-unknown-linux-gnu-v0.22.2.tgz
archive="${RUNNER_TEMP}/${asset}"
install_root="${RUNNER_TEMP}/finch-cargo-audit-${version}"
member="${root}/cargo-audit"
expected_sha256=ab28a1bdb54db4d5d8ad5981cf1f959410370b3d28250dbd35f6a44248620e39
url="https://github.com/rustsec/rustsec/releases/download/cargo-audit/v${version}/${asset}"

curl --fail --location --retry 3 --max-filesize 8388608 --proto '=https' --tlsv1.2 --output "${archive}" "${url}"
printf '%s  %s\n' "${expected_sha256}" "${archive}" | shasum -a 256 --check --status

archive_members=$(tar -tzf "${archive}")
expected_members=$(printf '%s\n' \
  "${root}/" \
  "${root}/LICENSE-APACHE" \
  "${root}/CHANGELOG.md" \
  "${member}" \
  "${root}/LICENSE-MIT" \
  "${root}/README.md")
if [[ "${archive_members}" != "${expected_members}" ]]; then
  echo "verified cargo-audit archive has unexpected members" >&2
  exit 1
fi
if ! tar -tvzf "${archive}" | awk '$1 !~ /^[d-]/ { exit 1 }'; then
  echo "verified cargo-audit archive contains a link or special file" >&2
  exit 1
fi

mkdir -p "${install_root}"
audit_path="${install_root}/cargo-audit"
tar -xOzf "${archive}" "${member}" >"${audit_path}"
chmod 0755 "${audit_path}"
echo "${install_root}" >>"${GITHUB_PATH}"

echo "configured fixed-digest cargo-audit ${version}"
