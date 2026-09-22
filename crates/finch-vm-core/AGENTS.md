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

**Surface tiers (issue #964 audit).** The `pub use` list in [`src/lib.rs`](src/lib.rs) is the
cross-crate contract; method visibility below it is tiered so implementation detail cannot leak
back in. Widening any item below is a capsule change, not cleanup.

- **Crate-internal (`pub(crate)`):** the `certify_module` construction chain
  (`Elaborated::new`, `Elaborated::add_linked_function`, `Elaborated::seal`,
  `ModuleSealed::verify`), `GrantSet::revoke` (called by `CapabilityLedger::revoke`),
  `DiagnosticPhase::label` (feeds `VmDiagnostic` note formatting), and
  `FileSelector::contains_selector` (selector-algebra and grant-coverage checks).
- **Test-only (`#[cfg(test)]`):** `CapabilityLedger::grant_global`,
  `GrantSet::active_global_requirements`, `Elaborated::add_function` and its private helper
  `FunctionCertified::into_function`, `FunctionCertified::{certify, function, facts}`,
  `Parsed::{source_id, ast}`, `Verifier::certify_function` (its only caller,
  `FunctionCertified::certify`, is test-only, so the local-certification entry has no
  production caller), and `FileSelector::intersection` (`SelectorError` variants stay pub).
- **Contract (kept `pub` despite zero direct name references outside this crate):**
  `BoolBranch` names the public signatures of `SemanticBuilder::start_bool_branch` and
  `SemanticBuilder::jump_to_merge`, both with live callers in `finch-colisp`, which also
  reads its `pub` fields; `LoopBinding` is likewise externally constructed.

Deleted as unreferenced by the same audit (including the two items deferred to this pass from
the #960 review): `ModuleSealed::module`, `SemanticBuilder::finish_closed`, `SourceSpan::bytes`.

**Dependencies:** this unpublished foundation crate depends only on `anyhow`, `once_cell`, `serde`,
`serde_json`, `thiserror`, and `uuid`. It never depends on `finch-vm` or the root `finch` crate.

**Invariants:** `VM_TYPE_SYSTEM_VERSION` remains the version of serialized IR and typed runtime
checkpoints. Structured `VmDiagnostic` records are part of the frozen Runtime/Application ABI
and must keep their serde representation. Moving a contract here must not change its serde
representation, authority semantics, or vocabulary entry.

**Focused tests:** `./scripts/test_brains.sh cargo test -p finch-vm-core --lib`. Also run the
`finch-vm` serialization compatibility and frontend equivalence integration tests for boundary
changes.
