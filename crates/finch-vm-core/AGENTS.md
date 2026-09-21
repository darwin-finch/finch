# vm-core capsule: shared typed machine contract

Supplements the root [`AGENTS.md`](../../CLAUDE.md), which still applies in full.

**Owns** `crates/finch-vm-core/src/`: the provider-neutral typed IR, types, signatures,
diagnostics, capability/effect descriptions, verifier, language identity, core vocabulary,
and the syntax-neutral semantic-construction protocol with opaque phase types.
It owns no frontend, interpreter, runtime, fiber scheduler, or checkpoint codec.
Capability *requirements* live here as typed effects; grants, approval policy, and
authorization ledgers remain physically in this crate until application-runtime
extraction, and are not consulted during compilation.

**Boundary:** the [README](README.md) explains two caller workflows; [`src/lib.rs`](src/lib.rs)
is the flat facade, and `cargo doc -p finch-vm-core --no-deps --open` renders public methods.
Application callers use `finch-vm`; compiler-support exports are an intentionally restricted
workspace seam, not a stable application API. Do not recreate a generated signature catalog.

**Dependencies:** this unpublished foundation crate depends only on `anyhow`, `once_cell`, `serde`,
`serde_json`, `thiserror`, and `uuid`. It never depends on `finch-vm` or the root `finch` crate.

**Invariants:** `VM_TYPE_SYSTEM_VERSION` remains the version of serialized IR and typed runtime
checkpoints. Structured `VmDiagnostic` records are part of the frozen Runtime/Application ABI
and must keep their serde representation. Moving a contract here must not change its serde
representation, authority semantics, or vocabulary entry.

**Focused tests:** `./scripts/test_brains.sh cargo test -p finch-vm-core --lib`. Also run the
`finch-vm` serialization compatibility and frontend equivalence integration tests for boundary
changes.
