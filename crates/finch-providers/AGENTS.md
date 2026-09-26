# finch-providers capsule: LLM transports, OAuth, catalogs, and credential ports

Supplements the root [`AGENTS.md`](../../CLAUDE.md), which still applies in full.

**Owns** `crates/finch-providers/src/`: the `LlmProvider` / `ProviderBackend` dispatch
boundary, provider-neutral wire types and stream events, model catalog, capabilities,
usage/allowance, OAuth lifecycle (`oauth` module), provider-specific OAuth dialects,
and the Claude / OpenAI-compatible / Gemini / ChatGPT / SuperGrok / Claude-subscription adapters.

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
- OAuth cancellation, expiry, and denial are terminal; interrupted refresh
  recovers only as tombstones.
- Secrets never appear in `Debug`, logs, or error text.
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
