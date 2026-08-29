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

Production token verification is pinned to issuer `https://auth.openai.com`,
the exact discovery and JWKS paths on that origin, RS256, RSA signing keys, and
an unambiguous `kid`. Both the ID token and access token must have valid
signatures and matching subject/account/plan claims. The ID-token audience is
the pinned public client; a multi-audience identity token additionally requires
that exact client as `azp`. Issuer spelling is exact and every token requires a
bounded signed `iat` preceding `exp`. The separately checked access-token audience for the
pinned compatibility fixture is `https://api.openai.com/v1`. That JWT claim is
authorization-server metadata only: Finch still sends the subscription token
exclusively to the separately bound `chatgpt.com/backend-api/codex` service and
never to the Platform API. Discovery redirects, proxy/environment routing,
header-selected keys, duplicate JSON fields or key IDs, algorithm confusion,
oversized documents, stale/rotated keys, and issuer/JWKS substitution fail
closed.

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

`finch auth status chatgpt` is a read-only, secret-free local check; a missing
store remains missing. `finch auth login chatgpt` starts device authorization,
prints the accessible URL and one-time code, reports the countdown, and accepts
`--copy` and explicit `--open`. `finch auth logout chatgpt` performs bounded
revocation and retains a local tombstone. Every command accepts
`--credential <stable-name>` (default `chatgpt:default`) so compatible model
profiles can reuse one account while distinct references keep accounts
isolated. Interrupted refresh/revoke state is reported as
`recovery_required`, never as signed out.
Run `finch auth recover chatgpt --credential <name>` to convert an interrupted
mutation into a local secret-cleared tombstone without HTTP, then use explicit
login. Expired credentials are reported as `expired`, not `active`.

First-run setup, `finch setup`, and `/setup` use one post-wizard device ceremony.
The complete secret-free graph is validated before OAuth begins; cancellation,
denial, expiry, or an invalid sibling prevents config/persona save. The
temporary account-unknown preflight record is never persisted. Only verified
token metadata is written to `config.toml`; tokens remain in Finch's
descriptor-anchored store.
For multiple new accounts, setup records which successful issues require
compensation. A later denial, cancellation, or invalid account locally
tombstones every earlier new token before returning; restart can then resume
the same named transaction without leaving an orphaned active credential.

Browser PKCE remains disabled because the pinned browser protocol uses a
Codex-only originator that Finch does not impersonate. Windows token persistence
also remains fail-closed pending a descriptor/handle-anchored implementation;
CI compiles the exact provider-neutral and verifier sources there. Direct
ChatGPT subscription inference, catalog, streaming, allowance, and provenance
transport remain #202. No live authorization is part of automated tests or
this compatibility claim.
