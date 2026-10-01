# finch-providers capsule: LLM transports, OAuth, catalogs, and credential ports

Supplements the root [`AGENTS.md`](../../CLAUDE.md), which still applies in full.

**Owns** `crates/finch-providers/src/`: the `LlmProvider` / `ProviderBackend` dispatch
boundary, provider-neutral wire types and stream events, model catalog, capabilities,
usage/allowance, OAuth lifecycle (`oauth` module), provider-specific OAuth dialects,
and the Claude / OpenAI-compatible (including origin-pinned direct Meta Model API) / Gemini /
ChatGPT / SuperGrok / Claude-subscription adapters.

**Platform boundary (issue #1357).** `claude_cli.rs`'s MCP tool-call bridge is a
`tokio::net::{UnixListener, UnixStream}` transport with no Windows equivalent. Rather than gating
the whole module (which would also require `src/providers/factory.rs`, `src/providers/mod.rs`,
`src/server/claude_cli_session.rs`, `src/providers/claude_cli_daemon.rs`, and `src/server/mod.rs`
/`ipc.rs` in the root crate — all of which reference `ClaudeCliProvider`/`ClaudeCliSessionRegistry`
unconditionally, issue #1354 grew that surface further — to gain their own platform split),
`ClaudeCliProvider` itself stays one type, constructible and nameable on every platform. Only the
socket-dependent internals (`RunningTurn`, `ParkedTurn`, `DriveOutcome`, the `parked` field,
`execute_turn` and everything it calls, `drive`/`read_bridge_request`/`write_bridge_response`) are
`#[cfg(unix)]`, matching this same file's pre-existing
`spawn_retrying_text_file_busy`/`harden_bridge_socket_permissions` split. `parked_call_match` and
the `ProviderBackend::send_message_validated`/`send_message_stream_validated` entry points get
`#[cfg(not(unix))]` twins: the former reports `ParkedCallMatch::NoPendingCall` (true, since nothing
can ever park a turn there), the latter fail closed with a clear "not supported on this platform"
error. Every other file that constructs or stores a `ClaudeCliProvider` — `factory.rs`,
`claude_cli_session.rs`, `claude_cli_daemon.rs`, `server/ipc.rs`'s `claude_cli_round` RPC handler —
needs no changes at all: the type, its constructors, and its trait impl exist unconditionally, so
selecting this provider on a non-Unix build still compiles and fails only at first real use, with a
named cause. Test code (`#[cfg(test)] mod tests`, heavily `UnixStream`-based) is untouched — `cargo
check` never compiles `#[cfg(test)]` items, so it does not affect the Windows compile this fixes,
and splitting it is unnecessary extra work with no current Windows test CI job to serve.

**Boundary:** [README.md](README.md) traces configured-provider and setup-catalog callers.
[`src/lib.rs`](src/lib.rs) is the flat facade; every handwritten child module is private,
including `oauth` (its contract is re-exported flat from the facade, issue #958). Rustdoc
renders callable methods. Finch uses compatibility facades at `src/providers` and `src/oauth`.
Do not regenerate a signature catalog. Provider transport notes remain in the shared docs tree
until they are extracted here.

**Surface tiers (issue #958 audit).** The `pub use` list in [`src/lib.rs`](src/lib.rs) is the
cross-crate contract; everything below it is tiered so implementation detail cannot leak back in:

- **Crate-internal (`pub(crate)`):** the injected-environment seam (`ProviderPorts`,
  `HttpTransport`, `Clock`, `Sleeper`, `AuthorizationPresenter`, `BillingActionConfirmer`,
  `ProviderTelemetry`, `ReqwestTransport`, `SystemClock`, `TokioSleeper`, `FrozenClock`,
  `InstantSleeper` — retained deliberately; `ports.rs` carries a scoped dead-code allow because
  OAuth today reads only the HTTP transport and the timeout), `with_retry`/`NonRetriableError`,
  `ToolBindingTable::{empty, entries, is_empty, len, encode_semantic, decode_wire_call}`,
  `BoundTool` (+`anthropic_tool`, `chatgpt_function`), `WireToolIdentity`, `WireToolKind`,
  `ResultEncoding`, `MAX_ADVERTISED_TOOLS`, `ToolBindingError`,
  `compile_tool_bindings`/`compile_from_definitions`, `GrokJwksVerifier::production`,
  `GrokCredentialSource`, `GrokCredentialLease`,
  `OpenAIProvider::new_compatible_named_header`, the request helpers
  (`ProviderRequest::{tool_policy, sanitize_messages, truncate_to_context_limit}`),
  `ModelCapabilities::validate_request`, `DEFAULT_MAX_OUTPUT_TOKENS`, and the Anthropic
  SSE-parse types `StreamEvent`/`StreamDelta`/`SseContentBlock`.
- **Test-only (`#[cfg(test)]`):** `ProviderSession::{new, with_config, state, reset_state}`,
  `MessageRequest::append_user_message`, `StreamEvent::{is_text_delta, is_tool_use_start, text}`,
  `OpenAIProvider::{new_openai, new_grok}`, and the `ToolBindingError::UnknownWireProtocol`
  variant.
- **Oauth flatten (issue #958):** `oauth` is a private child module; its contract — the 17 items
  the Finch `src/oauth` facade re-exports — is published flat from the crate facade. No oauth
  item changed visibility; the module path is no longer nameable by callers.

Deleted as unreferenced by the same audit: `FallbackChain::{len, is_empty}`,
`GrokJwksVerifier::for_test`, `OpenAIProvider::new_mistral`,
`ProviderSession::send_message_with_truncation` (+ its now test-only `truncate_context`
helper), `ProviderSession::optimization_stats` and the `OptimizationStats` type,
`BoundTool::{openai_tool, gemini_declaration}`, `ToolBindingTable::{protocol, provider, model}`,
`ProviderRequest::with_tool_policy`, `ToolAuthority::as_str`,
`ToolCompilePolicy::{with_authority, with_native_grant}`.

Kept `pub` with verified external callers despite sitting in the audited internal families:
`ClaudeProvider::new` (`src/claude/client.rs`), `ProviderSession::{provider_name,
with_shared_provider, send_message, send_message_stream}` (`src/cli/repl.rs`),
`FallbackChain::{new, from_shared, primary_provider}` (`src/providers/factory.rs`,
`src/server`), and the whole dispatch/wire/capability/credential/oauth surface traced by the
README and the facade.

**Dependencies:** this unpublished crate depends on HTTP/crypto/async libraries and
never on the root `finch` crate, Brain, TUI, daemon, CLI orchestration, tools
execution, or application `Config`. Credential types, reasoning effort, and
tool-call *wire* types live here and are re-exported by Finch. Environmental
effects are injected through [`ProviderPorts`](src/ports.rs).

**Invariants:**
- Dialects own every provider fact (URL, client ID, scope, issuer, token shape).
- `ValidatedProviderRequest` is unforgeable; backends consume `into_request_for`.
- Tool calls become semantic `ToolUse` only after adapter validation.
- Opaque reasoning/replay material is not display content.
- Direct Meta Model API `reasoning_content` is dialect-scoped: buffered values are validated and
  discarded, while streamed values become bounded `ThinkingDelta` events carrying Meta
  provider/model provenance and no opaque replay. It never enters assistant-visible completed
  content, and canonical OpenAI does not accept the Meta-only field.
- Adapters emit `TextDelta` and `ContentBlockComplete` (plus usage/allowance/metadata).
  OpenAI and Claude also emit native `ToolCallDelta` / `ToolCallComplete`.
  Generation-layer translation of `ContentBlockComplete(ToolUse)` into
  `ToolCallComplete` lives in `finch-generation`. Dual encoding of the same
  id+input is one call at the ToolLoop. Do not flatten per-adapter parsers
  into a lowest-common-denominator decoder.
- Per-request tool wire names come from `compile_tool_bindings` (issue #241).
  Semantic Finch identities persist in history; adapters decode through the
  immutable table compiled at `validate_provider_request` and returned by
  `into_request_for`. Generic OpenAI-compatible clients do not use
  ChatGPT/Codex reserved namespaces. Provider-native tools are advertised
  only with a Finch handler and grant.
- **Claude CLI subscription tool calls run through Finch's own MCP bridge, never the CLI's own
  built-in tools (issue #1309), and the bridge is a pure translator, not an execution authority
  (issue #1341).** `claude_cli.rs` always spawns `claude` with `--tools ""` (its own
  Read/Write/Edit/Bash/Grep/Glob never execute on the CLI's own authority — verified directly that
  several of them auto-execute for real before any permission hook runs at all: a file read inside
  the CLI's own working directory, anything under the OS temp directory regardless of working
  directory, and read-only Bash commands per Claude Code's own permissions docs, none of which is
  gateable from outside the CLI). `ClaudeCliProvider::capabilities().tools` is `Supported`, but
  only because Finch's own tool implementations (a fixed, curated subset: `CLAUDE_CLI_TOOL_NAMES`)
  are served to the CLI over MCP (`--mcp-config`, `--strict-mcp-config` so the user's own personal
  MCP integrations never leak in, `--allowedTools "mcp__finch__<tool>,..."` so the bridge is never
  blocked on an approval prompt with no host to answer it). The MCP server is this same Finch
  binary, re-invoked with the hidden `CLAUDE_CLI_MCP_BRIDGE_FLAG` (`src/cli/claude_cli_bridge.rs`
  in the root crate, outside this crate's own execution-free boundary). That process no longer
  executes anything itself (the original #1309 shape built its own `PermissionManager::for_peer()`
  and dispatched directly — wrong, because it duplicated execution/approval authority that already
  exists and works correctly for every other provider, and it could never actually prompt a human):
  it resolves the MCP wire name to a plain Finch tool name and forwards `{name, input}`,
  line-delimited JSON, over a Unix domain socket named by `CLAUDE_CLI_TOOL_SOCKET_ENV` — an `env`
  entry this crate puts on the MCP server's own spec in `--mcp-config`, never on `claude`'s own
  environment — back to *this* transport, running in the frontend process that owns the Brain's
  turn. `ClaudeCliBridgeToolRequest`/`ClaudeCliBridgeToolResponse` are the wire shape for that
  socket. This transport surfaces the forwarded call as an ordinary `StreamChunk::ToolCallComplete`
  in the same stream every other provider's tool calls already take (`pump_until_settled` in
  `claude_cli.rs`), so the frontend's real, interactive `ToolLoop` executes it — real approval, real
  file access, the same authority as every other provider. Because a single `claude` subprocess
  invocation is one long-lived process (not the discrete request/response shape every other
  provider's turn has), and Claude Code's own internal MCP round trip blocks that process until the
  bridge answers, the underlying child is **parked** (`RunningTurn`/`ParkedTurn`, kept alive, never
  re-spawned) for however long real interactive approval takes; `ClaudeCliProvider::execute_turn`
  recognizes the next Finch-level round's trailing `ToolResult` as the answer to a specific parked
  call, replies to the bridge's still-open connection, and resumes reading the same child's stdout
  — matching the same "observe the whole stream, batch-execute, re-invoke with the result appended"
  round-trip every other provider already goes through, with no changes needed to `ToolLoop`,
  `ToolExecutionCoordinator`, or `query_processor.rs`.
  **Accepted trade-off, verified live: multiple tools requested in one turn are handled
  sequentially, one Finch-level round per tool — and the real CLI itself never pipelines a second
  `tools/call` before the first is answered (issue #1351, measured against the real, logged-in
  `claude` CLI 2.1.284 on 2026-09-29).** `pump_until_settled`/`drive` accept and pause on exactly
  one bridge connection at a time. The bridge's own JSON-RPC loop is already single-threaded and
  sequential (`claude_cli_bridge.rs`'s `run()` fully awaits one `tools/call`'s round trip, including
  the real interactive approval wait, before reading its next stdin line — true before and after
  #1341), so this transport never deadlocks or cross-talks between two tool calls in the same turn:
  `mcp-two-tool-calls`'s test fixture (`claude_cli.rs`) proves the *sequential* case — a second tool
  requested only after the first one's real result returns — completes correctly across two
  pause/resume cycles.
  Three live turns against the real CLI (driving `ClaudeCliProvider` directly, real binary, real
  OAuth subscription login, real bridge subprocess, no fixture) tested whether the CLI's own MCP
  client ever sends a second `tools/call` before reading the first one's reply: (1) "read these
  three unrelated files and report their line counts", (2) the same request but explicitly told to
  read "simultaneously, in parallel ... do not wait for one to finish before starting the next",
  (3) three independent `bash` echo commands with the same explicit parallel instruction. All three
  turns produced a model reply that *narrated* parallel intent ("I'll read all three files in
  parallel." / "Running the three commands in parallel.") but the wire traffic was strictly
  sequential every time: a temporary instrumentation patch on `claude_cli_bridge.rs`'s `run()` (an
  env-gated probe, reverted after the investigation) logged, for every `tools/call` line received,
  whether a second complete stdin line arrived within 500ms *before this bridge had sent any reply*
  — since no reply had gone out yet, any such line could only be an unprompted, unpaired pipelined
  request. It never fired: across all three turns, the very next `tools/call` (observed as little as
  ~3ms after the previous reply was written) always arrived *after*, never before, the prior reply.
  The real CLI's MCP client dispatches one `tools/call` at a time and waits for its JSON-RPC
  response before sending the next, regardless of how the model narrates its own intent. Multiple
  tools requested "in parallel" therefore still surface to Finch as separate
  `[assistant: ToolUse]`/`[user: ToolResult]` round pairs in `ConversationHistory`, never one
  combined round the way a native HTTP-based provider produces for a parallel-tool-call turn — but
  this is the CLI's own real, observed behavior, not a Finch approximation of a hazard that turned
  out not to exist. Given this, the pre-existing design decision — one Finch-level round per tool
  call — is retained as an accepted trade-off (issue #1351's own framing), not revisited: it also
  avoids surfacing two simultaneous interactive approval prompts competing for a human's attention.
  Separately, even if a future CLI version *did* pipeline, the bridge's architecture makes it
  provably safe regardless: `relay_over_socket` has no read timeout, `tools/call` payloads are tiny
  relative to the kernel pipe buffer, and `run()`'s single-threaded loop never has more than one
  socket relay in flight, so a pipelined second line simply waits, buffered, until the first
  completes — no deadlock, no dropped call, and replies stay correctly paired with their own
  requests by strict FIFO order. `two_pipelined_tools_call_requests_on_stdin_are_still_answered_correctly_one_at_a_time`
  (`tests/claude_cli_bridge_subprocess.rs`) pins this deterministically at the real subprocess
  boundary: both `tools/call` requests are written to the real, compiled bridge binary's stdin back
  to back, with no reply read in between (genuine pipelining pressure, not just fast sequential
  dispatch, with an artificial slow-approval delay on the first call to remove any doubt), and the
  test asserts no deadlock and correct, non-cross-talking request/reply pairing.
  This is why `ClaudeGenerator::needs_prompt_injection`
  (`src/generators/claude.rs`) now bypasses its #1303 prompt-injection fold for this provider —
  `supports_tools()` derives straight from `capabilities().tools`, so the two decisions cannot drift
  apart. `TurnRecord::absorb_line` still observes (counts/logs) any `tool_use` block in the CLI's
  own stdout for visibility, but deliberately emits no `StreamChunk` for it there — the *real*
  `ToolCallComplete` is emitted by `pump_until_settled`, driven by the bridge's own forwarded socket
  request, a decoupled and differently-ordered signal delivered over a different pipe; emitting a
  second one from the stdout-observation path would violate "dual encoding of the same id+input is
  one call at the ToolLoop" by handing the ToolLoop two different ids for what is one real call.
  **The bridge socket is hardened to owner-only (0o600), not merely bound (issue #1341, found in
  independent review).** `read_bridge_request`/`forward_tool_call` trust any well-formed request on
  any accepted connection with no peer verification, and `/tmp` (where the socket lives, chosen for
  `sockaddr_un.sun_path`'s ~104-byte limit) is world-listable; an unhardened socket would let *any*
  local process connect and be answered as if it were the real bridge for the lifetime of the turn.
  `bind_tool_socket` chmods immediately after bind, mirroring `src/server/ipc.rs`'s
  `harden_ipc_socket_permissions` for the identical unauthenticated-local-socket pattern (issue
  #911's rationale applies verbatim).
  **Text typed while a tool call is parked is queued and delivered as a merged follow-up round, not
  discarded (issue #1341, found in independent review).**
  `ConversationHistory::append_text_blocks_to_last_user_message` folds any text the user types while
  a tool call is executing into the *same* trailing message as the eventual `ToolResult`, so the next
  round's tail can be `[ToolResult, Text]`, not a bare `ToolResult`. `tail_tool_result` extracts the
  matching `ToolResult` (for the bridge reply) and any accompanying `Text` blocks separately; the
  text can never reach the parked `claude` process directly (its stdin was already written and closed
  at spawn time), so it queues in `ClaudeCliProvider::pending_followup` and is sent as its own
  `--resume` round — wrapped through `user_input_line` like any other input line — the moment some
  round on the session next completes without pausing again. Because that is a second `claude`
  process for what the caller sees as one Finch-level round, `TurnRecord::absorb_followup` merges its
  record into the original one (accumulating `assistant_text`, the same pattern issue #1331 already
  established for multiple `assistant` events within one invocation) rather than letting
  `execute_turn` return the follow-up's outcome on its own, which would silently drop the first
  round's completed text and desync it from what actually streamed live.
  **A mid-turn tool call also affects text ordering, not just tool execution (issue #1331).** The
  real CLI can put a preamble text block (e.g. "I'll check that file.") in the very same
  `assistant` event as the `tool_use` block, then emit a second `assistant` event with the final
  answer once the tool result folds back in — every `text_delta` from both messages still streams
  live as an ordinary `TextDelta` chunk, with no per-message boundary signal. `TurnRecord`'s
  `assistant_text` field therefore accumulates every `assistant` event's own text across the whole
  turn instead of the last message overwriting the others, so `response_text()` (what
  `ContentBlockComplete` reports as the turn's completed content) always equals the full streamed
  total; letting the last message win silently dropped the preamble and desynced the two, which
  `src/cli/repl_event/query_processor.rs` detects and fails the turn over ("Provider streaming text
  did not match its completed content"). This is user-visible by design, not a side effect to hide:
  the finished transcript now includes any preamble narration the model produced before its tool
  call, matching exactly what streamed to the screen in real time.
  **Accumulation is conditioned on an actual `tool_use` bridging the two text segments, not merely
  on both arriving within one invocation (issue #1388, found via a real Claude CLI Subscription
  session).** #1331 established that `assistant_text` accumulates rather than overwrites so a
  preamble survives a mid-turn tool call. But the real CLI can also emit two independent, *complete*
  `assistant` text events back to back with no `tool_use` anywhere between them — a confused first
  reply reacting to bare context, immediately followed by a second, unrelated real answer (the
  reproduction: asked to spawn a subagent for `47*89`, no `▶ Tools (N call)` indicator ever
  appeared). Appending blindly fused the two into one string with no separator, mid-word
  (`...help with?I don't have...`), fed straight into the wire parser as bogus "source" and shown to
  the user as one garbled reply. `TurnRecord::tool_use_bridges_next_text` tracks whether a `tool_use`
  block has been observed since text was last recorded; `absorb_line`'s `assistant` handling inserts
  `INDEPENDENT_REPLY_SEPARATOR` before a new message's text only when that flag is false. The live
  delta stream needs the identical decision made at the identical point — `stream_delta_text` carries
  no per-message boundary of its own, so `TurnRecord::mid_delta_run` marks whether a
  `content_block_delta` run already has an open, not-yet-finalized message, and the separator is
  prefixed to the first delta of a new run under the same condition — or the streamed text and
  `response_text()` desync and `query_processor.rs`'s streamed-vs-completed check fails the turn.
  `two_text_only_assistant_events_with_no_tool_use_between_them_get_a_separator` (unit-level) and
  `two_unrelated_assistant_replies_with_no_tool_use_get_a_separator_not_fused_mid_word`
  (production-boundary, via a new `two-unrelated-replies` fixture) cover this; the existing #1331
  fixtures (`tool-preamble`, `mcp-tool-call-with-preamble`) are unaffected since their `tool_use`
  block keeps the flag true across the boundary.
  **That whole-turn accumulation must not leak into a *resumed* call's own `ContentBlockComplete`
  (found via a real Claude CLI Subscription session, not a synthetic fixture).** #1331's fix covers
  a preamble and final answer arriving within *one continuous `execute_turn` call* (the `claude`
  process pauses only long enough to forward a `tools/call` over the bridge, then keeps streaming on
  the same `deltas` channel). It does not cover the more common case: the pause returns all the way
  to `query_processor.rs`, which executes the tool for real and opens a *second*, separate
  `send_message_stream` call to resume — a fresh `deltas` channel that only ever carries this call's
  own new text, even though `TurnRecord::response_text()` (the same record, correctly still
  accumulating per #1331) now spans both calls. Reporting the whole accumulated text as *this* call's
  `ContentBlockComplete` desynced `query_processor.rs`'s check again, on real live traffic, months
  after #1331 landed. `TurnRecord::already_streamed_len` snapshots the prefix length right before
  `execute_turn` resumes a parked turn's `pump_until_settled`, and
  `TurnRecord::newly_streamed_text()` (used only by the streaming `ContentBlockComplete` site) reports
  just the suffix beyond it — `response_text()` itself, and the non-streaming `response_from` path,
  are untouched, since they correctly want the whole turn.
  `resumed_round_reports_only_its_own_new_text_not_the_prior_rounds_preamble` reproduces this at the
  production boundary via a new `mcp-tool-call-with-preamble` fixture (the existing `tool-preamble`
  fixture never genuinely pauses, so it could not have caught this).
  **Since issue #1354, this transport can be driven by more than one caller connection over its
  own lifetime — the daemon now owns and constructs it (`src/server/claude_cli_session.rs` in the
  root crate), reused unchanged; this crate's own process-management logic did not move or
  change.** `execute_turn`'s internal `take_matching_parked_turn` still silently abandons a
  non-matching parked turn and starts fresh — correct for the single-frontend cancel/retry case
  this type was originally built for (issue #1341), where the same frontend made that decision
  itself — but a caller reachable from more than one connection over the session's lifetime must
  not let an uninformed request hit that path uninformed. `ClaudeCliProvider::parked_call_match`
  is a non-destructive peek (`self.parked.lock().await`, `.as_ref()`, never `.take()`) such a
  caller uses to reject a mismatched request *before* calling `execute_turn` at all, reporting
  `ParkedCallMatch::Mismatch { pending_id }` instead of silently killing a live, possibly
  mid-human-approval `claude` child. `parked_call_match_reports_mismatch_and_leaves_the_parked_turn_alive`
  and `parked_call_match_is_a_non_destructive_peek_that_never_abandons_a_correct_resume` (both in
  `claude_cli.rs`) prove the peek never disturbs the parked state either way.
- **The inner `claude` subprocess must never act as an independent agent with its own persistent
  state, skills, or interactive prompts — it is a scoped model-generation backend, full stop
  (issue #1389, found via a real Claude CLI Subscription session).** `--tools ""` only turns off
  the CLI's own *built-in* Read/Write/Edit/Bash/etc; it does nothing about a *real, legitimately
  advertised* Finch tool served over the MCP bridge (`mcp_bridge_args`). A completely ordinary
  chat message ("remember that") made the model call Finch's own MCP-bridged `write` tool to
  persist a file shaped exactly like Claude Code's own auto-memory feature (`name`/`description`/
  `metadata.type` frontmatter) under `~/.claude/projects/<hashed-cwd>/memory/` — real files, on
  the host filesystem, entirely outside Finch's own Brain/memory store, surfaced to the user as an
  ordinary Finch write-approval dialog with no indication it targeted a different tool's storage.
  `invocation_args()` now always includes `--restricted --permission-prompts none`, verified live
  against the real `claude` CLI 2.1.284 on 2026-09-28 (a fake MCP server standing in for the real
  bridge, advertising just a `write` tool exactly as `mcp_bridge_args` does): four consecutive
  live runs with these flags produced no memory write and no `write` `tool_use` at all, and a
  separate live run confirmed a real, explicitly requested MCP tool call still round-trips
  correctly (`system/init` reports the `finch` server `connected`, a proper `tool_use` is emitted,
  the fake bridge receives it) under the machine's real OAuth subscription login (no
  `ANTHROPIC_API_KEY` in the environment). Two flags were tried first and rejected — do not re-add
  either without new live evidence: **`--bare`** forces `ANTHROPIC_API_KEY`/`apiKeyHelper` auth and
  never reads OAuth or the keychain per its own help text, which breaks this provider's entire
  reason for existing (driving the user's Claude subscription login, `claude_oauth.rs`).
  **`--safe-mode`** did suppress the memory write in the same live repro, but a second live run —
  asking the model to use the MCP-bridged `write` tool for a real, explicit task — showed
  `system/init`'s `mcp_servers` come back `[]`: it drops even an *explicitly passed* `--mcp-config`
  server, not just ambient/settings-discovered ones, and the model fell back to emitting a
  hallucinated `<invoke name="Write">...` text block that nothing here can execute — silently
  breaking every real Finch tool call through this provider. `--permission-prompts none`
  additionally closes the "Edit in $EDITOR"-style terminal-hijack risk from the issue's report:
  anything that would still try to prompt a human outside Finch's own approval flow is denied
  automatically instead of ever reaching an interactive dialog, regardless of what triggers it.
  `invocation_args_always_scope_the_subprocess_against_its_own_persistent_state_and_prompts` pins
  both flags present (with and without tools requested) and the two rejected flags absent;
  `real_tool_call_still_round_trips_with_the_new_subprocess_scoping_flags_present` is the
  production-boundary proof that a real MCP tool call still completes end to end through the
  spawned process with the new argv (both in `claude_cli.rs`).
- OAuth cancellation, expiry, and denial are terminal; interrupted refresh
  recovers only as tombstones.
- Secrets never appear in `Debug`, logs, or error text.
- **Direct Meta Model API credentials are a distinct origin-bound namespace.**
  `CredentialProvider::MetaModelApi` requires issuer `meta-model-api`, audience
  `EndpointFamily::MetaModelApi`, and the fixed `https://api.meta.ai` origin.
  `OpenAIProvider::new_meta_model_api` uses the documented Chat Completions
  surface and `muse-spark-1.3`; it never falls back to OpenCode Zen, Muse Code,
  contributor-tier models, or a custom compatible origin (#317, direct Meta Model API for Muse Spark; reviewed
  against official Meta documentation on 2026-10-01).
- Subscription and API billing are never automatically interchangeable.
- Claude requests opt into Anthropic's top-level automatic moving-prefix cache. OpenAI API and
  ChatGPT transports continue to send stable, complete prefixes and rely on those services'
  implicit prompt caching; provider context caching never permits Finch to omit conversation
  messages from a stateless request.
- **Claude subscription OAuth is opt-in and disabled by default.** `claude_oauth.rs`
  authenticates against Anthropic using Claude Code's own OAuth client id
  (`9d1c250a-e61b-44d9-88ed-5944d1962f5e`) — Finch has no client id of its own registered with
  Anthropic for this surface. Reusing another application's client identity to talk to a
  provider's own OAuth servers matches a pattern Anthropic has a **documented history of actively
  detecting and blocking** for other third-party tools; this is real, observed enforcement
  behavior, not a hypothetical risk. This crate itself has no `Config` and cannot gate anything,
  so the application layer gates both entry points before any network access:
  `require_claude_subscription_oauth_opt_in` in `src/providers/factory.rs` (provider
  construction, re-checked on every construction because a stored credential can outlive the
  flag being turned back off) and `require_claude_subscription_oauth_opt_in` in `src/main.rs`
  (`finch auth login claude`). Both read `Config::features.claude_subscription_oauth_enabled`,
  which defaults to `false` (`test_features_config_safe_defaults` in `src/config/settings.rs`
  pins this). There is no setup-wizard entry for this provider; it is CLI-only
  (`finch auth login|status|logout|recover claude`) by design, so a casual user does not stumble
  into the reused-identity risk without reading the opt-in error message that names it. Anthropic
  exposes no known public token-revocation endpoint for this client id in any source consulted
  while building this dialect (unlike ChatGPT/Grok, which both have one); `finch auth logout
  claude` is therefore local-only (`ClaudeAuthService::logout` in `src/cli/claude_auth.rs`) and
  does not claim server-side revocation. The exact scope set (`claude_required_scopes` —
  `user:profile`, `user:inference`, `user:sessions:claude_code`, `user:mcp_servers`,
  `user:file_upload`, deliberately excluding `org:create_api_key`) and the
  `platform.claude.com` token-endpoint origin are moderate-confidence, third-party
  reverse-engineered protocol detail, not first-party documentation; see `claude_oauth.rs`'s
  module doc comment for sourcing and what a live login attempt would still need to confirm.

**Focused tests:**
```bash
./scripts/test_brains.sh cargo test -p finch-providers --lib
./scripts/test_brains.sh cargo test -p finch-providers --test crate_boundary
./scripts/test_brains.sh cargo test -p finch-providers --example custom_adapter
```

Feature-disabled builds: `cargo check -p finch-providers --no-default-features`.
Default features enable the current Claude, OpenAI-compatible, Gemini, ChatGPT,
and SuperGrok subscription adapters.

**Agent-context audit:** a worker can implement a transport using this capsule, README, facade,
and rustdoc without opening Finch application code. Config-taking factory mapping remains in
Finch `src/providers`.

**Named remainders (not this extraction):**
- Thread streaming HTTP through adapter constructors (named remainder of #775).
- Feature-gate optional deps (`reqwest`/`png`/`ring`) so `--no-default-features` drops them (Issue 4 / #775).
- Stop baking `~/.finch` into crate constructors; Finch should pass cache/store roots (Issue 5 / #775).
