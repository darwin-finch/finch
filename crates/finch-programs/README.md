# Finch programs

This crate describes a program as source plus durable identity and metadata. It also holds the
small vocabulary manifest presented to a model, wire-repair classification, and optional
source-only corpus capture. “Catalog” means a way to *describe and discover* authored programs;
it does not mean the crate preloads a library of executable programs into a runtime.

It does not own the canonical source files, SQLite index, a running VM, approvals, or a live
Brain. The language frontends decide what Forth and Lisp mean; the application decides what
programs are saved and which are relevant to a request.

Two callers illustrate that split:

1. The [application program registry](../../src/program_registry.rs) uses `ProgramDefinition`
   and `ProgramRef` to save an authored source file, convert it to a rebuildable memory index
   record, and build a task-specific `VmManifest`. This crate defines those identities and
   manifest shapes; the registry chooses paths, persists files, and queries memory.
2. The [REPL query processor](../../src/cli/repl_event/query_processor.rs) optionally calls
   `capture_with_compiler_context_from_env` for a first-pass or repaired wire submission. It
   supplies `ProgramRuntime::compiler_context` lazily so disabled capture does no extra runtime
   work. The REPL owns provider output and repair policy; this crate only records source and
   source-only compilation evidence.

The [agent contract](AGENTS.md) covers allowed dependencies and invariants. The [flat
facade](src/lib.rs) and `cargo doc -p finch-programs --no-deps --open` show the callable API.
