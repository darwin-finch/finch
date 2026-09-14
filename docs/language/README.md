# Finch language documentation

This directory owns design that crosses the CoLisp frontend, Co-Forth frontend, shared typed IR,
and VM runtime. The current canonical design is the
[typed Lisp/Forth capability and JIT plan](TYPED_LISP_FORTH_CAPABILITY_JIT_PLAN.md). It is intended
direction, not evidence that every described feature is implemented.

Documentation follows the same semantic waist as the workspace:

- Shared value, ownership, concept, effect, module, and source-to-IR rules stay here. No one crate
  can define them independently.
- `crates/finch-colisp/docs/` owns implemented CoLisp reader grammar and frontend lowering details.
- `crates/finch-coforth/docs/` owns implemented Co-Forth reader grammar and frontend lowering
  details.
- `crates/finch-vm-core/docs/` owns implemented typed-IR schema, verifier contracts, diagnostics,
  and versioning details.
- `crates/finch-vm/docs/` owns implemented interpreter, fiber, checkpoint, and execution semantics.

Create those crate-local documents when their subject has an implemented contract large enough to
need more than the crate's `AGENTS.md`; do not copy planned semantics into them prematurely. The
crate capsules remain the fastest authoritative map of current ownership, and generated
`INTERFACE.md` files remain the exact public Rust surfaces.

The long-term documentation split should remain coarse: one shared language specification, one
syntax reference per frontend, one IR/verifier reference, and one execution reference. Avoid a file
per feature. As sections become stable and implemented, extract them from the planning document
into the appropriate reference and replace them with links, leaving this plan as the roadmap and
cross-cutting rationale.
