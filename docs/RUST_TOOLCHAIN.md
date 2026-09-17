# Rust Toolchain Contract

Finch's tested and release toolchain is exactly Rust 1.98.0. The repository-owned
`rust-toolchain.toml` selects that compiler together with rustfmt, Clippy, and the standard-library
targets used by supported CI and release builds. Authoritative CI and release workflows must pin
the same version; `tests/toolchain_contract.sh` rejects drift or an unqualified moving `stable`.

This tested/release version is not an MSRV claim. `Cargo.toml` declares `rust-version = "1.98"`
(the pinned channel's major.minor), and `tests/toolchain_contract.sh` rejects drift between that
declared floor and `rust-toolchain.toml`. The field records the floor Finch actually pins and
tests; it is not evidence for the oldest compiler capable of building the current dependency
graph. Versions below 1.98.0 are unsupported until a dedicated lower-bound matrix supplies
evidence for a truthful MSRV.

The pin makes compiler and formatter selection reproducible. It does not claim reproducible
dependency resolution or byte-identical release artifacts; dependency policy belongs to #150.
Pull-request merge gates are Linux-only (`cargo fmt` and the Ubuntu test matrix). Apple Silicon
and Windows remain listed toolchain targets and future build surfaces; they are not required
on every PR. Trusted `main` still runs the macOS test job to warm the isolation cache.

The contract follows rustup's repository toolchain-file mechanism and Cargo's distinction between a
tested compiler and the optional `rust-version` package field:

- <https://rust-lang.github.io/rustup/overrides.html#the-toolchain-file>
- <https://doc.rust-lang.org/cargo/reference/rust-version.html>
- <https://blog.rust-lang.org/2026/08/20/Rust-1.98.0/>

## Formatting migration status

The clean checkout at `e8627479ada52997c7845d88027ffea1a6827706` is not formatted by Rust
1.98.0: a direct all-tracked-file audit emits a 52-file, 9,104-entry drift. Pinning alone therefore
cannot make `cargo fmt --all -- --check` pass.

`RUSTFMT_1_98_MIGRATION_MANIFEST.txt` records that exact 52-file footprint, and
`tests/apply_issue_191_format_migration.sh --apply` refuses to proceed from a dirty checkout or if
the resulting footprint differs. The one-time rewrite must be committed as an isolated mechanical
commit containing only the Rust 1.98.0 formatter output.

The audited manifest SHA-256 is
`a9e4cc6250f6fcfe89fc4c5a45f2f871eb03753838c42026d0faae0d1cdddea4`.
The sorted 304-file Rust blob preimage SHA-256 is
`db2911ff9c8e252898363757cfadd2bde3350f2c9a10178914fdc2b8e330d5fa`; after any rebase that
changes it, the footprint must be re-audited and both guards updated before formatting.
