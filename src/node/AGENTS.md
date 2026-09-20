# Node compatibility facade

Supplements the root [`AGENTS.md`](../../CLAUDE.md), which still applies in full.

`src/node` preserves the root-package `finch::node` path for application callers. The authoritative
implementation, ownership rules, tests, and public API live in
[`crates/finch-node`](../../crates/finch-node/AGENTS.md). This facade contains no implementation and
must remain a flat `pub use` surface; callers must not gain child-module paths here.

Regenerate [`INTERFACE.md`](INTERFACE.md) with
`python3 scripts/generate_interfaces.py --write` whenever the compatibility surface changes.
