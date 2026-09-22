# Finch OAuth compatibility path

This module preserves the `crate::oauth` import path for application callers. It owns no OAuth
state machine, provider dialect, token file, or user-interface behavior. Those belong to
`finch-providers`' private `oauth` module (re-exported flat from that crate's facade),
provider-specific dialects, and the CLI respectively. The facade
re-exports selected provider-neutral OAuth contracts while code moves across crate boundaries.

For ChatGPT login, `src/cli/chatgpt_auth.rs` imports `OAuthClient` and
`FileOAuthCredentialStore` through this facade, supplies the OpenAI dialect from
`crate::providers`, and owns device-code presentation and named-credential Config updates.

For Grok login, `src/cli/grok_auth.rs` uses the same re-exported client and store with the xAI
dialect. It owns its own status text and cancellation handoff; token refresh, terminal device
outcomes, and durable mutation remain in the provider OAuth implementation.

Read [AGENTS.md](AGENTS.md) for the compatibility rule, [mod.rs](mod.rs) for the exact exports,
and the [OAuth implementation capsule](../../crates/finch-providers/src/oauth/README.md) for
state-machine invariants. Rustdoc supplies methods on the re-exported types.
