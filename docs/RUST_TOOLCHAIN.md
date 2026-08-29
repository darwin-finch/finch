# Rust Toolchain Contract

Finch's tested and release toolchain is exactly Rust 1.98.0. The repository-owned
`rust-toolchain.toml` selects that compiler together with rustfmt, Clippy, and the standard-library
targets used by supported CI and release builds. Authoritative CI and release workflows must pin
the same version; `tests/toolchain_contract.sh` rejects drift or an unqualified moving `stable`.

This tested/release version is not an MSRV claim. Finch has not established or continuously tested
the oldest compiler capable of building the current dependency graph, so `Cargo.toml` intentionally
does not declare `rust-version`. Versions below 1.98.0 are unsupported until a dedicated lower-bound
matrix supplies evidence for a truthful MSRV.

The pin makes compiler and formatter selection reproducible. It does not claim reproducible
dependency resolution or byte-identical release artifacts; dependency policy belongs to #150.
The full platform/feature matrix remains a required merge gate for this contract.

The contract follows rustup's repository toolchain-file mechanism and Cargo's distinction between a
tested compiler and the optional `rust-version` package field:

- <https://rust-lang.github.io/rustup/overrides.html#the-toolchain-file>
- <https://doc.rust-lang.org/cargo/reference/rust-version.html>
- <https://blog.rust-lang.org/2026/08/20/Rust-1.98.0/>

## Formatting migration status

The clean checkout at `c40c325f44d83deb9227b65f8ab3cc77301eb92e` is not formatted by Rust
1.98.0: a direct all-tracked-file audit emits a 51-file, 9,100-entry drift. Pinning alone therefore
cannot make `cargo fmt --all -- --check` pass.

`RUSTFMT_1_98_MIGRATION_MANIFEST.txt` records that exact 51-file footprint, and
`tests/apply_issue_191_format_migration.sh --apply` refuses to proceed from a dirty checkout or if
the resulting footprint differs. The one-time rewrite must be committed as an isolated mechanical
commit containing only the Rust 1.98.0 formatter output.

The audited manifest SHA-256 is
`bb74c92f4169628def17988c68c45b290ce3a3e366b41ca04379ad3ce18f623f`.
The sorted 299-file Rust blob preimage SHA-256 is
`17d2467f2cba26c566c924a5921beb66f90bfea25bc524915be0d10a97490925`; after any rebase that
changes it, the footprint must be re-audited and both guards updated before formatting.
