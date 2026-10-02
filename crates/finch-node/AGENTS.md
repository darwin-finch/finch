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
- **Narrowed dead public surface (#986 bounded pass).** A 2026-09-20 LSP audit listed seven `pub`
  items with zero external semantic callers. PR #1364 (merged 2026-09-27) narrowed all seven to
  `pub(crate)` — `NodeCapabilities::is_cloud_only`, `NodeIdentity::device_uuid`, `WorkStats::new`,
  `WorkStats::{started_at, last_query_at}`, `collect_machine_specs`, `node_name::load_or_create` —
  and removed the two now-dead root compatibility re-exports that pointed at
  `collect_machine_specs` and `load_or_create` (`src/node/mod.rs`, `src/node_name.rs`). A
  2026-09-29 re-verification (whole-workspace grep, not just LSP) confirmed all seven remain
  internal-only on current `main` and added the serde round-trip regression the issue asked for
  but #1364 didn't include: `test_work_stats_serde_wire_format_is_stable_across_the_visibility_narrowing`
  in `src/stats.rs` pins that narrowing `started_at`/`last_query_at` from `pub` to `pub(crate)`
  left the JSON wire shape (field names and encoding) unchanged, and that a `work_stats.json`
  written by a pre-narrowing (fields still `pub`) build still deserializes.
- **`WorkTracker::persist` — deleted in #1364, open design question referred to #986.** The same
  audit found `persist` had zero references anywhere, including this crate's own tests, and no
  trait/dynamic-dispatch path. #1364 deleted it as a unilateral judgment call. Whether that was
  right is a separate, still-open question: production calls `WorkTracker::load_persisted`
  (`src/server/handlers/node.rs`'s `GET /v1/node/stats`), but no production path has ever called
  `WorkTracker::new()` or `record_query` outside this crate's own tests — not even in the commit
  that introduced this file (`33fdb6b0`, "distributed worker network foundation"). That means the
  write side was never wired in production at any point in this repo's history, and
  `GET /v1/node/stats` has always returned `WorkStats::new()`/zero defaults rather than real
  cumulative totals. See the open comment on issue #986 for the full write-up of both directions
  (delete as genuinely-always-dead speculative infra vs. wire up the stated "future worker-network
  reputation system" — see this file's own header comment in `stats.rs`); not decided here.

## Invariants

- TLS material derived from a signing identity keeps its certificate and private key paired.
- Capability reports describe supplied facts; constructing one performs no model loading.
- Statistics updates remain bounded to the node-owned accounting types.
- TLS and invitation policy changes are outside this subsystem boundary and require their own
  security-sensitive work.

## Focused tests

Run through the repository supervisor and Cargo slot:

```bash
scripts/factory/with-cargo-slot ./scripts/test_brains.sh cargo test -p finch-node --lib
scripts/factory/with-cargo-slot ./scripts/test_brains.sh cargo test --lib brain::
scripts/factory/with-cargo-slot ./scripts/test_brains.sh cargo test --lib server::
```

Use the smallest matching filter first. The [README](README.md) traces Brain and server callers;
[`src/lib.rs`](src/lib.rs) is the facade, and `cargo doc -p finch-node --no-deps --open`
renders public methods on re-exported types.
