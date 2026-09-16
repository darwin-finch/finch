# ipc/codec capsule: Cap'n Proto Brain and checkpoint framing

Supplements the root [`AGENTS.md`](../../../CLAUDE.md) and the parent
[`ipc` capsule](../AGENTS.md), which still apply in full.

**Owns** `src/ipc/codec/`: the Brain remote-envelope codec (`brain_codec`) and
the closed typed-runtime checkpoint codec (`checkpoint_codec`). These files
translate Brain, runtime, and VM domain types into the generated Cap'n Proto
schema and back. They do not own RPC dispatch, socket transport, or the
generated schema module — `schema.rs` stays on the parent because client and
server use it for method surfaces, not only for these translations.

This is a sub-subsystem of `ipc`. Nesting here is the same shape as
`tools/mcp`: a directory with its own capsule inside another.

**Interface:** [`INTERFACE.md`](INTERFACE.md) lists every item the codec
facade re-exports. Child modules stay private. Callers outside `src/ipc` use
`crate::ipc::Item`; they must not name `ipc::codec`, `ipc::brain_codec`, or
`ipc::checkpoint_codec`. Sibling IPC modules (`client`, `server`) may use
`crate::ipc::codec::` for encode/decode helpers that are not on the parent
facade.

**Dependencies:** Brain, runtime, VM, providers, and `server::RunnerEffectRecord`
are existing against-direction edges. Do not close the `ipc → brain` or
`ipc → server` cycles in this capsule. Add no new reverse edges.

**Wire bytes are the contract.** Encode/decode must stay byte-stable for a
given schema generation. Do not change discriminants, packing, trailing-byte
rejection, or nesting limits in a facade-only commit.

**Focused tests:** `./scripts/test_brains.sh cargo test --lib -- ipc::codec::`.
Parent `ipc::` tests cover the facade leak ratchet and pinned Detach/checkpoint
framing.
