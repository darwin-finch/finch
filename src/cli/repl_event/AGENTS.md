# repl_event capsule: the interactive REPL event loop

Supplements the root [`AGENTS.md`](../../../CLAUDE.md), which still applies in full.

**What this is.** The machinery behind the interactive REPL. A Tokio `select!` in
`EventLoop::run` reads user input, provider output, and Brain traffic, turns each into a
`ReplEvent`, and dispatches it. The selected generator is called by `query_processor`, and
tool execution is coordinated here through [`crate::tools::ToolLoop`] (shared with the
scheduler): stream events are observed, then admitted at most once.
`finch-conversation` owns the committed provider history and staged tool-round ledger; the
event loop owns the admission, checkpoint, and continuation decisions around those transitions.

**Boundary and dependencies.** The [README](README.md) traces construction by `Repl` and
event projection by the MemTree console. `mod.rs` is the facade; new callers should use its
flat exports rather than reaching through child modules. All children are private: the REPL
obtains its selection policy, construction parts, provider-profile resolver, and committed-set
handles through named facade exports. The application injects provider,
tool, Brain, and UI dependencies; this module may coordinate them but must not become their
owner. No lower-level crate should depend on the REPL event loop.

**Named-Brain provider wire carries application authority, not ambient owner authority.**
`QueryMetadata::grant_ceiling` is bound from the daemon-issued runner request before provider
dispatch and is passed unchanged through direct, deterministic-wrap, and repaired wire
submission and through the tool-execution coordinator into provider-native `submit_program`.
Local owner queries leave it unset so their explicit reusable grants retain their existing
meaning. The runtime captures a present ceiling with any suspended continuation, so a late
ambient grant cannot expand a resumed named-Brain program.

**Where the code lives.** `event_loop.rs` holds the `EventLoop` struct, `new`, and `run`. The
handlers live beside it, grouped by what they handle, and are `pub(super)` so only the loop calls
them:

| File | Handles |
|------|---------|
| `event_loop/dispatch.rs` | the `match event` over every `ReplEvent` variant |
| `event_loop/input.rs` | a submitted line: commands, typed programs, or a query |
| `changeset.rs` | consecutive write/edit/patch grouping and aggregate diff preview |
| `event_loop/tools.rs` | tool results, tool and VM approval prompts |
| `event_loop/brain.rs` | remote and named Brain traffic, invitations, reconnection |
| `event_loop/plan.rs` | plan mode, plan tasks, poset confirmation |
| `event_loop/commands.rs` | slash-style commands: provider switch, feedback, demos |
| `runner_recovery.rs` | leftover-daemon / lease / environment mismatch labels |

**Adding an event.** Add the variant to `ReplEvent` in `events.rs`, then handle it in
`dispatch.rs`. The compiler will tell you the match is non-exhaustive — that is the point of the
enum, so do not add a catch-all arm. Keep the arm short: if handling takes more than a few lines,
put a `pub(super)` method in whichever file above it belongs to and call it.

**IPC recovery is header/status, not transcript.** Peer disconnect, home event-watch loss, and
runner reconnect attempts update `StatusBar` (`SessionLabel`) through
`EventLoop::project_ipc_recovery_header`. They must not call `output_manager.write_info` — that
commits sticky conversation rows between program source and output (#819). Leftover-daemon /
environment mismatch at startup still uses `apply_home_runner_startup` (header plus the detailed
startup TUI line, #794).

**A self-correcting wire-protocol repair round hides its raw diagnostic from the transcript
(#1383).** `execute_wire_with_single_repair` (`query_processor.rs`) rejects a provider response
that is not a valid `ProgramSubmission`, then — when the rejection is repairable — retries once
with a corrective prompt before showing anything to the user. The raw compiler-style diagnostic
(`file:line`, an `error[E-...]` code, a `phase:` field) is internal detail with nothing a
non-technical user can act on; it goes to `tracing::debug!` only, never
`output_unit.append_response`. If the repair round then fails for any reason (cancelled while
waiting on it, the corrective generation itself errors or returns no usable program, or the
repaired program is rejected too), the transcript gets the plain-language
`WIRE_REPAIR_FAILED_MESSAGE` fallback instead of the raw diagnostic — never nothing, and never the
compiler-style shape. Only a first-pass rejection classified as non-repairable at all (no retry
ever attempted) still shows its raw diagnostic, since there is no eventual corrected answer to
prefer showing instead. `test_repairable_rejection_diagnostic_reaches_debug_log_not_transcript`,
`failed_wire_turn_stays_expanded_program_output`,
`named_brain_effect_audit_cancel_before_repair_never_invokes_provider`, and
`named_brain_effect_audit_cancel_drops_inflight_repair_without_continuation` in
`query_processor.rs` cover the successful-repair, cancelled-before-repair, and cancelled-during-repair
cases.

**A deterministic raw-prose fallback must caption unsupported completed filesystem mutation
claims.** `claims_tool_grounded_fact` (`query_processor.rs`) recognizes a narrow completed-claim
grammar for create/write/edit/update/patch/move/rename/delete assertions whose direct target is a
path, file, or directory, optionally after the bounded standalone acknowledgement `Done.`/`Done!`.
When that query has no completed tool call, the existing visible unverified-tool caveat is retained
in the live output and canonical `StreamingComplete` response; prospective instructions, requests,
plans, refusals, examples, hyphenated adjectives such as `file-based`, and creative prose remain
uncaveated. `named_brain_raw_prose_file_creation_claim_is_caveated_without_creating_the_file`,
`completed_filesystem_mutation_claims_match_only_asserted_effects`,
`completed_filesystem_mutation_claim_after_done_line_is_detected`,
`hyphenated_file_adjective_is_not_a_filesystem_target`, and
`unattempted_prose_claiming_file_creation_gets_no_caveat_after_completed_write` in
`query_processor.rs` cover the production boundary and controls (#1477).

**Snapshot replay reconstructs say-turn cards (#970).** The say ViewModel is produced only by
the live paths, so `project_remote_brain_snapshot_runs` also runs
`reconstruct_replayed_say_turn_cards`: a freshly replayed Interactive run whose journal pattern is
one typed `Program` (the run-correlated provider event, or the run-unaffiliated event at
`run.request_seq` for typed programs) plus a successful `Result`, with no tool or approval events,
gets `begin_say_turn` on its run group and the output filled from the Result; the one guarded
`set_complete` transition closes it. The run group's legacy rows stay as the canonical record (a
typed-program replay gains its program row there, since no run-correlated Program event exists),
and the viewport renders the card because the renderer consults `say_turn_view()` first. Runs keep
the legacy projection when the pattern does not match — tools/approvals/speculative prompts, more
than one Program, an errored Result, a failed/cancelled/completed-without-output run, a run unit
this snapshot did not create (a locally rendered live turn or an earlier snapshot), or a unit that
already carries a card (a later snapshot must not reset `show_program` or duplicate output). The
Snapshot branch also skips the run-unaffiliated Program source unit for programs a replayed card
already covers, so one turn never wears two representations.

**Interactive named-Brain runs project semantic turns, not lifecycle UUIDs (#422).** A remote or
replayed `Interactive` run retains its `RunId` in orchestration state, but its transcript unit is
an `Interactive` WorkUnit: program and tool/approval rows remain inspectable, successful `Result`
bytes are the assistant body, and the terminal run status settles the unit. Failed and cancelled
turns carry their actionable diagnostic once on that unit. Speculative/background runs retain the
explicit run-labelled activity projection. Brain-context recaps attribute a correlated successful
Interactive result to `assistant`, never to the journal sender `daemon`.

**Locally executed runs never wear the legacy run group (#978).** When this frontend holds the
home runner lease, runs it initiated are executed by its own callback paths and rendered through
the same local units every other local path uses, so daemon lifecycle events for those runs never
create or paint the run group — `project_remote_brain_live_run_event` consults
`LocallyRenderedRuns` (delegated turns and programs in flight, queued local projections, and
completed pure-say runs marked in `locally_say_projected_runs`) plus the initiating-attachment
prediction on `RunStarted`. The delegated typed-program path (`dispatch_named_brain_program`)
paints the source unit and `begin_say_turn` card itself and registers a `LocalBrainProjection`
(`program_seq` = the run's request sequence) when the VM settles; `finish_named_brain_turn` does
the same for delegated provider turns, so a pure-say turn's terminal `Result` is suppressed and
marked, while a tool-bearing turn keeps its run-group rows. The pushed-program echo is matched
against a bounded `locally_pushed_programs` marker so the run-unaffiliated source unit renders
exactly once whichever arrives first. A peer's run on another Brain, or one this frontend did not
initiate or execute, keeps its legacy rows (the control regressions pin this).

**The state is shared, and that is the known weakness.** Every handler takes `&mut self` on an
`EventLoop` whose field list is long. Before adding a field, check whether the state belongs to a
query (`query_state.rs`) or to a tool run (`tool_execution.rs`) instead.

**Generic-compatible status is capability-aware and secret-free.** `/status` identifies an
`openai_compatible` profile as generic OpenAI-compatible Chat Completions and reports only its
profile/model, operator-configured context and output limits, image-input state, and whether
capacity is configured. Endpoint URLs and paths, credential references, environment bindings,
headers, and resolved secrets never enter the status view. Built-in provider reports retain their
existing four-line shape. Capability values are attested for the configured compatible model only;
when a Brain or one-shot model overlay selects a different model, `/status` withholds those values
and names both the selected overlay and configured model in a secret-free provenance diagnostic.

**`/clear` and `/reset` clear every provider-context representation, not only the visible
transcript.** `EventLoop` and `LlmLoop` share one session `SharedSummaryCache`. The active command
path holds the conversation write boundary while it calls `ConversationHistory::clear` (removing
committed messages and provider-invisible staged tool rounds) and invalidates that cache, so a
regrown history cannot reuse summary bytes from before the reset. The cache generation also makes
a compactor holding an in-flight pre-clear snapshot discard its plan and reject a late commit. It
then propagates that rejection through request assembly, terminally cancels the invalidated query,
and never falls back to sending either its stale summary or its stale raw window. Before the
command confirms that the conversation is starting fresh, it terminalizes and releases the exact
old active query and discards only input queued before the reset boundary. A post-confirmation
prompt can therefore claim the active slot immediately; late invalidation, completion, and tool
events carrying the old query id are idempotent and cannot release the new owner, delete its
queue, overwrite its status, or repopulate the cleared cache. Query processing checks that same
query-owned cancellation/state fence immediately after a non-streaming provider returns and at
each streaming receive boundary, before response bytes can mutate a WorkUnit, publish statistics,
stage or execute tools, run wire source, or emit a visible failure.
`test_clear_and_reset_commands_remove_committed_and_staged_provider_context`
pins the raw/staged boundary; `test_clear_and_reset_commands_invalidate_summary_before_actual_generator_request`
drives both spellings through the real `LlmLoop` and captures the assembled generator request;
`test_clear_and_reset_during_inflight_summary_never_send_stale_provider_request` blocks the real
summarizer across both commands, starts and settles a fresh provider turn before releasing it, and
proves the late invalidation/tool events have no provider, cache, active-query, queue, status, or
transcript effect;
`test_clear_and_reset_fence_late_non_streaming_provider_success_and_failure` blocks a real main
provider across both commands and proves both a rich success and a failure are fenced before any
post-reset projection or execution;
`test_unrelated_help_command_preserves_provider_context_and_staged_round` keeps raw, staged, and
summary context non-destructive for unrelated slash commands.

**Resume identity.** A clean interactive exit prints `To resume, run: finch attach <brain-name>`
whenever `register_home_brain` reached the daemon this session and the home Brain's entry was
created or loaded in the durable store (`EventLoop::home_brain_registered`), or a visible
persistence failure otherwise. This is *not* the same as the live `home_brain` watch attachment
being connected at the moment of exit — that can be `None` mid-reconnect while the Brain still
exists on disk, and the exit line must still agree with `finch brain ls` in that case (#1387: it
used to key off `home_brain.is_some()` and claimed "not saved" for a Brain `brain ls` listed).
It does not mint or checkpoint a client-owned UUID session file. `ConversationHistory` stays an
in-memory projection; named Brains are the durable store.

**Focused tests:** `./scripts/test_brains.sh cargo test --lib -- cli::repl_event::`. Tests for the
loop are in `event_loop/tests.rs` and reach private items through `use super::*` exactly as they
did when they were inline.
