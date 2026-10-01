# Finch provider transports

This crate owns provider-neutral request and stream contracts plus the concrete Claude,
OpenAI-compatible (including direct Meta Model API for Muse Spark), Gemini, ChatGPT, Grok, and
Claude-subscription transports, provider capability
and model-catalog logic, credential ports, and OAuth state machines. It validates
provider-specific wire behavior without importing Finch application configuration. The root
application maps `Config` onto these contracts and decides which provider profile to run;
generation lifecycle and tool execution belong elsewhere.

Grok subscription authorization is a versioned compatibility adapter for the open-source Grok
Build public client, not xAI Console API-key authentication. Device initiation identifies
`grok-build`; both initiation and polling carry the pinned public client version and one stable
surface (`ui`, `cli`, or `headless`). The official token shape permits an opaque access bearer and
an optional refresh token, so Finch derives identity only from a separately verified ES256 ID
token (or, when no ID token exists, from a verified signed access token). A protocol-revision
change never migrates bearer material: the one recognized predecessor is converted to a
secret-cleared tombstone and requires explicit sign-in.

Claude subscription (`claude_oauth.rs`/`claude_subscription.rs`) authenticates a claude.ai
Pro/Max/Team subscription via Anthropic's browser authorization-code + PKCE OAuth flow, reusing
the audited Anthropic Messages wire protocol (`claude.rs`) with an OAuth bearer token in place of
an API key. It is **opt-in and disabled by default** in the application layer
(`Config::features.claude_subscription_oauth_enabled`, checked in `src/cli/claude_auth.rs` and
`src/providers/factory.rs`) because it reuses Claude Code's own OAuth client identity rather than
one Anthropic issued to Finch — see the Invariants section below and `claude_oauth.rs`'s module
doc comment for the full rationale and confidence level.

For configured generation, `src/providers/factory.rs` reads Finch provider entries and
credentials, constructs a concrete provider through this crate, and exposes a `ProviderGraph` to
the REPL or daemon. It wraps provider dispatch to enforce credential lifecycle. The crate owns
validated requests and transport behavior; the factory owns application profile selection.

For model setup, `src/cli/setup_wizard/catalog.rs` converts the chosen provider and persisted
entry into a `ModelCatalogProfile`, using this crate's auth and endpoint contract. The setup
wizard asks the catalog for available models and retains the application decision about what to
save. A catalog response is not proof that a model or provider path has passed end-to-end
conformance.

Provider requests keep stable conversation prefixes so service-side prompt caches can match them.
Claude requests explicitly enable Anthropic's automatic moving-prefix cache; OpenAI API and
ChatGPT use their service-managed implicit caches. Finch still sends complete request context to
remote stateless APIs, because a cache hit is a provider optimization rather than stored
conversation authority.

The direct Meta Model API dialect accepts Muse Spark's documented `reasoning_content` response
field without treating it as assistant output. Streaming reasoning crosses the provider boundary
only as bounded `StreamChunk::ThinkingDelta` activity with Meta provider/model provenance;
schema-valid `null` and empty streamed values are ignored, and buffered reasoning is validated and
discarded. It is never appended to `ProviderResponse` content, tool arguments, or opaque replay
material. Canonical OpenAI parsing remains strict and does not inherit Meta-only response fields.

Configured generic OpenAI-compatible profiles use a strict, bounded response boundary without
claiming first-party OpenAI semantics. Streaming requires `text/event-stream`, a valid terminal
choice, and exactly one `[DONE]`; malformed, sparse, unknown, truncated, or post-terminal events
fail the stream. Individual SSE lines and accumulated tool arguments are limited to 1 MiB, a whole
stream to 4 MiB, a non-stream success body to 32 MiB, and an error body to 64 KiB. Compatible error
diagnostics redact response bodies because an upstream can reflect the configured credential or
private request headers. These response rules do not add request extensions or expose generic
`reasoning_content`; legacy compatible constructors retain their existing dialect behavior.

Read [AGENTS.md](AGENTS.md) for dependency and security rules, [src/lib.rs](src/lib.rs) for the
crate facade, and `cargo doc -p finch-providers --no-deps --open` for signatures. The OAuth
state machine has its own [capsule](src/oauth/AGENTS.md); its contract is re-exported flat from
the crate facade, and external callers never name the `oauth` module path (issue #958).
