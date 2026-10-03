# Finch language documentation

This directory owns language contracts that cross the CoLisp frontend, Co-Forth frontend, shared
typed IR, and VM runtime. The [Finch language specification](SPECIFICATION.md) is normative where
it makes a decision; unresolved specification findings are tracked in the adversarial
[design review](DESIGN_REVIEW.md), while implementation blockers and proof gates are tracked in the
[implementation roadmap](IMPLEMENTATION_ROADMAP.md). The [language design](FINCH_LANGUAGE_DESIGN.md) retains rationale
and decision history. None of these documents is evidence that a described feature is implemented.

The [language implementation plan](IMPLEMENTATION_ROADMAP.md) maps the specification onto the
current workspace, staged issues, integration gates, and the bounded file set for a language-focused
agent. The generated machine-readable [required prelude](spec-prelude.json) freezes names,
structural callable/concept contracts, and named trusted-law expression ASTs used by version 0.1.
An independent consumer does not yet execute this AST, so the artifact is not yet a complete
semantic oracle.

The executable closure artifacts are the three frontend [grammar](grammar/) files, which the checker
executes against an accept/reject [grammar corpus](fixtures/grammar-corpus.json); the canonical
[semantic-event schema](schemas/semantic-events.schema.json) and
[digest rules](semantics/canonical-digests.json); the
[transition rules](semantics/transitions.json), which a reference machine interprets directly to
produce the paired [execution vectors](fixtures/execution-vectors.json) and
[static rejections](fixtures/static-rejections.json), with
[checked transition coverage](semantics/transition-coverage.json); the
[typed stack IR, version 6](semantics/ir.json), to which every vector is lowered, verified, and
executed, and which must reproduce the reference machine's observable behaviour;
[canonical schema instances](fixtures/schema-instances.json),
and the [target](schemas/target-abi.schema.json) and
[native-call ABI](schemas/native-call-abi.schema.json) and
[portable message ABI](schemas/portable-message-abi.schema.json) schemas, plus the executable
[replay automaton](semantics/replay-automaton.json). Run
`python3 scripts/language/check_language_spec.py` from the repository root to validate them and
confirm that the checked-in prelude is current. Its isolated Python dependency is pinned in
`scripts/language/requirements.txt`.
Performance acceptance targets live in
[`performance-budgets.json`](semantics/performance-budgets.json); their presence is not benchmark
evidence until a dated measurement manifest satisfies them.

Documentation follows the same semantic waist as the workspace:

- Shared value, ownership, concept, effect, module, and source-to-IR rules stay here. No one crate
  can define them independently.
- `crates/finch-colisp/docs/` owns implemented CoLisp reader grammar and how that reader
  submits the shared construction protocol. It does not own typed-stack-IR lowering.
- `crates/finch-coforth/docs/` owns implemented Co-Forth reader grammar and the same
  construction-protocol submission. It does not own typed-stack-IR lowering.
- `vocabulary/language/wire.gbnf` is the published compact-wire complete-response
  grammar, generated from those reader lexicons. It is not the submission envelope
  (`schema.json`) and is not a proof of semantic safety.
- `crates/finch-vm-core/docs/` owns implemented typed-IR schema, verifier contracts, diagnostics,
  and versioning details.
- `crates/finch-vm/docs/` owns implemented interpreter, fiber, checkpoint, and execution semantics.

Create those crate-local documents when their subject has an implemented contract large enough to
need more than the crate's `AGENTS.md`; do not copy planned semantics into them prematurely. The
crate capsules remain the fastest authoritative map of current ownership, and generated
`INTERFACE.md` files remain the exact public Rust surfaces.

The long-term documentation split should remain coarse: one shared language specification, one
syntax reference per frontend, one IR/verifier reference, and one execution reference. Avoid a file
per feature. Keep design rationale in the design history, normative behavior in the specification,
and implementation status in the plan rather than mixing any of them with current API evidence.
