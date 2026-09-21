# finch-node capsule: local transport identity and node metadata

Supplements the root [`AGENTS.md`](../../CLAUDE.md), which still applies in full.

`crates/finch-node` owns the local node identity, its TLS material, persisted machine name,
immutable capability reporting, and the work statistics advertised to Brain and server callers.
Its public contract is the facade in `src/lib.rs`; callers must not name child modules.

## Boundary

- It has no Finch subsystem dependencies. The persisted machine name moved into this crate with
  the identity that consumes it.
- Receives model capability facts as immutable input. It must not inspect, load, or schedule model
  implementations.
- Must not own distributed protocol, invitation, scheduler, Brain/server, or model-loading policy.
- `identity.rs`, `node_name.rs`, `stats.rs`, and `tls.rs` are private implementation modules. Add
  public surface by re-exporting it from `src/lib.rs`; do not regenerate a signature catalog.

## Invariants

- TLS material derived from a signing identity keeps its certificate and private key paired.
- Capability reports describe supplied facts; constructing one performs no model loading.
- Statistics updates remain bounded to the node-owned accounting types.
- TLS and invitation policy changes are outside this subsystem boundary and require their own
  security-sensitive work.

## Focused tests

Run through the repository supervisor and Cargo slot:

```bash
.agents/skills/finch-backlog/scripts/with-cargo-slot ./scripts/test_brains.sh cargo test -p finch-node --lib
.agents/skills/finch-backlog/scripts/with-cargo-slot ./scripts/test_brains.sh cargo test --lib brain::
.agents/skills/finch-backlog/scripts/with-cargo-slot ./scripts/test_brains.sh cargo test --lib server::
```

Use the smallest matching filter first. The [README](README.md) traces Brain and server callers;
[`src/lib.rs`](src/lib.rs) is the facade, and `cargo doc -p finch-node --no-deps --open`
renders public methods on re-exported types.
