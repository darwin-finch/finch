# ipc capsule: Cap'n Proto CLI ↔ daemon transport

Supplements the root [`AGENTS.md`](../../CLAUDE.md), which still applies in full.

**Owns** `src/ipc/`: the Unix-socket Cap'n Proto client and server, the event
bus, socket path helpers, and the generated schema re-export. Brain remote
envelopes and typed-runtime checkpoint framing live in the nested
[`codec`](codec/AGENTS.md) capsule.

**Interface:** `codec` is private, so callers outside this directory use
`crate::ipc::Item` and must not name `ipc::codec`, `ipc::brain_codec`, or
`ipc::checkpoint_codec`. `client`, `server`, `events`, `transport`, and
`schema` remain named modules until a later ipc facade cut; do not treat this
commit as that cut. `mod.rs`'s re-exports are the whole public surface — read it directly for
exact signatures.

**Dependencies:** intended edges are runtime, VM, and providers for values the
codecs translate. `ipc → brain` and `ipc → server` are existing
against-direction debt. Do not close those cycles here, and do not rewrite
`BrainStore` or HTTP handlers except for `use` path updates required by the
codec facade.

**Schema vs codec.** `schema.rs` re-exports the capnpc output from the crate
root so RPC method types resolve. It is IPC-owned. Codec-owned code consumes
that schema; it does not generate or own it.

**Wire behavior is not a facade concern.** `IPC_PROTOCOL_VERSION` and packed
Runtime/Application envelopes stay as they are. Changing framing belongs to
the codec capsule with a byte-level regression, not to a re-export commit.
Leftover-daemon health advertisement (`leftover_daemon_message`,
`protocol_generation_from_health_json`) lives on this facade so HTTP 200 is
not treated as compatibility.

**Focused tests:** `./scripts/test_brains.sh cargo test --lib -- ipc::`.
