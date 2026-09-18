# brain capsule: durable named Brains, schedules, events, and credentials

Supplements the root [`AGENTS.md`](../../CLAUDE.md), which still applies in full.

**Owns** `src/brain/`: `BrainStore` composition, the credential authority, remote brain clients,
task records, name generation, and the in-memory background-process task table (`background`,
issue #754: `BackgroundTaskManager` owns long-lived commands beyond the turn — bounded in count
and output retention, killed and reaped by stop or process shutdown, kill-not-adopt on restart).
Persistence and coordination internals live in nested
facades: [`journal`](journal/AGENTS.md), [`schedule`](schedule/AGENTS.md), [`run`](run/AGENTS.md),
[`attachment`](attachment/AGENTS.md), and [`projection`](projection/AGENTS.md). Named-Brain
portable effect delivery lives beside the reducible checkpoint as
`{root}/{name}/runtime/effects.jsonl` (`VmEffectDeliveryLog`, Brain-bound). The daemon
subtree is owned from here but implemented in `src/daemon` (its own capsule). Server, IPC,
client, and agent composition live outside this subtree.

**Interface:** [`INTERFACE.md`](INTERFACE.md) lists every exported item with its signature.
Child modules are private (`attachment`, `background`, `credential`, `journal`, `names`,
`projection`, `remote`, `run`, `schedule`, `store`, `tasks`), so the `pub use` list in
`src/brain/mod.rs` is
the whole public surface; `effect_audit_archive` stays `pub(crate)`. Callers outside this
directory use `crate::brain::Item`; they must not name `brain::store::`, `brain::journal::`,
`brain::schedule::`, `brain::run::`, `brain::attachment::`, `brain::projection::`,
`brain::tasks::`, `brain::remote::`, `brain::credential::`, `brain::names::`, or
`brain::background::`.

**Dependencies:** `runtime` (layer 2; the one allowed incoming direction), `models`, `tools`,
`claude`. Persistence, isolation, credential, and HTTP behavior are not facade concerns: do not
change storage layout, journaling, isolation proofs, credential handling, or wire behavior in a
facade commit. Do not extract `finch-brain`.

Add public surface by re-exporting it from `mod.rs`, then regenerate `INTERFACE.md` with
`python3 scripts/generate_interfaces.py --write`.
