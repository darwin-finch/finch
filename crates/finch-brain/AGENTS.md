# Finch Brain agent contract

Supplements the root [agent rules](../../AGENTS.md). Read the [Brain README](README.md) for
ownership and caller workflows. The flat public facade is [`src/lib.rs`](src/lib.rs); use
`cargo doc -p finch-brain --no-deps --open` for method signatures on re-exported types. Root
callers use the `crate::brain` compatibility path, while direct dependents use `finch_brain`.

## Dependencies and extension rules

- Brain may depend on `finch-runtime`, `finch-vm`, `finch-programs`, `finch-providers`,
  `finch-node`, and `finch-ipc`. Root server, CLI, daemon, and client implementations depend on
  Brain, never the reverse. Their HTTP routes, dialogs, and process lifecycles stay at the
  application composition root.
- Keep Brain identity, event and run records, schedule state, attachment authority, credentials,
  and Brain-specific remote-client semantics here. The IPC crate owns domain-neutral framing;
  runtime owns typed execution and effect delivery semantics. Do not move these contracts into a
  caller just to avoid a dependency.
- Export a new external capability deliberately as a flat `pub use` in `src/lib.rs`; child modules
  stay private. Keep the feature-gated `test_support` surface for application integration tests,
  not production shortcuts. Do not regenerate or hand-maintain an API catalog.

## Invariants and lifetimes

- `BrainStore` is the durable authority for a named Brain. A daemon restart reconstructs its
  snapshot, active runs, schedules, and attachments from the Brain journal; a console's
  `AttachedBrainClient` is a projection and transport connection, not another source of truth.
- A run, runner lease, attachment, and connection have distinct identities and lifetimes.
  Detaching a connection must not silently grant runner authority. The server explicitly rejects
  attaching with `AttachmentRole::Runner`; runner authority goes through a lease.
- Keep checkpoint, effect-delivery log, and Brain journal roles separate. Preserve idempotent
  receipt and terminalization behavior through disconnect, retry, and restart. Storage layout,
  credential verification, and wire compatibility are not facade-cleanup opportunities.
- **`BrainEventKind::ContextCompacted` (schema v16, #1265) is durable journal scaffolding with no
  producer yet** — a marker that local-model conversation history up through `covers_through` was
  compacted into a `ContextCompactionTier` (`Verbatim` / `LightlyCompressed` / `Gist`; provisional
  pending #1266, the sibling tiering-logic issue, which owns the canonical scheme). It is
  audit-only: `BrainStore::apply` performs no snapshot-visible projection from it, matching how
  `Prompt`/`ToolCall`/`Result`/etc. are handled. It is deliberately **not** named "checkpoint" —
  that term stays reserved for `RuntimeCommitted`'s restart-recovery snapshot
  (`checkpoint_sha256`), a different concept. Nothing appends this event yet; the compaction
  algorithm (#1266) and the application-layer trigger that actually produces it during real
  conversation flow (#1269) are separate, later changes. Covered by
  `test_context_compacted_event_survives_store_restart_replay` and
  `test_pre_context_compacted_journal_still_replays_after_schema_bump` in
  `src/store/tests.rs`; `test_context_compacted_round_trip_keeps_tier_digest_and_provider` and
  `test_context_compacted_event_round_trips_through_real_journal_append_and_read` in
  `src/journal/tests.rs`; and the Cap'n Proto wire round trip in
  `every_current_brain_event_round_trips_through_capnp` in `src/ipc_codec.rs`.

- **Caller naming convention, not a Brain crate concept:** `spawn_task` (`src/tools/implementations/spawn.rs` in the root crate) creates one named Brain per subagent run through this same `BrainStore`, named `sub-<generate()>` and archived via the existing `archive()` call immediately on completion unless the caller opts to keep it live. This crate does not know or enforce the `sub-` prefix or the archive-on-completion policy — both live entirely in the caller — but a future change here that alters `generate()`'s output shape or `archive()`'s semantics affects that caller too. See `src/tools/EXECUTION.md` for the caller-side policy.

Nested persistence contracts: [attachment](src/attachment/AGENTS.md),
[journal](src/journal/AGENTS.md), [projection](src/projection/AGENTS.md),
[run](src/run/AGENTS.md), and [schedule](src/schedule/AGENTS.md).

## Focused proof

Run tests through the repository supervisor and Cargo slot:

```bash
.agents/skills/finch-backlog/scripts/with-cargo-slot ./scripts/test_brains.sh cargo test -p finch-brain --lib
.agents/skills/finch-backlog/scripts/with-cargo-slot ./scripts/test_brains.sh cargo test -p finch --lib server::brain_service::
.agents/skills/finch-backlog/scripts/with-cargo-slot ./scripts/test_brains.sh cargo test -p finch --lib cli::repl_event::event_loop::
```

For persistence or authority changes, also run the supervised Brain integration inventory. The
requested `scripts/check_subsystems.py` does not exist; inspect `scripts/seam_cost.py` output
for dependency evidence instead of claiming a nonexistent check passed.
