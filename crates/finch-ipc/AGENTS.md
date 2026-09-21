# ipc capsule: domain-neutral Cap'n Proto transport core

Supplements the root [`AGENTS.md`](../../CLAUDE.md), which still applies in full.

**Owns** `crates/finch-ipc/`: the generated Cap'n Proto namespace facade, protocol-generation and health
helpers, event bus, JSON-value wire translation, and socket-path helpers. The frontend application
client adapter lives in `src/client`; the daemon RPC implementation, listener lifecycle, dispatch,
and authority enforcement live in `src/server`. Brain-envelope translation lives with Brain, and
typed-runtime checkpoint/frame translation lives with runtime.

**Boundary:** the [README](README.md) traces client and server callers; [`src/lib.rs`](src/lib.rs)
is the facade. `cargo doc -p finch-ipc --no-deps --open` shows public signatures. Handwritten
child modules stay private. The generated `finch_ipc_capnp` namespace is the deliberate
exception: generated self-references require it at the crate root. Callers must not name the
private `events`, `transport`, or `value_codec` modules. Do not recreate a signature catalog.

**Dependencies:** this core is domain-neutral. It must not depend on Brain, server, runtime,
client, CLI, scheduler, generators, providers, tools, programs, or VM.

**Schema:** this crate owns the unchanged schema, build script, and generated include. Do not change
schema, discriminants, packing, nesting limits, protocol generation, or wire bytes in a
boundary-only change.

**Focused tests:** `./scripts/test_brains.sh cargo test -p finch-ipc --lib`. Run supervised
client/server integration tests if the schema, version, or wire codec changes.
