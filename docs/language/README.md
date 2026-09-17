# Finch language documentation

This directory owns design that crosses the CoLisp frontend, Co-Forth frontend, shared typed IR,
and VM runtime. The current canonical design is the
[Finch language design](FINCH_LANGUAGE_DESIGN.md). It is intended direction, not evidence that
every described feature is implemented.

The [language implementation roadmap](IMPLEMENTATION_ROADMAP.md) maps that design onto the current
workspace, staged issues, integration gates, and the bounded file set for a language-focused agent.

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
per feature. As sections become stable and implemented, extract their reference material from the
design into the appropriate crate document and replace detailed implementation notes with links.
Keep design rationale here and implementation status in the roadmap rather than mixing either with
current API evidence.
