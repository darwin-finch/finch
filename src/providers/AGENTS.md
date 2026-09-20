# providers capsule: Finch Config mapping onto finch-providers

Supplements the root [`AGENTS.md`](../../CLAUDE.md), which still applies in full.

**Owns** this Finch compatibility facade: Config-taking factory and catalog mapping
(`factory.rs`, `catalog.rs`) and re-exports of `finch-providers`. Transports, OAuth
dialects, wire types, dispatch, and adapter tests live in
[`crates/finch-providers/AGENTS.md`](../../crates/finch-providers/AGENTS.md).

**Interface:** `mod.rs`'s re-exports are the whole public surface — read it directly for exact
signatures. Callers outside this directory use `crate::providers::Item`.

**Dependencies:** `finch-providers` (transports and contracts), `config` (application
`Config` / `ProviderEntry` / `TeacherEntry`). Do not add Brain, TUI, daemon, or tool
execution here.

**Owns the compatibility path** `crate::providers::Message` for the universal wire
types. The Claude HTTP client in `src/claude` must not re-export that trio.

**Invariants:** dialects, `ValidatedProviderRequest`, and adapter parsing belong to
the crate. This facade only maps application configuration onto crate constructors.
SuperGrok subscription construction is `GrokSubscriptionProvider::production`; it is
not an OpenAI-compatible xAI Console API-key profile.

**Focused tests:** `./scripts/test_brains.sh cargo test --lib -- providers::`; crate
tests via `./scripts/test_brains.sh cargo test -p finch-providers --lib`; also
`cargo test --test gemini_streaming_test --test provider_token_binding_test` when
wire behavior changes.
