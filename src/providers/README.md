# providers: Finch Config mapping onto finch-providers

This is a thin compatibility facade: it maps Finch's application `Config` (`ProviderEntry`,
`TeacherEntry`) onto [`finch-providers`](../../crates/finch-providers/README.md) constructors and
re-exports its types. It exists so that Config-taking factory/catalog logic is separated from the
provider-neutral crate — the crate never knows about Finch's configuration format, and this facade
never implements a dialect or adapter itself.

Ownership, dependencies, invariants, and test commands are in [`AGENTS.md`](AGENTS.md).

## Further documentation

[`../claude/CLIENT.md`](../claude/CLIENT.md) — the Claude HTTP client (now wraps any default
provider, despite the name; `DESIGN.md` flags this doc as describing a Claude-only client).
