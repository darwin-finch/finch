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
- **A type that appears in a `pub` method signature on an exported type must itself be re-exported
  from `src/lib.rs`, or the signature leaks an unnameable type to callers.** `BrainMutationHandle`
  (`src/remote.rs`) is the caller-persisted idempotency handle `RemoteBrainClient`'s
  `prepare_*_mutation`/`*_with_handle` retry pairs (`prepare_push_mutation`, `push_with_handle`,
  and the runner-handoff/cancel-run/schedule equivalents) return and accept; those methods are real
  production surface (`src/brain_application_tests.rs` in the root crate calls them across the
  `crate::brain` alias), but until #992's audit the type was only `pub` inside the private `remote`
  module, so an external caller could receive and hold a value of it (via type inference) but never
  spell its name in a field, signature, or generic bound. Fixed by adding `BrainMutationHandle` to
  the `pub use remote::{...}` list rather than changing the mutation-handle methods' signatures.

## Invariants and lifetimes

- `BrainStore` is the durable authority for a named Brain. A daemon restart reconstructs its
  snapshot, active runs, schedules, and attachments from the Brain journal; a console's
  `AttachedBrainClient` is a projection and transport connection, not another source of truth.
- **A Brain's canonical `environment.workspace` is per-Brain, recorded once at creation, not a
  single daemon-wide default (#1381).** `BrainStore.environment` (set once from
  `std::env::current_dir()` at store construction, i.e. the daemon's own launch-time cwd) is only
  the *fallback* for a Brain whose own metadata carries no `workspace` — legacy Brains created
  before this field existed, and any caller that never supplies one. `BrainStore::snapshot_for_client`
  is the entry point that can set it: when a not-yet-existing Brain's metadata is first created
  (`ensure_loaded_with_workspace` -> `load_or_create_metadata`), the caller's `requesting_workspace`
  becomes that Brain's own, permanently recorded `BrainMetadata.workspace` (canonicalized, same as
  the store default). `BrainStore::snapshot` is a thin `snapshot_for_client(name, None)` wrapper for
  every caller that has no client-specific workspace to offer. Once set, a Brain's workspace is
  never overwritten by a later caller's cwd — canonical workspace is fixed at creation so
  workspace-mismatch detection (`verify_frontend_environment`,
  `src/cli/repl_event/brain_handler.rs` in the root crate) stays meaningful. The local Cap'n Proto
  `BrainService.snapshot` RPC (`crates/finch-ipc/schema/finch_ipc.capnp`) carries the requesting
  client's own cwd as `requestingWorkspace` for exactly this reason — a brand-new Brain created for
  one client invocation must never record a *different* daemon-wide default and then immediately
  read back as workspace-mismatched against the very client that just created it. Covered by
  `test_brain_created_through_real_ipc_records_creating_clients_cwd_not_daemon_launch_cwd` in
  `src/server/ipc/tests.rs` (root crate), which drives a real `IpcClient` against a real
  `BrainRpcService` over an actual Cap'n Proto `UnixStream` pair.
- **`BrainStore::snapshot_for_client` is not the only first-touch Brain-creation path — every one
  of them must independently carry the requesting client's cwd, or whichever runs first for a
  given Brain name wins the race and the others are silently moot (#1381, found live after the
  fix above shipped and still reproduced).** `BrainStore::set_provider_selection_for_client` is
  the second: the root crate's `EventLoop::run` startup sequence calls `hydrate_brain_selection`
  (which can persist an inherited default provider selection for a genuinely new Brain, over the
  HTTP `PUT /v1/brains/named/:name/selection` route) *before* `register_home_brain`'s
  already-fixed Cap'n Proto `snapshot` call ever runs. Brain-metadata creation is first-write-wins
  (`load_or_create_metadata_unlocked` never revisits an existing file), so the earlier,
  unfixed-at-the-time write baked in the daemon's own default workspace, and the later, correctly
  workspace-aware `snapshot_for_client` call found the Brain "already existed" and never corrected
  it — exactly the original bug, reintroduced through a sibling path the first fix never touched.
  `set_provider_selection` is now a thin `set_provider_selection_for_client(name, selection, None)`
  wrapper, mirroring `snapshot`/`snapshot_for_client`'s own shape; the root crate's HTTP layer
  carries the caller's cwd as the `x-finch-workspace` request header (not a field on
  `BrainProviderSelection` itself, which is also the persisted `metadata.selection` value and
  should not carry an unrelated concern). Covered by
  `set_provider_selection_for_client_records_the_requesting_workspace_on_first_creation`,
  `set_provider_selection_for_client_never_overwrites_an_existing_brains_workspace`, and
  `either_creation_path_running_first_records_the_requesting_workspace` (this file's own
  `store/tests.rs`), plus `requesting_workspace_header_parses_present_value_and_treats_absent_or_empty_as_none`
  in `src/server/handlers/handler_tests.rs` (root crate). Before trusting any future "fixed" claim
  about this invariant, grep for every caller of `load_or_create_metadata`/
  `load_or_create_metadata_unlocked` and confirm each one that can be a Brain's *first* touch has a
  `requesting_workspace`-aware entry point reachable from wherever the real client's cwd is known —
  this bug shipped twice because the first fix addressed one call site instead of the invariant.
- **A caller comparing `BrainEnvironment`s to gate runner-lease acquisition, handoff acceptance,
  or submission readiness must use `BrainEnvironment::same_host_identity` (machine + generation),
  never `==` (whole-struct, includes `workspace`).** Before the #1381 per-Brain-workspace fix
  above, every Brain's `workspace` was always identical to `BrainStore`'s own daemon-wide default,
  so `BrainLifecycleService::acquire_runner`/`accept_runner_handoff`
  (`src/server/brain_service.rs` in the root crate) and `ensure_named_brain_store_environment`
  (`src/server/handlers.rs`, the gate `named_brain_runner_is_ready` runs before *every* executable
  submission) could compare the full struct and it was harmless — the comparison was dead code that
  always passed. Once `workspace` genuinely varies per Brain, that same whole-struct comparison
  would reject the ordinary case (a runner supplying its own Brain's real, client-derived
  environment, which legitimately differs from the daemon's unrelated default workspace) and break
  runner registration and prompt submission for exactly the sessions #1381 was reported from.
  Covered by `acquire_runner_succeeds_when_the_brains_own_workspace_differs_from_the_daemon_default`
  in `src/server/brain_service.rs` and
  `prompt_submission_reaches_running_when_the_brains_own_workspace_differs_from_the_daemon_default`
  in `src/server/handlers/handler_tests.rs` (root crate); both fail if either call site regresses
  to `==`.
- A run, runner lease, attachment, and connection have distinct identities and lifetimes.
  Detaching a connection must not silently grant runner authority. The server explicitly rejects
  attaching with `AttachmentRole::Runner`; runner authority goes through a lease.
- Keep checkpoint, effect-delivery log, and Brain journal roles separate. Preserve idempotent
  receipt and terminalization behavior through disconnect, retry, and restart. Storage layout,
  credential verification, and wire compatibility are not facade-cleanup opportunities.
- **An unchanged runner checkpoint is an idempotent no-op, not durable progress (#1443).**
  `BrainStore::commit_runner_runtime_inner` compares both the returned runtime revision and the
  canonical encoded checkpoint hash with the current durable checkpoint. A lower revision remains
  stale; an equal revision with different bytes is a conflict; only an exact revision-and-hash
  match returns `Ok(None)` without appending `RuntimeCommitted`. Appending the duplicate would
  increment `runtime_commit_count` during projection and invent a later durable revision on replay.
  `named_brain_commits_a_validated_frontend_runner_checkpoint` and
  `named_brain_rejects_conflicting_runner_checkpoint_at_durable_revision` in
  `src/store/tests.rs` pin the store contract; the root server tests
  `denied_tool_turn_with_unchanged_checkpoint_completes_once_across_restart` and
  `conflicting_equal_revision_checkpoint_keeps_exact_error_across_restart` exercise terminal
  publication and restart at the production dispatch boundary.
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
- **`BrainStore::sweep_unused` is the only automatic reclaimer of a Brain nobody ever used, and classification must be strictly read-only (#411).** `generate()` mints a name on every launch, so `AgentServer::new` calls the sweep once before either listener starts. Every candidate must be classified directly from existing `metadata.json` and `events.jsonl` bytes: never call `ensure_loaded`, create `initialization.json` or effect-audit state, repair a journal, or leave a protected Brain resident. Missing, corrupt, torn, unsupported, or identity/sequence-inconsistent durable state fails closed. The deletion predicate remains the lifecycle predicate: retain every Brain with a substantive event, persisted provider selection, live attachment (including an already-resident pending connection), or runner lease. The state lock is held from the resident/live check through removal so an in-memory participant cannot race deletion, but an absent candidate is never hydrated merely to inspect it. The 24-hour minimum age, startup-only cadence, delete-rather-than-archive behavior, and transition-only log line remain unchanged. Store regressions in `src/store/tests.rs` pin successful deletion across restart; substantive history, provider selection, pending attachment, and runner-lease protection; missing/corrupt metadata and directory-symlink no-mutation; and zero-resident/byte-identical inspection after dropping and reopening the store. `production_constructor_sweeps_unused_brains_before_anything_can_touch_them` in root `src/server/mod.rs` pins the same no-hydration/no-mutation guarantee through the real `AgentServer::new` boundary.

Nested persistence contracts: [attachment](src/attachment/AGENTS.md),
[journal](src/journal/AGENTS.md), [projection](src/projection/AGENTS.md),
[run](src/run/AGENTS.md), and [schedule](src/schedule/AGENTS.md).

## Focused proof

Run tests through the repository supervisor and Cargo slot:

```bash
scripts/factory/with-cargo-slot ./scripts/test_brains.sh cargo test -p finch-brain --lib
scripts/factory/with-cargo-slot ./scripts/test_brains.sh cargo test -p finch --lib server::brain_service::
scripts/factory/with-cargo-slot ./scripts/test_brains.sh cargo test -p finch --lib cli::repl_event::event_loop::
```

For persistence or authority changes, also run the supervised Brain integration inventory. The
requested `scripts/check_subsystems.py` does not exist; inspect `scripts/seam_cost.py` output
for dependency evidence instead of claiming a nonexistent check passed.
