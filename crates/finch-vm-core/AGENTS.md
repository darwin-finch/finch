# vm-core capsule: shared typed machine contract

Supplements the root [`AGENTS.md`](../../CLAUDE.md), which still applies in full.

**Owns** `crates/finch-vm-core/src/`: the provider-neutral typed IR, types, signatures,
diagnostics, capability/effect descriptions, verifier, language identity, and core vocabulary.
It owns no frontend, interpreter, runtime, fiber scheduler, or checkpoint codec.

**Interface:** [`INTERFACE.md`](INTERFACE.md) is generated from `src/lib.rs`. Application callers
continue to use `finch-vm`; the five explicitly documented compiler-support exports (`BlockId`,
`nearest_names`, `apply_signature_types`, `instantiate_signature_types`, and `parse_type_name`)
are an intentionally restricted workspace seam and are not stable application API.

**Dependencies:** this unpublished foundation crate depends only on `anyhow`, `once_cell`, `serde`,
`serde_json`, `thiserror`, and `uuid`. It never depends on `finch-vm` or the root `finch` crate.

**Invariants:** `VM_TYPE_SYSTEM_VERSION` remains the version of serialized IR and typed runtime
checkpoints. Moving a contract here must not change its serde representation, authority semantics,
or vocabulary entry.

**Focused tests:** `./scripts/test_brains.sh cargo test -p finch-vm-core --lib`. Also run the
`finch-vm` serialization compatibility and frontend equivalence integration tests for boundary
changes.
