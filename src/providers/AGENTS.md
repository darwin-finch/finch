# providers capsule: provider graph, wire transports, and the model catalog

Supplements the root [`AGENTS.md`](../../CLAUDE.md), which still applies in full.

**Owns** `src/providers/`: the provider graph and factory, per-dialect wire transports
(Claude, OpenAI, Gemini, ChatGPT OAuth and subscription), OpenAI JWKS verification, the
model catalog with its static fallback and cache, teacher session management, the
fallback chain, and the non-overridable validated dispatch boundary. The Claude HTTP
client lives in `src/claude` (its own subtree); credential persistence is the `oauth`
capsule; which provider a conversation uses is the caller's decision, not this subtree's.

**Interface:** [`INTERFACE.md`](INTERFACE.md) lists every exported item with its
signature. Child modules are private (`alignment`, `chatgpt_oauth`,
`chatgpt_subscription`, `claude`, `endpoints`, `factory`, `fallback_chain`, `gemini`,
`model_catalog`, `openai`, `openai_jwks`, `teacher_session`, `types`), so the `pub use`
list in `src/providers/mod.rs` is the whole public surface. Callers outside this
directory use `crate::providers::Item`; they must not name `providers::<child>::`.

**Dependencies:** `config` (provider
entry and teacher types), `oauth` (dialect-facing credential types), `tools`,
`models`, `generators` (streaming chunks). `crate::cli::ConversationHistory` appears
only inside `claude.rs` tests.

**Owns the universal wire types.** `wire_types` defines `Message`, `ContentBlock`,
and `ImageSource` — the provider-neutral conversation vocabulary every caller
uses (`crate::providers::Message`). The Claude HTTP client in `src/claude`
consumes these types like any other transport and keeps only its own
`MessageRequest`/`MessageResponse` envelopes; it must not re-export the trio
under claude paths. `wire_type_boundary_tests` in `mod.rs` fails if a caller
reaches the trio through a claude path.

**Invariants:**

- Dialects own every provider fact — URL, client ID, scope, token issuer, endpoint
  behavior. Shared code and callers know none of them.
- `ValidatedProviderRequest` is minted only inside `validated_boundary`; its fields and
  constructor are private, so even a provider backend elsewhere in the crate cannot
  fabricate a dispatch token.
- Contract tests in `mod.rs` pin reasoning-control and output-token limits; wire changes
  must keep them green.

**Focused tests:** `./scripts/test_brains.sh cargo test --lib -- providers::`; also
`cargo test --test gemini_streaming_test --test provider_token_binding_test` when wire
behavior changes. Run the full suite when changing a re-exported `pub` item.
