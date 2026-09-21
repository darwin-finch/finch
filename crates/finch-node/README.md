# Finch node identity and facts

This crate owns the durable local node identity and the facts a node can advertise: its signing
and TLS material, machine name, supplied capabilities, and work statistics. It does not decide
who may join a Brain, load a model, select a provider, or serve HTTP. Those policies belong to
the callers that have the necessary application context.

When a Brain credential authority starts, `crates/finch-brain/src/credential.rs` loads a
`NodeSigningIdentity` from its private state directory and derives a `NodeTlsIdentity` from it.
Brain owns invitation signing, redemption, and revocation; the node crate owns the identity
material and the certificate/key pairing.

When the server answers node-info and node-stats requests, `src/server/handlers/node.rs` gathers
model and host facts, supplies them to `NodeCapabilities`, and reads persisted `WorkTracker`
statistics. The server owns endpoint behavior and model selection; this crate describes and
persists the node facts it receives.

Read [AGENTS.md](AGENTS.md) for boundary and testing rules, [src/lib.rs](src/lib.rs) for the
facade, and `cargo doc -p finch-node --no-deps --open` for methods on exported types.
