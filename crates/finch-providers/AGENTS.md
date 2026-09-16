# finch-providers capsule: LLM transports, OAuth, catalogs, and credential ports

Supplements the root [`AGENTS.md`](../../CLAUDE.md), which still applies in full.

**Owns** `crates/finch-providers/src/`: the `LlmProvider` / `ProviderBackend` dispatch
boundary, provider-neutral wire types and stream events, model catalog, capabilities,
usage/allowance, OAuth lifecycle (`oauth` module), provider-specific OAuth dialects,
and the Claude / OpenAI-compatible / Gemini / ChatGPT adapters.

**Interface:** [`INTERFACE.md`](INTERFACE.md) is generated from `src/lib.rs`. Child
modules are private; the `pub use` list is the whole public surface. Finch consumes
this crate through compatibility facades at `src/providers` and `src/oauth`.

**Documentation:** [`docs/README.md`](docs/README.md) owns crate-local reference
material. Provider transport notes remain in the shared docs tree until they are
extracted here.

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
- Per-request tool wire names come from [`compile_tool_bindings`](src/tool_bindings.rs)
  (issue #241). Semantic Finch identities persist in history; adapters decode
  through the immutable table compiled at `validate_provider_request`. Generic
  OpenAI-compatible clients do not use ChatGPT/Codex reserved namespaces.
  Provider-native tools are advertised only with a Finch handler and grant.
- OAuth cancellation, expiry, and denial are terminal; interrupted refresh
  recovers only as tombstones.
- Secrets never appear in `Debug`, logs, or error text.
- Subscription and API billing are never automatically interchangeable.

**Focused tests:**
```bash
./scripts/test_brains.sh cargo test -p finch-providers --lib
./scripts/test_brains.sh cargo test -p finch-providers --test crate_boundary
./scripts/test_brains.sh cargo test -p finch-providers --example custom_adapter
```

Feature-disabled builds: `cargo check -p finch-providers --no-default-features`.
Default features enable the current Claude, OpenAI-compatible, Gemini, and ChatGPT
adapters.

**Agent-context audit:** a worker can understand, implement against, and test this
crate from this capsule plus `INTERFACE.md` without opening Finch application
code. Config-taking factory mapping remains in Finch `src/providers`.

**Named remainders (not this extraction):**
- Thread streaming HTTP through adapter constructors (named remainder of #775).
- Feature-gate optional deps (`reqwest`/`png`/`ring`) so `--no-default-features` drops them (Issue 4 / #775).
- Stop baking `~/.finch` into crate constructors; Finch should pass cache/store roots (Issue 5 / #775).
- `generate_interfaces.py` omits `async fn` trait methods (Issue 6 / hygiene).
