# vm capsule: typed VM, CoForth, CoLisp

Supplements the root [`AGENTS.md`](../../CLAUDE.md), which still applies in full.

**Owns** `src/vm/` (IR, verifier, interpreter, capabilities, effects, CoForth and CoLisp
frontends), `src/lisp/` (reader only; Lisp semantics live in `vm::frontend::lisp`),
`vocabulary/`, and `examples/finch/`. The program runtime service is `src/runtime/`; the program
catalog is `src/programs/`.

**Interface:** no facade is enforced yet (#541 phase 3); use the `pub use` re-exports in
`src/vm/mod.rs` and `src/lisp/mod.rs`.

**Dependencies:** layer 0 in [`subsystems.toml`](../../subsystems.toml), no allowed edges. Debt:
`vm → programs` and `vm → runtime`, both in `src/vm/runtime.rs`. Importing any other subsystem
fails `scripts/check_subsystems.py`. The debt record allows `programs` and `runtime` imports
anywhere in this subsystem, so the check will not stop a new one; add none.

**Effects** go through the capability broker, never around it
([capability boundaries](../../CLAUDE.md#key-principles)).

**Focused tests:** `./scripts/test_brains.sh cargo test --lib -- vm:: lisp::`. Run the full suite
when changing a re-exported `pub` item, the IR, the verifier, or `vocabulary/`.

**Reference, not required:** the language contracts in `vocabulary/language/` are compiled into
the binary and shown to the model, so editing them changes model-facing behavior.
