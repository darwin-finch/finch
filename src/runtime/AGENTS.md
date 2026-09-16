# runtime capsule: program runtime service and host effects

Supplements the root [`AGENTS.md`](../../CLAUDE.md), which still applies in full.

**Owns** `src/runtime/` (typed program execution, capability/authority binding, host-effect
audit, automation broker, archive/authority persistence, the child-agent vocabulary, and the
versioned Runtime/Application ABI: `ProgramRun`, diagnostics, `VmSideEffect` envelopes,
`VmResume`, output-handle refs, and `VmEffectDeliveryLog` Brain/client identity ports), plus
the adjacent `poset` tree and composition adapter [`src/program_registry.rs`](../program_registry.rs)
listed on the DESIGN.md runtime row. Those adjacent trees are not this facade; this capsule is
`src/runtime/` only. Live attached-console streaming is issue #57 and is not this module.

The Runtime/Application ABI is a **development seam, not a production compatibility promise**.
Version 1 may still change when justified: bump `RUNTIME_APPLICATION_ABI_VERSION` and fail
closed. Do not grow a second durable journal, external client schema, or cache keyed on
these types as if they were frozen.

**Interface:** [`INTERFACE.md`](INTERFACE.md) lists every exported item with its signature. Child
modules are private, so the `pub use` list in `src/runtime/mod.rs` is the whole public surface.
Callers outside this directory use `crate::runtime::Item`; they must not name `abi`,
`agent_vm`, `agents`, `archive_store`, `automation`, `context`, `effect_audit`, `effect_log`,
`outcome`, `host`, `hostio`, or `mcp`.

**Dependencies:** `vm` (capability types and the typed machine), `programs` (language, values,
execution effect), `tools` (MCP client binding), and `memory` (optional MemTree binding).
Production code under `src/runtime/**` must not name `crate::brain`. Brain → runtime is the
intended direction (runtime layer 2, brain layer 4). Delivery identity is an embedder-neutral
port (`DeliveryConsumerIdentity`); Brain implements it. Do not extract `finch-runtime` until
remaining edges are measured and that reverse edge stays gone.

**Scheduling, capabilities, and effect audit are not this commit.** Configuration and host
bindings are not proof of execution-policy conformance. Do not change program execution,
capability grants, effect-audit transitions, or automation availability in a facade commit.

**Focused tests:** `./scripts/test_brains.sh cargo test --lib -- runtime::` plus a caller smoke
(`scheduler::`, `poset::`). Run the full suite when changing a re-exported `pub` item. Regenerate
the facade digest with `python3 scripts/generate_interfaces.py --write` whenever the public
surface changes.
