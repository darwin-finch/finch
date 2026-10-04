# server capsule: HTTP transport and named-Brain request handling

Supplements the root [`AGENTS.md`](../../CLAUDE.md), which still applies in full.

**Owns** `src/server/`: Axum router construction, HTTP authentication and rate limiting,
OpenAI-compatible request/response types, named-Brain HTTP handlers, runner callbacks, approval
bridging, the daemon-side Cap'n Proto RPC adapter and listener lifecycle, server lifecycle
state, and — since issue #1354 — the daemon-owned Claude CLI Subscription `claude`
process/MCP-bridge-socket lifecycle per Brain (`claude_cli_session.rs`). Daemon process lifecycle
belongs to `src/daemon`; durable Brain state belongs to `crates/finch-brain`; the domain-neutral
schema/protocol/socket core belongs to `crates/finch-ipc`.

**Facade:** callers outside this directory use flat `crate::server::Item` imports from the
`pub use` list in [`mod.rs`](mod.rs); they must not name `server::handlers` or
`server::openai_types`. The daemon-side `ipc` child is private; the client uses runtime's
delivery-frame encoder directly. Route-only HTTP payloads and approval handles stay out of the
public facade; root Brain application tests retain crate-visible approval registration access.
The flat facade exports `AppError` so external state-directory node-handler callers can name
their result type. Keep wire behavior, authentication, persistence, and runner semantics unchanged
in facade-only work. Do not recreate a generated symbol catalog.

**Dependencies:** Brain services and persistence, IPC-facing runner callbacks, provider and model
adapters, the program runtime, tool execution, configuration, metrics, and local generation. The
server composes those services; transport handlers must not become their owner.

**Invariants and lifetimes:** the daemon composes one shared `AgentServer` for HTTP and IPC;
the server owns listener/background-task lifetime, while the Brain store owns durable named-Brain
state. Authentication and rate limiting belong at the transport boundary, before a handler
mutates Brain state or dispatches a runner request. An IPC disconnect or retry must not invent
a second committed Brain turn. Keep runner callback and approval protocol changes covered by
the server and supervised IPC tests, not just a helper unit test.

**Schedule-delivery diagnostics report transitions, not retry cadence.** The process-ephemeral
failure registry in `schedule_delivery::FailureEpisodes` is keyed by exact indexed `BrainId` plus
display name and fenced by the schedule set's process-local lifecycle epoch, so cancel-last plus
same-identity recreation cannot form an ABA. The first failed attempt emits one actionable WARN with the full cause chain;
unchanged retries are silent; the first real success emits one INFO and clears the episode.
Archive, unused removal, external absence, and schedule retirement silently reconcile the entry,
and a completion that crossed one of those boundaries cannot reinsert it. A successful one-shot
captures its completion observation while the execution lane is still held, so its natural final
retirement does not hide the real recovery. That observation comes from the atomic schedule-queue
commit together with the in-lock entry observation; recovery requires that entry to match the
loop's sampled generation, so cancel-last/recreate before queueing cannot substitute successor
work, while a later external cancellation cannot masquerade as the delivery's own retirement.
Post-queue failures carry the same entry and completion observations: a final one-shot may retire
before runner readiness or dispatch fails, and that genuine failure must still WARN when the
captured completion remains the current lifecycle. An error raised inside the queue call after
the entry sample carries that same pair, so a one-shot committed earlier in the call cannot hide
a later sibling's failure. The episode is stored at that completion epoch: the one-shot's
retirement has already moved the activity epoch, and reconcile only keeps the epoch still
active. The sibling's unchanged retries stay silent, and its later success emits the one INFO.
A successor one-shot queued after cancel-last recreation warns when its captured completion is
still current; it does not recover the predecessor, because recovery still requires the loop's
sampled epoch. A runner that is live for the first
readiness check and gone before dispatch reports the queued count, the same success as a runner
that was already absent, so that retirement does not drop the recovery line. Pre-queue failures
retain the stricter active observation check.
Restart deliberately
starts empty, so the first real post-restart failure may warn again but never invents recovery.

**A participant's task-list replacement commits outside the Brain execution lane (issue #1585,
`todo_write` timed out after 30 seconds).** A `Prompt` holds the lane in
`submit_named_brain_event_with_authority_and_receipt` (`handlers.rs`) until its turn returns, and
that turn's own `todo_write` submits `TaskListReplaced` through the same driver attachment; queued
behind the lane, the tool waited for the turn that issued it. The replacement now returns before
the lane, after the same submittable-kind filter, `attachment_can_submit` role check, and
`validate_submitted_brain_tasks` as before, and through the same `BrainStore::push` /
`push_idempotent` append. It may therefore sit between a turn's request and its result in the
journal; a run's rebuilt context still reads the task list at or before its own request
(`named_brain_provider_messages_at`). `ApprovalDecided` is the only other kind that returns before
the lane; every other submission, `ParticipantMessage` included, still waits for the running turn.
A retried replacement replays its mutation receipt instead of appending again.
`test_task_list_replacement_during_an_active_turn_does_not_wait_for_the_turn`,
`test_two_task_list_replacements_during_one_turn_commit_once_each_in_order`,
`test_task_list_replacement_from_a_second_attachment_during_a_turn_keeps_role_authority`,
`test_retried_task_list_replacement_during_a_turn_replays_its_receipt_once`,
`test_participant_message_during_an_active_turn_still_waits_for_the_turn`,
`test_task_list_replacement_after_the_turns_terminal_state_commits_exactly_once`, and
`test_task_list_replaced_mid_turn_survives_restart_and_does_not_leak_into_that_turn` in
`handlers/handler_tests.rs`;
`test_todo_write_during_a_brain_turn_returns_success_through_the_real_tool_path` in
`src/cli/repl_event/tool_execution.rs` drives the real tool path against the same lane.

**On the WebSocket transport the same replacement also leaves the socket's serial command worker
(issue #1646, a task list write over the socket still waited for the turn).** `watch_named_brain`
(`handlers/runs.rs`) runs one command at a time per socket, so a socket whose `Prompt` is suspended
on its turn answers nothing else until that turn ends. `Submit(TaskListReplaced)` is routed to the
second worker that already carries `Submit(ApprovalDecided)`; the two kinds stay ordered with each
other and are the only commands that skip the queue. This depends on the paragraph above: a
replacement that still took the execution lane would park that second worker and block approval
decisions behind it. Role checks, validation, the revision precondition, and receipt replay are
unchanged, because the command still runs through `execute_remote_brain_command`.
`test_task_list_replacement_on_the_socket_during_a_turn_does_not_wait_for_the_turn`,
`test_socket_task_list_replacements_during_a_turn_commit_once_each_in_order_and_a_retry_replays`,
`test_participant_message_on_the_socket_during_a_turn_still_waits_for_the_turn`, and
`test_observer_socket_task_list_replacement_during_a_turn_is_refused` in
`handlers/task_list_socket_tests.rs` drive the real socket handler.

**Named-Brain provider execution is intrinsically bounded.** Every delegated `Prompt` and
`SpeculativePrompt` receives the daemon-authored `TypedRuntime::intrinsic_grants()` ceiling in its
`RunnerTurnRequest`; the frontend may transport and apply that ceiling but may not reconstruct it
from provider output or ambient runtime grants. Both raw VM wire and provider-native
`submit_program` tool calls use it. Restored queued prompts retain the same ceiling.
`named_brain_prompt_raw_wire_cannot_reuse_an_owner_file_write_grant` in `ipc/tests.rs` exercises
the raw-wire path;
`named_brain_prompt_submit_program_cannot_reuse_an_owner_file_write_grant` exercises the full
Cap'n Proto/runner/provider-tool/runtime boundary including cancellation and a late approval; and
`restarted_queued_prompts_dispatch_task_state_at_their_exact_request_sequence` pins restart replay.

**Local generation calls run on `tokio::task::spawn_blocking`, never inline on an axum worker
thread** — `LocalGenerator::try_generate_from_pattern`/`_with_tools`/`_streaming` are synchronous,
CPU/GPU-bound llama.cpp calls with no internal `.await`; every HTTP call site in this directory
(the SSE path in `handle_chat_completions_streaming`, the `RouteDecision::Local` branch of
`handle_chat_completions`, and `handle_local_only_query`, all in `openai_handlers.rs`; and the
`RouteDecision::Local` branch of `handle_message` — `POST /v1/messages`, `handlers.rs`) wraps the
call — including acquiring and dropping the generator's write lock — entirely inside a
`spawn_blocking` closure, so it cannot occupy a worker thread (or hold that lock across an
`.await`) for the duration of a turn. `local_only_generation_does_not_starve_concurrent_tasks`
(`openai_handlers.rs`) and `test_local_message_generation_does_not_starve_concurrent_tasks`
(`handlers/handler_tests.rs`) pin this on a `worker_threads = 1` runtime, where an inline call
would deterministically starve every other task on the daemon (#1254). This is not solely a
daemon-process concern: the interactive REPL's own in-process local-generation branch
(`Repl::process_query`'s `try_generate_from_pattern` call, `src/cli/repl.rs`) shared the identical
inline-call defect and is fixed the same way (`test_repl_local_generation_does_not_starve_concurrent_tasks`),
because the REPL's own tokio task also drives TUI rendering and input polling — the daemon HTTP
path and the REPL path are two independent instances of the same mistake, not one invariant that
implies the other. `DaemonLocalGenerator` (used when a daemon connection exists) stays exempt
because it only makes an async HTTP call to the daemon, a separate OS process; the in-process
fallback (`QwenGenerator`, used only when no daemon connection exists) already wraps its own
blocking calls the same way (`src/generators/qwen.rs`).

**Daemon-owned Claude CLI Subscription sessions (issue #1354).** `BrainService.claudeCliRound`
looks up (or lazily creates) a per-Brain `finch_providers::ClaudeCliProvider` in
`ClaudeCliSessionRegistry` and drives it directly — tool execution and approval stay wherever the
*caller* runs its own real `ToolLoop`, exactly as #1341/#1350 already built; only process/transport
ownership moved. The round-driving work (`drive_claude_cli_round`) runs on its own
`spawn_local` task, decoupled from the calling RPC's own future: capnp-rpc drops that future on a
client disconnect, and driving inline would release the per-Brain lock — and could race a second
`claude` process into existence — before the provider's own already-detached `execute_turn` task
(spawned inside `send_message_stream_validated`) had actually finished. A request that does not
correctly answer a currently parked tool call is rejected before ever touching
`send_message_stream` (`ClaudeCliProvider::parked_call_match`), so an uninformed reattaching
frontend cannot silently abandon a live, possibly mid-human-approval `claude` child.
`ClaudeCliSessionRegistry::remove` (called on Brain archive, `src/server/handlers/lifecycle.rs`)
is the only explicit teardown; a live child otherwise survives until the daemon process itself
exits, and `run_daemon`'s SIGTERM handler (`src/main.rs`, itself part of this issue's fix — SIGTERM
previously had no handler at all, so a real `finch daemon-stop` skipped every graceful-shutdown
step in that function, not only this one) is what lets that happen on an ordinary restart; a
SIGKILL/crash genuinely orphans a live child, which is an OS-level fact no software here can
prevent. Covered end-to-end by real spawned-daemon-subprocess tests in
`tests/claude_cli_daemon_session_test.rs`: a mismatched reattach fails closed and leaves the real
parked call resumable, a frontend disconnect mid-pending-tool-call does not lose the session, and a
graceful SIGTERM restart reaps the live `claude` child with no orphan.

**Extension rule:** put domain-neutral wire contracts in `finch-ipc` and durable Brain rules in
`finch-brain`; add a flat server export only for a real application caller. Do not move root
provider, tool, model, or daemon policy into a transport handler to make a crate split look cheap.

**Focused tests:** `./scripts/test_brains.sh cargo test --lib -- server::` and
`./scripts/test_brains.sh cargo test --test daemon_stdio_binding`. Run `python3 scripts/check_facade_boundaries.py` whenever
the module surface changes, and run the full supervised workspace suite before extraction.
