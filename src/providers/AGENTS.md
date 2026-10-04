# providers capsule: Finch Config mapping onto finch-providers

Supplements the root [`AGENTS.md`](../../CLAUDE.md), which still applies in full.

**Owns** this Finch compatibility facade: Config-taking factory and catalog mapping
(`factory.rs`, `catalog.rs`) and re-exports of `finch-providers`. Transports, OAuth
dialects, wire types, dispatch, and adapter tests live in
[`crates/finch-providers/AGENTS.md`](../../crates/finch-providers/AGENTS.md).

**Boundary:** the [README](README.md) traces application startup and REPL profile callers.
[`mod.rs`](mod.rs) is the flat facade; rustdoc renders methods on exported types. Callers
outside this directory use `crate::providers::Item`. Do not recreate a signature catalog.

**Dependencies:** `finch-providers` (transports and contracts), `config` (application
`Config` / `ProviderEntry`). Do not add Brain, TUI, or tool execution here.

**Narrow daemon exception (issue #1354):** `claude_cli_daemon.rs`'s `DaemonClaudeCliProvider`
depends on `crate::client::ipc::IpcClient` — the one daemon-connection dependency this capsule
otherwise forbids. It exists because `finch-providers` itself must never depend on the daemon or
IPC (see that crate's own `AGENTS.md`), so a thin, daemon-backed `ProviderBackend` that fronts a
daemon-owned Claude CLI Subscription session cannot live there; mapping "which concrete
`ProviderBackend` a Brain's configuration selects" onto a constructor is exactly this capsule's own
job, whether that constructor spawns locally (`finch_providers::ClaudeCliProvider`, still
available and used for the non-daemon/standalone case) or proxies to the daemon
(`DaemonClaudeCliProvider`). `IpcClient` is `!Send`/`!Sync` (capnp-rpc), so
`DaemonClaudeCliProvider` never holds one directly; it holds a `Send + Sync`
`DaemonClaudeCliHandle` — a channel to a dedicated `spawn_local` task that owns the real
connection — matching the same actor-behind-a-`Send`-safe-handle shape
`src/server/brain_runner.rs`'s `BrainRunnerBroker` already uses daemon-side for the identical
`!Send` constraint.

**Owns the compatibility path** `crate::providers::Message` for the universal wire
types. The Claude HTTP client in `src/claude` must not re-export that trio.

**Invariants:** dialects, `ValidatedProviderRequest`, and adapter parsing belong to
the crate. This facade only maps application configuration onto crate constructors.
SuperGrok subscription construction is `GrokSubscriptionProvider::production`; it is
not an OpenAI-compatible xAI Console API-key profile.
Direct Meta Model API construction is `OpenAIProvider::new_meta_model_api` behind
the distinct `CredentialProvider::MetaModelApi` mapping; endpoint overrides are
rejected before secret resolution.
A ChatGPT subscription's model list comes from the signed-in account, not an API key:
`refresh_chatgpt_subscription_from_config` (`catalog.rs`) preflights the whole provider
graph and then asks a `ChatGptAccountModels` source, whose production implementation is
`ChatGptSubscriptionProvider::account_models`. An invalid graph, a revoked credential, or
a profile of another provider is refused before the source is asked —
`test_chatgpt_subscription_catalog_rejects_a_revoked_credential_before_asking_the_account`,
`test_chatgpt_subscription_catalog_reports_the_accounts_models_as_discovered` in `catalog.rs`.

**Focused tests:** `./scripts/test_brains.sh cargo test --lib -- providers::`; crate
tests via `./scripts/test_brains.sh cargo test -p finch-providers --lib`; also
`cargo test --test gemini_streaming_test --test provider_token_binding_test` when
wire behavior changes.
