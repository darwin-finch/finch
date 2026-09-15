# brain capsule: durable named Brains, schedules, events, and credentials

Supplements the root [`AGENTS.md`](../../CLAUDE.md), which still applies in full.

**Owns** `src/brain/`: `BrainStore` persistence, runs and their statuses, schedules and delivery
policies, events and attachments, the credential authority, remote brain clients, task records,
and name generation. The daemon subtree is owned from here but implemented in `src/daemon`
(its own capsule). Server, IPC, client, and agent composition live outside this subtree.

**Interface:** [`INTERFACE.md`](INTERFACE.md) lists every exported item with its signature.
Child modules are private (`credential`, `names`, `remote`, `store`, `tasks`), so the `pub use`
list in `src/brain/mod.rs` is the whole public surface; `effect_audit_archive` stays
`pub(crate)`. Callers outside this directory use `crate::brain::Item`; they must not name
`brain::store::`, `brain::tasks::`, `brain::remote::`, `brain::credential::`, or `brain::names::`.

**Dependencies:** `runtime` (layer 2; the one allowed incoming direction), `models`, `tools`,
`claude`. Persistence, isolation, credential, and HTTP behavior are not facade concerns: do not
change storage layout, journaling, isolation proofs, credential handling, or wire behavior in a
facade commit. Do not extract `finch-brain`.

Add public surface by re-exporting it from `mod.rs`, then regenerate `INTERFACE.md` with
`python3 scripts/generate_interfaces.py --write`.
