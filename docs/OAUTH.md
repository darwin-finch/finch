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
  dialect and client identity pinned by `CHATGPT_OAUTH_PROTOCOL_REVISION`.
- Finch owns this implementation and its descriptor-anchored credential store,
  but it is not registered as an independent OpenAI OAuth client. The client
  identity and consumer contract are derived from [OpenAI Codex commit
  `94cbbddafc1776d5e377bca1b05932c697e82238`](https://github.com/openai/codex/commit/94cbbddafc1776d5e377bca1b05932c697e82238),
  recorded in source as
  `openai-codex-public-client@94cbbddafc1776d5e377bca1b05932c697e82238+finch-binding-v2`.
  They remain explicit compatibility risks rather than an OpenAI-supported
  third-party integration.
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

The active device-login compatibility dialect uses the public client identifier
recorded by that pinned Codex source for device authorization, token exchange,
refresh, and revocation. That is disclosure of the compatibility dependency,
not a claim that OpenAI independently registered or supports Finch as an OAuth
client.

Earlier browser-PKCE research was pinned to [Codex commit
`3e4707b34b16e139fcb7ad11ab8445993b62bba1`](https://github.com/openai/codex/commit/3e4707b34b16e139fcb7ad11ab8445993b62bba1),
specifically the login files under
`codex-rs/login/`. That browser flow uses the Codex-only `codex_cli_rs`
originator. Finch keeps browser PKCE disabled rather than impersonating that
originator; the provider-neutral core and synthetic dialects still exercise
browser PKCE, state, and nonce.

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
For multiple new accounts, each successful issue returns an opaque
generation-bound compensation handle from the same atomic store commit. A
later denial, cancellation, or invalid account locally tombstones only those
exact generations before returning; a concurrent replacement fails the CAS and
is left untouched. Restart can then resume a safely tombstoned transaction.

Browser PKCE remains disabled because the pinned browser protocol uses a
Codex-only originator that Finch does not impersonate. Windows token persistence
also remains fail-closed pending a descriptor/handle-anchored implementation;
CI compiles the exact provider-neutral and verifier sources there.

Current source constructs the credential-bound ChatGPT subscription provider
and implements its catalog, inference, streaming, allowance, and provenance
path; see [the native transport contract](CHATGPT_SUBSCRIPTION_TRANSPORT.md).
The transport remains experimental and version-pinned. [User dogfood on
2026-09-05 after the collaboration namespace fix](https://github.com/darwin-finch/finch/issues/180#issuecomment-5557196748)
exercised three local `spawn_agent` calls and a final typed Lisp response through
a fresh Finch session. That evidence is limited to the tested account and date;
live authorization is never part of ordinary automated tests. These are
current-source claims, not evidence that an older packaged release contains the
transport.
