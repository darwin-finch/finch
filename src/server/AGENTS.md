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
