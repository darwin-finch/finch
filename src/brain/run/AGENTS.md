# brain/run capsule: run lifecycle, leases, and terminalization

Supplements the root [`AGENTS.md`](../../../CLAUDE.md) and the parent
[`brain` capsule](../AGENTS.md), which still apply in full.

**Owns** `src/brain/run/`: `BrainRun` status and kind, runner leases and
handoffs, the closed run-transition table, cancellation reservation records,
and disconnect-terminalization intent files. Exact-once terminal state is an
invariant of this facade.

**Interface:** child modules are private, so the `pub use` list in `mod.rs` is the
whole public surface — read it directly for exact signatures. Callers outside `src/brain` use
`crate::brain::Item`.

**Dependencies:** `attachment` for the initiating attachment id; `journal` for
durable directory creation used by disconnect intents. Do not depend on
`schedule` indexing or `BrainStore`. Do not change transition legality, lease
identity, or terminalization file layout in a facade-only commit.

**Focused tests:** `./scripts/test_brains.sh cargo test --lib -- brain::run::`.
Parent `brain::store::` tests cover cancel-vs-disconnect races, late completion,
and restart of pending disconnect intents.
