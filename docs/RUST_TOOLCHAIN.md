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

The clean checkout at `aa050271a919596a712cbdc13e0adb55be79d9bd` is not formatted by any Rust
release from 1.88.0 through 1.98.0: a direct all-tracked-file audit emits the same 57-file,
2,858-entry drift for every release. Pinning alone therefore cannot make
`cargo fmt --all -- --check` pass.

The one-time Rust 1.98.0 rewrite is intentionally deferred while active branches edit overlapping
Rust files. `RUSTFMT_1_98_MIGRATION_MANIFEST.txt` records the exact 57-file footprint, and
`tests/apply_issue_191_format_migration.sh --apply` refuses to proceed from a dirty checkout or if
the resulting footprint differs. After the active Rust frontier merges, run that script and commit
its source-only result as an isolated mechanical commit. Until then, the formatting contract job is
expected to fail and the issue #191 pull request must remain blocked.

The audited manifest SHA-256 is
`16ea6402375aeffd18c972dbcd33d7777065d8f8be533fb8a1db368b6ebdbd13`.
The sorted 296-file Rust blob preimage SHA-256 is
`c014fb492365afcfc3ea3a853c9e7c996be19f82f4bd7b1e054e1263e5de2b3f`; after any rebase that
changes it, the footprint must be re-audited and both guards updated before formatting.
