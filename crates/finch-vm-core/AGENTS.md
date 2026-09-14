# vm-core capsule: shared typed machine contract

Supplements the root [`AGENTS.md`](../../CLAUDE.md), which still applies in full.

**Owns** `crates/finch-vm-core/src/`: the provider-neutral typed IR, types, signatures,
diagnostics, capability/effect descriptions, verifier, language identity, core vocabulary,
and the syntax-neutral semantic-construction protocol with opaque phase types.
It owns no frontend, interpreter, runtime, fiber scheduler, or checkpoint codec.
Capability *requirements* live here as typed effects; grants, approval policy, and
authorization ledgers remain physically in this crate until application-runtime
extraction, and are not consulted during compilation.

**Interface:** [`INTERFACE.md`](INTERFACE.md) is generated from `src/lib.rs`. Application callers
continue to use `finch-vm`; the compiler-support exports (`BlockId`, `nearest_names`,
`apply_signature_types`, `instantiate_signature_types`, `parse_type_name`, `SemanticBuilder`,
`Parsed`, `Elaborated`, `FunctionCertified`, `ModuleSealed`, and `ModuleVerified`)
are an intentionally restricted workspace seam and are not stable application API.

**Documentation:** [`docs/README.md`](docs/README.md) owns implemented IR/verifier reference
material for this crate. Cross-frontend planned semantics remain in the shared
[language design](../../docs/language/README.md); boundary changes follow the shared
[implementation roadmap](../../docs/language/IMPLEMENTATION_ROADMAP.md).

**Dependencies:** this unpublished foundation crate depends only on `anyhow`, `once_cell`, `serde`,
`serde_json`, `thiserror`, and `uuid`. It never depends on `finch-vm` or the root `finch` crate.

**Invariants:** `VM_TYPE_SYSTEM_VERSION` remains the version of serialized IR and typed runtime
checkpoints. Moving a contract here must not change its serde representation, authority semantics,
or vocabulary entry.

**Focused tests:** `./scripts/test_brains.sh cargo test -p finch-vm-core --lib`. Also run the
`finch-vm` serialization compatibility and frontend equivalence integration tests for boundary
changes.
