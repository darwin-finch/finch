# runtime capsule: program runtime service and host effects

Supplements the root [`AGENTS.md`](../../CLAUDE.md), which still applies in full.

**Owns** `crates/finch-runtime/src/` (typed program execution, capability/authority binding, host-effect
audit, automation broker, archive/authority persistence, the child-agent vocabulary, and the
versioned Runtime/Application ABI and its Cap'n Proto checkpoint/frame codec: `ProgramRun`,
diagnostics, `VmSideEffect` envelopes,
`VmResume`, output-handle refs, and `VmEffectDeliveryLog` Brain/client identity ports), plus
the adjacent root `poset` tree and composition adapter [`src/program_registry.rs`](../../src/program_registry.rs)
listed on the DESIGN.md runtime row. Those adjacent trees are not this facade; this capsule is
`crates/finch-runtime/` only. Live attached-console streaming is issue #57 and is not this crate.

The Runtime/Application ABI is a **development seam, not a production compatibility promise**.
Version 1 may still change when justified: bump `RUNTIME_APPLICATION_ABI_VERSION` and fail
closed. Do not grow a second durable journal, external client schema, or cache keyed on
these types as if they were frozen.

**Interface:** [`INTERFACE.md`](INTERFACE.md) lists every exported item with its signature. Child
modules are private, so the `pub use` list in `src/lib.rs` is the whole public surface.
Root callers use `crate::runtime::Item` through the compatibility re-export and direct dependents
use `finch_runtime::Item`; neither may name `abi`,
`agent_vm`, `agents`, `archive_store`, `automation`, `context`, `effect_audit`, `effect_log`,
`host`, `hostio`, `ipc_codec`, `mcp`, `outcome`, or `workbook`.

**Dependencies:** the extracted `finch-vm`, `finch-language`, `finch-programs`, `finch-memory`,
`finch-ipc`, and `finch-tools-api` crates. MCP and artifact-proposal transports
are application-injected ports; workbook allocation bounds are owned here and exposed as flat
facade items for the CLI preview.
Production code under `crates/finch-runtime/src/**` must not depend on Brain. Brain → runtime is the
intended direction (runtime layer 2, brain layer 4). Delivery identity is an embedder-neutral
port (`DeliveryConsumerIdentity`); Brain implements it.

Configuration and host bindings are not proof of execution-policy conformance. Do not change
program execution, capability grants, effect-audit transitions, or automation availability while
maintaining this crate boundary.

**Focused tests:** `cargo test -p finch-runtime --lib` plus root caller smokes for `scheduler::` and
`poset::`. Run the full supervised workspace suite when changing a re-exported `pub` item. Regenerate
the facade digest with `python3 scripts/generate_interfaces.py --write` whenever the public
surface changes.
