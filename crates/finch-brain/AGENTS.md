# brain capsule: durable named Brains, schedules, events, and credentials

Supplements the root [`AGENTS.md`](../../CLAUDE.md), which still applies in full.

**Owns** `crates/finch-brain/src/`: `BrainStore` composition, the credential authority, remote Brain clients,
Brain-domain Cap'n Proto envelope/value translation, task records, name generation, and the
in-memory background-process task table (`background`,
issue #754: `BackgroundTaskManager` owns long-lived commands beyond the turn — bounded in count
and output retention, killed and reaped by stop or process shutdown, kill-not-adopt on restart).
Persistence and coordination internals live in nested
facades: [`journal`](journal/AGENTS.md), [`schedule`](schedule/AGENTS.md), [`run`](run/AGENTS.md),
[`attachment`](attachment/AGENTS.md), and [`projection`](projection/AGENTS.md). Named-Brain
portable effect delivery lives beside the reducible checkpoint as
`{root}/{name}/runtime/effects.jsonl` (`VmEffectDeliveryLog`, Brain-bound). Daemon, server, client,
and agent composition remain in the root application package; the domain-neutral IPC schema/protocol core lives in
`crates/finch-ipc`. Root application fixtures that compose Brain with server/client/CLI live in
`src/brain_application_tests.rs`; they consume only the normal Brain facade plus the
`test-support` seam.

**Interface:** [`INTERFACE.md`](INTERFACE.md) lists every exported item with its signature.
Child modules are private (`attachment`, `background`, `credential`, `journal`, `names`,
`projection`, `remote`, `run`, `schedule`, `store`, `tasks`), so the `pub use` list in
`src/lib.rs` is the whole public surface; `effect_audit_archive` stays private. Root callers use
`crate::brain::Item` through the compatibility re-export and direct dependents use
`finch_brain::Item`; neither may name `brain::store::`, `brain::journal::`,
`brain::schedule::`, `brain::run::`, `brain::attachment::`, `brain::projection::`,
`brain::tasks::`, `brain::remote::`, `brain::credential::`, `brain::names::`, or
`brain::background::`, or `brain::ipc_codec::`. The codec implementation remains private; its
existing application adapters are exposed as flat facade items. The feature-gated flat
`brain::test_support` facade exposes only fixtures needed by root application tests.

**Dependencies:** the extracted `finch-runtime`, `finch-vm`, `finch-programs`,
`finch-providers`, `finch-node`, and `finch-ipc` crates. Root server/client/CLI composition is
test-only and stays outside this capsule. Persistence, isolation, credential, and HTTP behavior are
not facade concerns: do not change storage layout, journaling, isolation proofs, credential
handling, or wire behavior in a facade or extraction commit.

Add public surface by re-exporting it from `src/lib.rs`, then regenerate `INTERFACE.md` with
`python3 scripts/generate_interfaces.py --write`.
