# language capsule: compilation facade and orchestrator

Supplements the root [`AGENTS.md`](../../CLAUDE.md), which still applies in full.

**Owns** `crates/finch-language/src/`: source-language selection, the shared
compiler pipeline that returns `ModuleVerified`, and the compact-wire grammar
generated from the CoLisp and Co-Forth reader lexicons. It owns no interpreter,
runtime, fiber scheduler, capability grants, or checkpoint codec.

**Interface:** `src/lib.rs` is the facade; read it directly for exact signatures.
Application callers compile here, then submit the certificate to `finch-vm`.

**Documentation:** [`README.md`](README.md) owns the implemented
compilation-facade contract. Cross-frontend planned semantics remain in the
shared [language design](../../docs/language/README.md); boundary changes follow
the [implementation roadmap](../../docs/language/IMPLEMENTATION_ROADMAP.md).

**Dependencies:** `finch-language` depends on `finch-vm-core`, `finch-colisp`,
and `finch-coforth`. It never depends on `finch-vm` or the root `finch` crate.

**Invariants:** only `ModuleVerified` leaves this crate toward execution. Frontend
selection happens here, not in the VM. Grants, approval policy, and authorization
ledgers are not consulted during compilation. The published wire GBNF is generated
from reader lexicons and is a complete-response recognizer, not a proof of types,
vocabulary, capabilities, or host-effect safety.

**Focused tests:** `./scripts/test_brains.sh cargo test -p finch-language --lib`.
Include `wire` when changing readers or the published grammar. Execution
equivalence belongs to `finch-vm`, where an interpreter is available without
creating a dependency cycle.
