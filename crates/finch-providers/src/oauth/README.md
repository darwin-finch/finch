# OAuth: provider-neutral device and PKCE flows

This module implements OAuth once for every provider that needs it: RFC 8628 device
authorization, authorization-code with S256 PKCE, token refresh and revocation, and crash-safe
credential persistence. It exists so that adding a new OAuth-based provider means writing a small
dialect (its URL, client ID, scopes, token shape) rather than a second state machine — the state
machine itself knows no provider-specific facts.

Ownership, dependencies, invariants, and test commands are in [`AGENTS.md`](AGENTS.md).

## Scope

Provider dialects stay in sibling adapter modules; HTTP, clock, and sleep are injected through
`ProviderPorts` so the flows are testable without real network calls or wall-clock waits. Finch
reaches this through the compatibility facade at [`src/oauth/AGENTS.md`](../../../../src/oauth/AGENTS.md).
