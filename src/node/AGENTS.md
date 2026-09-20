# Node subsystem

Supplements the root [`AGENTS.md`](../../CLAUDE.md), which still applies in full.

`src/node` owns the local node identity, its TLS material, immutable capability reporting, and
the work statistics advertised to Brain and server callers. Its public contract is the facade in
`mod.rs`; callers must not name child modules.

## Boundary

- Its only Finch subsystem dependency is the persisted machine name in `crate::node_name`.
- Receives model capability facts as immutable input. It must not inspect, load, or schedule model
  implementations.
- Must not own distributed protocol, invitation, scheduler, Brain/server, or model-loading policy.
- `identity.rs`, `stats.rs`, and `tls.rs` are private implementation modules. Add public surface by
  re-exporting it from `mod.rs`.

## Invariants

- TLS material derived from a signing identity keeps its certificate and private key paired.
- Capability reports describe supplied facts; constructing one performs no model loading.
- Statistics updates remain bounded to the node-owned accounting types.
- TLS and invitation policy changes are outside this subsystem boundary and require their own
  security-sensitive work.

## Focused tests

Run through the repository supervisor with a worktree-specific absolute Cargo target directory:

```bash
./scripts/test_brains.sh cargo test --lib node::
./scripts/test_brains.sh cargo test --lib brain::
./scripts/test_brains.sh cargo test --lib server::
```

Use the smallest matching filter first. `mod.rs`'s re-exports are the whole public surface — read
it directly for exact signatures.
