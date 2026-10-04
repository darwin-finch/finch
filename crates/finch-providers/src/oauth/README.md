# OAuth lifecycle

This module implements provider-neutral device authorization and authorization-code flows,
credential refresh and revocation, and generation-checked persistence. Provider-specific URLs,
scopes, issuers, and token validation come from dialects outside the module. Finch owns the
user-facing ceremony and the decision to save named credentials in application configuration;
this module owns the token state machine and durable store.

For ChatGPT login, `src/cli/chatgpt_auth.rs` constructs an `OAuthClient` with the OpenAI dialect
and a `FileOAuthCredentialStore`, presents a device code, and calls
`finish_device_authorization_commit` with caller-owned cancellation. Its UI and named-credential
metadata stay in the CLI; the OAuth module commits the token record and returns an exact
generation for compensation after a later application failure.

For Grok login, `src/cli/grok_auth.rs` uses the same `OAuthClient` contract with the xAI dialect.
It can refresh an existing named credential or finish a new device authorization, while its own
presentation code stays outside this module. Both callers depend on terminal cancellation,
expiry, and denial and on secret-free status handling.

Read [AGENTS.md](AGENTS.md) for security invariants, [mod.rs](mod.rs) for the module facade, and
`cargo doc -p finch-providers --no-deps --open` for callable methods.
