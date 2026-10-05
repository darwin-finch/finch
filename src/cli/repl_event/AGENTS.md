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
| `event_loop/patterns.rs` | `/patterns`: list, remove, clear, and add standing tool approvals |
| `runner_recovery.rs` | leftover-daemon / lease / environment mismatch labels |

**Adding an event.** Add the variant to `ReplEvent` in `events.rs`, then handle it in
`dispatch.rs`. The compiler will tell you the match is non-exhaustive — that is the point of the
enum, so do not add a catch-all arm. Keep the arm short: if handling takes more than a few lines,
put a `pub(super)` method in whichever file above it belongs to and call it.

**The metrics logger is injected; the event loop never derives a metrics path itself.**
`RuntimeParts::metrics_logger` is the only source of `EventLoop::metrics_logger`, which is both
what a turn records through and what `/metrics` reads. `Repl::run_event_loop` hands in the logger
built from `Config::metrics_dir`; the headless test runners in `event_loop/brain.rs` hand in
`None`, and a test that wants metrics injects its own temporary directory through
`set_metrics_logger_for_test`. `EventLoop::new` used to build a logger from `dirs::home_dir()`,
so every fixture provider driven through a test runner under a bare `cargo test` wrote a
wire-adherence row into the real `~/.finch/metrics` and showed up in the user's `/metrics`
report (issue #1629, test runs filling the report with fixture providers).
`test_headless_runner_fixture_wire_metric_never_reaches_the_home_metrics_directory` and
`test_injected_metrics_logger_keeps_the_fixture_wire_metric_in_the_tests_own_directory` in
`event_loop/tests.rs` drive a real turn through a uniquely named fixture provider and fail if a
row naming it appears under the home directory.

**A request is one user turn, and each turn records exactly one source-free request metric.**
The unit is the query id: every tool round and provider continuation of a turn runs under it, so
a turn that makes five provider calls is one request. `QueryStateManager` (`query_state.rs`)
owns the recording because it is the one place every terminal transition already passes through
under one lock: `try_publish_completion_content` (completed), `update_state` (failed), and
`cancel_query` (cancelled). `take_request_metric` builds the row on the first terminal
transition and sets `QueryMetadata::request_metric_taken`, so a provider that answers after a
cancel, or a second path closing the same query, adds nothing. The row is appended after the
state lock is released, and a write failure is logged, never surfaced into the turn.
`LlmLoop::spawn_query` binds the provider entry name, model, and local-or-cloud kind (the
configured `ProviderEntry::is_local`, never a guess: a generator no entry matches is recorded
without a kind); `process_query_with_tools` rebinds when its routing step picks the separate
on-device generator. The row holds identities, an outcome, and a duration only; it does not hash
the prompt. Duration is the whole turn, including tool execution and time spent waiting on an
approval prompt. Before this, nothing under `repl_event/` recorded a request, so `/metrics`
showed zeros after real turns (issue #1629, `/metrics` request counters always zero). The
daemon's HTTP `GET /metrics` endpoint is a separate surface tracked by issue #131 (make daemon
health and metrics truthful with request route provenance); it should adopt this row shape
rather than a second vocabulary. Tests, all in `event_loop/tests.rs` on the real worker:
`test_completed_turn_records_exactly_one_request_metric_for_its_cloud_provider_entry`,
`test_turn_on_a_local_provider_entry_is_counted_as_local`,
`test_failed_turn_records_exactly_one_failed_request_metric`,
`test_cancelled_turn_records_exactly_one_cancelled_request_metric_despite_late_completion`,
`test_turn_with_a_tool_round_records_one_request_metric_not_one_per_provider_call`.

**IPC recovery is header/status, not transcript.** Peer disconnect, home event-watch loss, and
runner reconnect attempts update `StatusBar` (`SessionLabel`) through
`EventLoop::project_ipc_recovery_header`. They must not call `output_manager.write_info` — that
commits sticky conversation rows between program source and output (#819). Leftover-daemon /
environment mismatch at startup still uses `apply_home_runner_startup` (header plus the detailed
startup TUI line, #794).

**A self-correcting wire-protocol repair round hides its raw diagnostic from the transcript on success, but preserves actionable diagnosis on failure (#1383, #1690).** `execute_wire_with_single_repair` (`query_processor.rs`) rejects a provider response that is not a valid `ProgramSubmission`, then — when the rejection is repairable — retries once with a corrective prompt before showing anything to the user. While the corrective generation runs, the raw diagnostic is kept out of the transcript so a self-healed turn shows only the final successful output. If the repair round fails for any reason (cancelled while waiting on it, the corrective generation itself errors or returns no usable program, or the repaired program is rejected or errors too), the transcript retains the diagnostic error explanation alongside `WIRE_REPAIR_FAILED_MESSAGE` (`wire_repair_failed_message`), so the user receives actionable diagnostic information rather than an empty or wiped notice (#1690). Only a first-pass rejection classified as non-repairable at all (no retry ever attempted) shows its raw diagnostic directly without a repair attempt. `test_repairable_rejection_diagnostic_reaches_debug_log_not_transcript`, `failed_wire_repair_preserves_error_diagnosis_in_transcript`, `failed_wire_turn_stays_expanded_program_output`, `named_brain_effect_audit_cancel_before_repair_never_invokes_provider`, and `named_brain_effect_audit_cancel_drops_inflight_repair_without_continuation` in `query_processor.rs` cover the successful-repair, failed-repair, cancelled-before-repair, and cancelled-during-repair cases.

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

**`/patterns` manages the approval path's own store, and only for the owner (#1634, "/patterns
commands say 'recognized but not yet implemented'").** `event_loop/patterns.rs` reads and changes
the `ToolExecutor` behind `ToolExecutionCoordinator::tool_executor` — the same confirmation cache
`spawn_tool_execution` consults — never a second copy loaded from disk, so a removed or cleared
approval stops auto-approving on the next tool call. `/patterns list` shows each pattern's ID,
what it matches, `persistent` or `session`, and its match count as plain text. `/patterns remove`
and `/patterns clear` cover session approvals as well as persistent ones and save the store;
`clear` always confirms, `remove` confirms above ten matches, and `add` is a dialog wizard that
stores nothing until its final confirmation. A `/patterns` dialog is a state machine on
`EventLoop::pending_patterns_dialog`, resolved first in `resolve_dialog_result`; when another
prompt (tool, VM, remote-Brain approval, `ShowDialog`, poset confirmation) has taken the dialog
area, the answer belongs to that prompt and the `/patterns` flow is dropped without changing
anything. The commands are reachable only through `handle_user_input`, which receives the local
composer's line; a peer's prompt arrives as a named-Brain turn and is never parsed as a slash
command. Pinned in `event_loop/tests.rs` by
`test_patterns_list_shows_id_match_scope_and_count_from_the_live_approval_store`,
`test_patterns_remove_revokes_a_persistent_pattern_so_the_next_call_asks`,
`test_patterns_remove_revokes_a_session_pattern_so_the_next_call_asks`,
`test_patterns_remove_of_a_heavily_used_pattern_waits_for_confirmation`,
`test_patterns_clear_confirms_then_revokes_every_persistent_and_session_approval`,
`test_patterns_add_wizard_saves_a_persistent_pattern_that_auto_approves`,
`test_patterns_dialog_displaced_by_a_tool_approval_changes_nothing_and_answers_the_tool`, and
`test_peer_turn_prompt_naming_a_patterns_command_cannot_list_or_change_owner_approvals`.

**A turn's reply is one row with the turn's own duration, and unreported input is never shown as
zero (#1671, every Claude CLI reply wrapped in two `(ran 0s)` rows with `0 in`).**
`execute_wire_with_single_repair` (`query_processor.rs`) runs a plain-prose reply's wrapped `say`
form in the same output unit as the rejected attempt, so no empty completed row precedes the reply;
its output units are created through `OutputManager::start_reply_work_unit` from the turn's
generation unit, so `(ran Ns)` counts from the request being sent, across tool rounds, rather than
from the finished reply being printed. `SessionUsageLedger`'s readouts (`src/cli/usage.rs`) print
`N out` alone when output was recorded with no input, because a provider that reports no input has
not used zero. Pinned in `event_loop/turn_indicator_tests.rs` by
`test_bridge_prose_reply_renders_one_reply_row_and_one_timing_row`,
`test_completed_turn_reports_the_time_since_its_request_was_sent`,
`test_completed_turn_with_a_tool_round_reports_the_whole_turn_time`, and
`test_session_token_line_omits_input_the_provider_never_reported`.

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
