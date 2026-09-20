# finch-programs: program identity, catalog, and corpus

This crate is the durable record of "what is a Finch program": identity and metadata for a stored
program, the script envelope, and the wire-corpus capture used for compiler-conformance auditing.
It exists separately from execution (`finch-vm`) and scheduling (`src/runtime/`) so that a
program's identity and catalog entry can be reasoned about without pulling in either the
interpreter or the scheduler.

Ownership, dependencies, invariants, and test commands are in [`AGENTS.md`](AGENTS.md); the exact
Rust surface is [`src/lib.rs`](src/lib.rs).

## Scope

The typed machine that actually runs a program is `crates/finch-vm/`; the service that schedules
and authorizes a run is `src/runtime/`. `ProgramLanguage` (what source language a program is
written in) lives in `vm` and is only re-exported here for compatibility — a program's meaning
belongs to the VM frontends, never to a second evaluator in this crate.
