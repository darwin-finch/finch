# node: local node identity

Owns the local node's identity, its TLS material, immutable capability reporting, and the work
statistics advertised to Brain and server callers. It exists as its own facade because identity
and TLS material are security-sensitive and narrowly scoped — this module receives model
capability facts as input and must never itself inspect, load, or schedule model implementations.

Ownership, dependencies, invariants, and test commands are in [`AGENTS.md`](AGENTS.md).
