# Finch runtime

`finch-runtime` turns verified Forth and Lisp programs into bounded executions. It owns the
session runtime, capability grants, host-effect requests and their audit state, resumable
continuations, and the versioned Runtime/Application messages used to deliver effects. The
runtime owns the meaning of a `ProgramRun`, effect cursor, and resume response; an application
decides who is authorized to run and how to present or persist the result.

It does not own named-Brain records, the REPL or its dialogs, MCP connections, or the editor
used to review a proposed artifact. The application binds those host facilities through runtime
ports. The typed VM and program compiler are dependencies, not hidden implementations here.

Two caller workflows show the boundary:

1. `BrainStore::program_runtime` restores a named Brain's reducible checkpoint with
   `ProgramRuntime::from_checkpoint_at_revision`, restores its separate authority state, and
   binds a Brain-owned `VmEffectDeliveryLog`. The Brain store owns the path, Brain/client identity,
   and durable event records; the runtime owns checkpoint validation, authority semantics, and
   effect-delivery cursors. Start in [`BrainStore`](../finch-brain/src/store.rs) and use the flat
   [`finch-runtime` facade](src/lib.rs) for the runtime calls.
2. The interactive query processor builds a `ProgramSubmission` from provider source, sends
   effects to its event loop through a `TypedEffectSink`, and calls `submit_tool_program`. An
   approval or awaited host effect resumes the retained execution with
   `resolve_typed_approval` or `resume_vm_effect`; it never submits the source a second time.
   Start in the [query processor](../../src/cli/repl_event/query_processor.rs) and use the same
   facade through the root `crate::runtime` compatibility path.

The [agent contract](AGENTS.md) states dependency rules, lifetimes, and focused tests. The
[`src/lib.rs` facade](src/lib.rs) defines the exports and facade-local signatures; methods on
re-exported types are still defined in their private implementation files.
