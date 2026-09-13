# vm capsule: typed VM, CoForth, CoLisp

Supplements the root [`AGENTS.md`](../../CLAUDE.md), which still applies in full.

**Owns** `crates/finch-vm/src/` (IR, verifier, interpreter, capabilities, effects, CoForth and
CoLisp frontends, plus the Lisp reader), and the repository-root `vocabulary/` and
`examples/finch/`. The program runtime service is `src/runtime/`; the program catalog is
`src/programs/`.

**Interface:** [`INTERFACE.md`](INTERFACE.md) lists every exported item with its signature;
it is generated from the `pub use` list in `src/lib.rs`, which is the whole public surface. Child
modules are private, so reaching past it is a compile error. To expose something new, re-export it
deliberately. The root crate provides compatibility namespaces for the former `finch::lisp`
reader and types paths.

**Dependencies:** `finch-vm` is the foundation and does not depend on the root `finch` crate. Its
only direct third-party dependencies are `anyhow`, `once_cell`, `serde`, `serde_json`, `thiserror`,
and `uuid`. The pure CPU fiber scheduler (`fiber.rs`) and `ProgramLanguage` (`language.rs`) live
here for that reason; `programs` re-exports `ProgramLanguage`.

**Effects** go through the capability broker, never around it
([capability boundaries](../../CLAUDE.md#key-principles)).

**Focused tests:** `./scripts/test_brains.sh cargo test -p finch-vm --lib`. Run the full suite
when changing a re-exported `pub` item, the IR, the verifier, or `vocabulary/`.

**Reference, not required:** the language contracts in `vocabulary/language/` are compiled into
the binary and shown to the model, so editing them changes model-facing behavior.
