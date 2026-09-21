# Node compatibility facade

Supplements the root [`AGENTS.md`](../../CLAUDE.md), which still applies in full.

`src/node` preserves the root-package `finch::node` path for application callers. The authoritative
implementation, ownership rules, tests, and public API live in
[`crates/finch-node`](../../crates/finch-node/AGENTS.md). This facade contains no implementation and
must remain a flat `pub use` surface; callers must not gain child-module paths here.

The [README](README.md) traces CLI and server workflows; [`mod.rs`](mod.rs) is the exact
compatibility surface. Do not recreate a generated signature catalog. Add an export here only
when a root-package caller still needs the legacy path; new subsystem code should depend on
`finch-node` directly. Identity persistence, TLS pairing, and test-state isolation invariants
belong to the crate, not to this alias.

For focused proof, run `./scripts/test_brains.sh cargo test -p finch-node --lib` and
`./scripts/test_brains.sh cargo test -p finch --lib server::handlers::`. A change to this facade
also needs an all-target compile because the binary and server use its types.
