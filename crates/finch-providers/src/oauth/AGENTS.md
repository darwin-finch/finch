# oauth capsule: provider-neutral OAuth and credential persistence

Supplements the crate [`AGENTS.md`](../../AGENTS.md) and the root
[`AGENTS.md`](../../../../CLAUDE.md), which still apply in full.

`oauth` owns RFC 8628 device authorization, authorization-code with S256 PKCE,
caller-owned cancellation, token refresh and revocation, crash-safe
generation-checked persistence, and the secret-free named-credential projection.
Provider dialects stay in sibling adapter modules. HTTP, clock, and sleep are
injected through `ProviderPorts`.

## Boundary

- This module is private; its public contract is the flat re-export in the crate
  [`lib.rs`](../lib.rs) facade (issue #958). External callers must not name the `oauth` path at
  all, and callers inside the crate enter through the facade.
- The [README](README.md) traces ChatGPT and Grok CLI callers. rustdoc renders methods on the
  facade's exported types. Do not recreate a signature catalog.
- `file_store` is a private `OAuthCredentialStore` implementation. Construct
  `FileOAuthCredentialStore` from the facade export.
- Credential types are crate-level (`AudienceBinding`, `CredentialKind`,
  `CredentialLifecycle`, `CredentialProvider`, `CredentialResolver`,
  `ProviderCredential`, `ResolvedCredential`, `ResolvedSecret`). This module does
  not depend on Finch application `Config`.

## Invariants

- Dialects own every provider fact. The state machine knows no provider URL, client ID, scope,
  token shape, or account claim.
- Cancellation, expiry, and denial are terminal and must not persist tokens.
- Refresh writes `mutation_pending` before remote rotation. Interrupted mutations recover only as
  durable tombstones; they must not resurrect possibly rotated secrets.
- Unix file persistence is descriptor-anchored, `0700`/`0600`, generation-CAS, and fails closed on
  other platforms.
- Debug formatting never reveals secrets, correlation data, or token material.

## Focused tests

```bash
.agents/skills/finch-backlog/scripts/with-cargo-slot ./scripts/test_brains.sh cargo test -p finch-providers --lib -- oauth::
```
