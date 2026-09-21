# Brain run agent contract

Supplements the root [agent rules](../../../../AGENTS.md) and the parent
[Brain contract](../../AGENTS.md). The [README](README.md) traces two caller workflows;
[`mod.rs`](mod.rs) is the nested facade and [`src/lib.rs`](../../lib.rs) is the external one.

**Owns:** run identity, kind and status; runner leases and handoffs; the closed run-transition
table; cancellation reservation records; and disconnect-terminalization intent files. Brain's
store owns durable event application, not this helper module.

**Facade and extension rule:** callers outside the crate use the flat `finch_brain::Item`
surface, not `run` paths. Keep run-state vocabulary here and publish a type through the crate
facade only when an external caller needs it. The transition and disconnect-intent helpers are
for Brain's store, not application callers. Do not recreate a generated API catalog.

**Dependencies:** `attachment` supplies the initiating attachment id; `journal` supplies
durable directory creation and sync for disconnect intents. This module must not depend on
`schedule` indexing, `BrainStore`, or root server/CLI code. `BrainStore` applies these rules
and persists run events; server policy and runner execution remain application-owned.

**Invariants and lifetimes:** a run, runner lease, and handoff have separate IDs and
lifetimes. Terminal run states cannot transition again. `Interrupted` is recoverable, not
terminal. The transition table lets a queued run become running, cancelled, or failed; the
store decides whether a runner environment is available before choosing one of those states.
Disconnect intents are synced to disk before a disconnected runner's run is terminalized and
cleared only after the state transition is durably recorded. Do not change transition legality,
lease identity, or intent-file layout in documentation-only work.

**Focused tests:** `./scripts/test_brains.sh cargo test -p finch-brain --lib run::` for the
transition table and intent files. Run supervised `store::` tests for cancel-vs-disconnect,
late completion, and restart of pending intents when lifecycle behavior changes. Run the
supervised server Brain-service tests when changing a caller-visible run contract.
