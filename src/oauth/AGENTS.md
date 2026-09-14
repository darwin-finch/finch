# oauth capsule: provider-neutral OAuth and credential persistence

Supplements the root [`AGENTS.md`](../../CLAUDE.md), which still applies in full.

`src/oauth` owns RFC 8628 device authorization, authorization-code with S256 PKCE, caller-owned
cancellation, token refresh and revocation, crash-safe generation-checked persistence, and the
secret-free named-credential projection. Provider dialects, ChatGPT/Claude product authentication,
and browser UX live outside this subtree.

## Boundary

- The public contract is the facade in `mod.rs`. Callers must not name child modules.
- `file_store` is a private `OAuthCredentialStore` implementation. Construct
  `FileOAuthCredentialStore` from the root export.
- The only Finch subsystem dependency is `config`'s credential types: `AudienceBinding`,
  `CredentialKind`, `CredentialLifecycle`, `CredentialProvider`, `CredentialResolver`,
  `ProviderCredential`, `ResolvedCredential`, and `ResolvedSecret`. Add no other `config`
  imports. Provider dialects stay in `src/providers`.
- Do not extract `finch-oauth` until that credential surface is a crate-level contract rather
  than `crate::config` types. The facade is the ownership boundary; a crate would currently
  copy credential types or take a wide config dependency without narrowing the contract.

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
./scripts/test_brains.sh cargo test --lib -- oauth::
```

Run provider and CLI ChatGPT tests when changing a re-exported `pub` item, because those callers
construct `FileOAuthCredentialStore` and drive login, refresh, recovery, and logout. Regenerate
the facade digest with `python3 scripts/generate_interfaces.py --write` whenever the public
surface changes.
