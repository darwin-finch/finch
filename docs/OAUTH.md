# Finch-native OAuth compatibility boundary

Finch's OAuth state machine is provider-neutral. It implements RFC 8628 device
authorization, authorization-code with S256 PKCE, state and nonce correlation,
bounded polling/HTTP, refresh rotation, revocation, crash markers, and durable
generation-checked persistence. Provider dialects own every client identity,
endpoint, issuer, audience, scope, token shape, account claim, and error mapping.
Tokens never become generic bearer credentials.

The first production-shaped adapter is ChatGPT subscription OAuth. It is
strictly separate from the OpenAI Platform API-key provider:

- OAuth authorization uses the versioned OpenAI public-client compatibility
  dialect pinned by `CHATGPT_OAUTH_PROTOCOL_REVISION`.
- Subscription inference is bound only to
  `https://chatgpt.com/backend-api/codex` and its named ChatGPT account.
- `api.openai.com`, Platform API keys, compatible endpoints, and silent account
  fallback are rejected by this adapter.
- The public-client and subscription-service contracts are upstream
  compatibility risks, not stable third-party contracts. Protocol drift must
  produce an actionable error; Finch must never relax issuer, audience,
  signature, nonce, account, scope, or origin checks to recover.

The compatibility fixtures are pinned to OpenAI Codex commit
`3e4707b34b16e139fcb7ad11ab8445993b62bba1`, specifically
`codex-rs/login/src/device_code_auth.rs`, `codex-rs/login/src/server.rs`,
`codex-rs/login/src/auth/default_client.rs`, and
`codex-rs/login/src/token_data.rs`. That browser flow uses the Codex-only
`codex_cli_rs` originator. Finch records the exact `/oauth/authorize` endpoint
and six scopes from the source, but the ChatGPT adapter keeps browser PKCE
disabled rather than impersonating that originator. The provider-neutral core
and synthetic dialects still exercise browser PKCE, state, and nonce.

## Persistence and #174 binding

On Unix, the file store walks from a trusted filesystem root using
descriptor-relative `openat`/`mkdirat` operations with `O_NOFOLLOW`, validates
every ancestor, and performs descriptor-relative atomic replacement plus
directory `fsync`. It deliberately fails closed on non-Unix platforms until an
equivalent descriptor- or handle-anchored implementation exists. Records are
private, bounded, generation-checked, and retain revoked tombstones. A refresh
writes `mutation_pending` before remote rotation; restart then fails closed
until explicit recovery tombstones the record and the user signs in again.

Successful authorization projects only secret-free metadata into
`ProviderCredential`: stable name, `oauth_device` or `oauth_browser_pkce`,
ChatGPT provider/issuer/audience, exact account, scopes, expiry, refreshability,
and an opaque `oauth-store:<name>` reference. The injected OAuth resolver loads
only that exact record and rechecks the full binding without refreshing or
performing network activity.

## Current integration fence

This initial slice deliberately does not expose a setup wizard or `finch auth`
command and does not enable ChatGPT provider construction. The default OpenAI
token verifier fails closed until an audited signature/JWKS implementation is
wired for the pinned dialect. No real authorization or private service request
is permitted from this slice. Remaining #105 work includes that verifier,
setup/auth-command orchestration, complete direct subscription transport and
stream conformance, secure platform credential-store integration, and opt-in
live acceptance after independent review.
