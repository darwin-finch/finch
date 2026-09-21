# Finch typed-machine core

`finch-vm-core` is the shared meaning of a verified Finch program: typed IR, stack signatures,
effects and capability requirements, source diagnostics, vocabulary, and verification. It is a
foundation crate, not a source-language frontend or an interpreter. Capability requirements are
descriptions here; granting authority and executing host effects are later runtime decisions.

Two consumers need this contract for different reasons:

1. The [CoLisp frontend](../finch-colisp/src/frontend.rs) turns span-preserving source forms
   into shared `Instruction` and `SemanticBuilder` structures, then calls the common
   certification pipeline. CoLisp owns reader syntax; core owns the type and verifier rules that
   make its result a `ModuleVerified` value.
2. The [VM interpreter](../finch-vm/src/interpreter.rs) executes already verified instructions
   and carries typed values, diagnostics, and capability requirements across host boundaries.
   Core defines those records; the VM owns execution order, suspension, and checkpointing.

The [agent contract](AGENTS.md) states dependency and compatibility rules. The [flat
facade](src/lib.rs) and `cargo doc -p finch-vm-core --no-deps --open` show the public API. Planned
cross-frontend semantics remain in the shared [language design](../../docs/language/README.md).
