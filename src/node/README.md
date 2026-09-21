# Root node compatibility path

`src/node` keeps the old `finch::node` import path while `finch-node` owns identity, TLS material,
capability descriptions, and work statistics. This directory contains no implementation,
persistence policy, or protocol. The root package decides what local facts to supply; the crate
defines and stores node-owned facts. New lower-layer code should use the crate facade directly.

For `finch node-info`, `src/main.rs` selects the current model and RAM facts, constructs
`NodeCapabilities`, and calls `NodeInfo::load` through this compatibility path. The command owns
the displayed text and provider-configuration decision; the node crate owns durable identity and
the resulting description.

For `/v1/node/info` and `/v1/node/stats`, `src/server/handlers/node.rs` supplies host/model facts
to `NodeInfo` and loads `WorkTracker` statistics through the same root path. The server owns HTTP
shape and status; the node crate owns identity material and counters. Isolation tests use the
Unix-only disposable-state exports rather than the real user state directory.

Read [AGENTS.md](AGENTS.md) for extension rules, [mod.rs](mod.rs) for the exact alias surface,
and the [finch-node README](../../crates/finch-node/README.md) for the owning crate contract.
