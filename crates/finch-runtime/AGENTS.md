# Finch runtime agent contract

Supplements the root [`AGENTS.md`](../../CLAUDE.md). Read the [runtime README](README.md) for
ownership and caller workflows; the flat `pub use` surface and facade-local signatures are in
[`src/lib.rs`](src/lib.rs). Methods of re-exported types remain defined in private child files.
Use `cargo doc -p finch-runtime --no-deps --open` to read their public signatures and method
contracts without opening those implementations. Root callers may use the
`crate::runtime` compatibility re-export; crate callers use `finch_runtime`.

## Dependencies and extension rules

- The runtime may depend on `finch-vm`, `finch-language`, `finch-programs`, `finch-memory`,
  `finch-ipc`, and `finch-tools-api`. It must not import Brain, CLI, server, theme, or root tool
  implementations. Brain and other applications depend on runtime, never the reverse.
- Keep runtime-defined contracts here: submission, capability and effect authority, checkpoint
  validation, portable effect delivery, and resume. The application owns named-Brain paths,
  client identity, editor/UI decisions, and transport connections. Bind the latter through the
  existing `RuntimeMcpClient`, `ArtifactProposalHost`, and effect sink seams at composition.
  Do not add a manager, service locator, or a trait around a pure helper.
- Add an externally needed item as a flat re-export from `src/lib.rs`; do not make child modules
  public. Confirm a real caller needs it and keep the associated type beside the invariant it
  represents. Do not duplicate the facade as a generated signature catalog.
- **Narrowed/removed dead public surface (#981 bounded pass, 2026-09-29).** A prior LSP audit
  (2026-09-20) listed `ProgramRun::with_identity`, `ProgramRuntime::capability_project_id`,
  `ProgramRuntime::has_mcp_client`, `EffectAuditReducer::active_for_run`, and
  `VmEffectDeliveryLog::cursor_for` as confirmed-dead (zero references, including this crate's own
  tests). A whole-workspace re-grep found this stale: all five had real internal callers predating
  the audit date (`ipc_codec.rs`'s `ProgramRun::new(..).with_identity(..)`, `effect_log.rs`'s own
  admission-quota check calling `active_for_run`, and round-trip assertions in `tests.rs` /
  `archive_store.rs`'s test module for the other three) — an LSP false negative even within the
  crate boundary the audit's compensating whole-workspace grep was meant to catch. These five were
  narrowed to `pub(crate)` instead of deleted. Confirmed actually dead and removed:
  `ProgramRuntime::{submit_as_with_typed_effect_sink, clear_task_output_root,
  deny_typed_execution_for_effect}`, `DeliveryConsumerIdentity::parse_wire_key`,
  `ProgramRun::effect_handle`, the `EffectAuditReducer::replay_fence_count` getter (the backing
  field stays, still written by the transition machinery), and the constant
  `MAX_EFFECT_AUDIT_JOURNAL_BYTES_PER_BRAIN` (referenced only by its own facade re-export, never
  an enforced bound unlike its sibling `MAX_ACTIVE_EFFECT_AUDIT*` limits). Narrowed to
  `pub(crate)` (real internal-only use confirmed): `ProgramRuntime::{cancel_typed_execution_for_effect,
  resume_typed_execution_for_effect, submit_as, submit_as_typed_only,
  submit_as_typed_only_with_typed_effect_sink, submit_with_deferred_host_effects,
  submit_with_typed_effect_sink, attach_memory, authority_state, bind_host_machine_root,
  bind_project_root, bind_whole_machine_root, cancel_typed_execution, clear_host_machine_root,
  clear_project_root, from_archive, from_archive_with_authority, pending_typed_execution_count,
  restore_authority_state, restore_capability_ledger, with_automation}`,
  `ProgramRuntimeArchiveStore::load_archive`, `ProgramRuntimeAuthorityStore::load_state`, and
  `RuntimeApplicationMessage::abi_version`. No behavior change; verified with a full workspace
  `cargo check --workspace --all-targets` and the crate's `--lib` test suite.

## Invariants and lifetimes

- A `ProgramRuntime` belongs to one execution session or named Brain. Its reducible checkpoint
  is separate from host-owned authority, filesystem roots, and live transport bindings. Restore
  and rebind those deliberately after restart; never infer authority from a checkpoint alone.
- A `(ProgramRun, effect sequence)` identifies one host effect. The application must resolve the
  retained continuation once; approval, cancellation, and replay must not resubmit source or
  redispatch a host effect. Delivery cursors are per Brain/client consumer.
- Runtime/Application ABI version 1 is a development seam, not a public compatibility promise.
  Bump finch-vm's `RUNTIME_APPLICATION_ABI_VERSION` and fail closed on incompatible frames; do
  not create a second journal or alternate encoding. Host effects remain subject to typed grants
  and the effect-audit fence.
- `AutomationBroker::execute` (`src/automation.rs`) must fail with a readable sentence, not a
  serialized `AutomationAvailability` blob, whenever `state != Available` — CLAUDE.md's GUI
  Accessibility invariant requires an actionable message, and for `PermissionRequired`
  specifically the sentence must literally contain "System Settings → Privacy & Security →
  Accessibility". `AutomationAvailability::unavailable_message()` is the single place that text
  is built; the structured `state`/`backend`/`operations` fields stay available separately for
  programmatic callers (e.g. the `availability` query) — only the *error text* changed from JSON
  to prose. `test_permission_required_message_names_the_system_settings_path`,
  `test_unavailable_message_is_prose_not_a_serialized_struct`,
  `test_disabled_broker_execute_error_is_prose_not_json` in `src/automation.rs` (issue #1382,
  partial — the semantic-targeting and `gui_inspect` element/label read gaps that issue also
  describes remain open).

## Focused proof

Run tests through the repository supervisor and Cargo slot:

```bash
.agents/skills/finch-backlog/scripts/with-cargo-slot ./scripts/test_brains.sh cargo test -p finch-runtime --lib
.agents/skills/finch-backlog/scripts/with-cargo-slot ./scripts/test_brains.sh cargo test -p finch-brain --lib
.agents/skills/finch-backlog/scripts/with-cargo-slot ./scripts/test_brains.sh cargo test -p finch --lib cli::repl_event::query_processor::tests::direct_wire_text_is_a_lisp_or_forth_submission_not_display_prose -- --exact
```

If the facade or host-effect behavior changes, also run the supervised workspace suite and the
relevant Brain/REPL integration tests. `scripts/check_subsystems.py` does not exist; use
`scripts/seam_cost.py` for dependency evidence and the crate boundary for enforced direction.
