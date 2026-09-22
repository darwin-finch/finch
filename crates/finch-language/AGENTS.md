# language capsule: compilation facade and orchestrator

Supplements the root [`AGENTS.md`](../../CLAUDE.md), which still applies in full.

**Owns** `crates/finch-language/src/`: source-language selection, the shared
compiler pipeline that returns `ModuleVerified`, and the compact-wire grammar
generated from the CoLisp and Co-Forth reader lexicons. It owns no interpreter,
runtime, fiber scheduler, capability grants, or checkpoint codec.

**Boundary:** the [README](README.md) traces source-only corpus checking and executable
compilation; [`src/lib.rs`](src/lib.rs) is the flat facade. `cargo doc -p finch-language --no-deps
--open` renders public methods. Application callers compile here, then submit the certificate to
`finch-vm`. Do not recreate a generated signature catalog.

**Dependencies:** `finch-language` depends on `finch-vm-core`, `finch-colisp`,
and `finch-coforth`. It never depends on `finch-vm` or the root `finch` crate.

**Invariants:** only `ModuleVerified` leaves this crate toward execution. Frontend
selection happens here, not in the VM. Grants, approval policy, and authorization
ledgers are not consulted during compilation. The published wire GBNF is generated
from reader lexicons and is a complete-response recognizer, not a proof of types,
vocabulary, capabilities, or host-effect safety.

**Surface tiers (issue #960 semantic LSP audit).** The `pub use` list in
[`src/lib.rs`](src/lib.rs) is the cross-crate contract; everything below it is tiered so
implementation detail cannot leak back in:

- **Crate-internal (`pub(crate)`):** the wire helpers in [`src/wire.rs`](src/wire.rs) —
  `accepts_wire_source`, `accepts_published_grammar`, `render_wire_gbnf`, `WireReject`,
  `WIRE_GRAMMAR_VERSION`, and `WIRE_GRAMMAR_ARTIFACT`. They are in-crate oracles for the
  published artifact, not a wire ABI: no vocabulary contract document names them and no
  external crate calls them. `accepts_wire_source` and `accepts_published_grammar` carry a
  scoped `cfg_attr(not(test), allow(dead_code))` because only the wire conformance tests call
  them today; widening any of these is a capsule change, not cleanup.
- **Wire surface:** the committed artifact `vocabulary/language/wire.gbnf`, its generator
  output (byte-conformance drift test), and the reader-oracle agreement tests. Helper
  visibility is not part of that contract; the artifact bytes are.
- **Deferred deletions (defining-crate passes):** `ModuleSealed::module` and
  `SemanticBuilder::finish_closed` (finch-vm-core, issue #964, the vm-core public-surface
  tightening audit). Zero references through this facade; methods cannot be removed from a
  re-exported type from this crate, so they disappear when their defining crates drop them.
  The finch-colisp `Val::{as_float, as_str, as_list}` accessors already left this category:
  their defining crate deleted them unreferenced in the issue-#961 colisp surface-tiers pass
  (PR #1103), so nothing pending remains from finch-colisp.

**Focused tests:** `./scripts/test_brains.sh cargo test -p finch-language --lib`.
Include `wire` when changing readers or the published grammar. Execution
equivalence belongs to `finch-vm`, where an interpreter is available without
creating a dependency cycle.
