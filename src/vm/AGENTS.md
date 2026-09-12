# vm capsule: typed VM, CoForth, CoLisp

Supplements the root [`AGENTS.md`](../../CLAUDE.md), which still applies in full.

**Owns** `src/vm/` (IR, verifier, interpreter, capabilities, effects, CoForth and CoLisp
frontends), `src/lisp/` (reader only; Lisp semantics live in `vm::frontend::lisp`),
`vocabulary/`, and `examples/finch/`. The program runtime service is `src/runtime/`; the program
catalog is `src/programs/`.

**Interface:** [`INTERFACE.md`](INTERFACE.md) lists every exported item with its signature;
it is generated from the `pub use` list in `src/vm/mod.rs`, which is the whole public surface. Child modules are
private, so reaching past it is a compile error, and `scripts/check_subsystems.py` rejects any
`pub mod` there. To expose something new, re-export it deliberately. `src/lisp/mod.rs` has no
facade yet.

**Dependencies:** `vm` is the foundation and imports no other module of this crate. Keep it that
way: a `crate::` import here is a design error, not a shortcut. The pure CPU fiber
scheduler (`fiber.rs`) and `ProgramLanguage` (`language.rs`) live here for that reason; `programs`
re-exports `ProgramLanguage`. One test in `src/vm/runtime.rs` still calls
`programs::parse_finch_script` and must move before `finch-vm` becomes its own crate.

**Effects** go through the capability broker, never around it
([capability boundaries](../../CLAUDE.md#key-principles)).

**Focused tests:** `./scripts/test_brains.sh cargo test --lib -- vm:: lisp::`. Run the full suite
when changing a re-exported `pub` item, the IR, the verifier, or `vocabulary/`.

**Reference, not required:** the language contracts in `vocabulary/language/` are compiled into
the binary and shown to the model, so editing them changes model-facing behavior.
