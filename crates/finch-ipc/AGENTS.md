# ipc capsule: domain-neutral Cap'n Proto transport core

Supplements the root [`AGENTS.md`](../../CLAUDE.md), which still applies in full.

**Owns** `crates/finch-ipc/`: the generated Cap'n Proto namespace facade, protocol-generation and health
helpers, event bus, JSON-value wire translation, and socket-path helpers. The frontend application
client adapter lives in `src/client`; the daemon RPC implementation, listener lifecycle, dispatch,
and authority enforcement live in `src/server`. Brain-envelope translation lives with Brain, and
typed-runtime checkpoint/frame translation lives with runtime.

**Interface:** [`INTERFACE.md`](INTERFACE.md) is the flat public surface. Child implementation
modules are private; callers must not name `ipc::events`, `ipc::transport`, or `ipc::value_codec`.

**Dependencies:** this core is domain-neutral. It must not depend on Brain, server, runtime,
client, CLI, scheduler, generators, providers, tools, programs, or VM.

**Schema:** this crate owns the unchanged schema, build script, and generated include. Do not change
schema, discriminants, packing, nesting limits, protocol generation, or wire bytes in a
boundary-only change.

**Focused tests:** `cargo test -p finch-ipc`. Regenerate the facade
digest with `python3 scripts/generate_interfaces.py --write` whenever the public surface changes.
