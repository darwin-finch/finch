# Vendored internal-russh-forked-ssh-key 0.6.18+upstream-0.6.7

This directory contains the exact source payload published as
`internal-russh-forked-ssh-key` 0.6.18+upstream-0.6.7, plus Finch metadata and
the recorded minimal RSA-removal patch.

- Crate: `https://static.crates.io/crates/internal-russh-forked-ssh-key/internal-russh-forked-ssh-key-0.6.18+upstream-0.6.7.crate`
- Crate SHA-256: `25f8a978272e3cbdf4768f7363eb1c8e1e6ba63c52a3ed05e29e222da4aec7cb`
- Upstream repository: `https://github.com/Eugeny/RustCrypto-SSH`
- Upstream branch: `russh-current-0.6.7` (the repository publishes no tags)
- Upstream commit: `07f295e8cd0f8c9415bf8757e47aa3a80a78a132`
- Published source path at that commit: `ssh-key`
- Apache-2.0 license SHA-256: `a9040321c3712d8fd0b09cf52b17445de04a23a10165049ae187cd39e5c86be5`
- MIT license SHA-256: `33f702959c0ea91c08b21b65cf1f08b6c122ec9e6db0b5db784a7b367d942330`

The dual Apache-2.0 OR MIT license files are included in the published crate.
`FINCH-VENDOR.md` and any `FINCH-*.patch` file are Finch metadata and are not
part of the upstream payload. CI reconstructs the checked-in tree from the
checksummed crate plus the explicit patch.

The published `Cargo.toml.orig` is retained byte-for-byte as
`Cargo.toml.upstream`; the repository hygiene policy reserves `.orig` for
transient files. The patch manifest records this packaging-only rename.

The only source delta is `FINCH-RSA-REMOVAL.patch`. It removes the optional RSA
dependency and RSA features plus code gated exclusively on those features.
Ungated RSA wire-format representations and parsing remain byte-for-byte
upstream so other algorithms and protocol behavior are unchanged.

The import adds approximately 820 KiB and 113 files. Finch publication remains
disabled while either security-patched path dependency is required.
