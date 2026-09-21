# Finch memory

`finch-memory` stores and retrieves the local MemTree, maintaining a SQLite-backed index that
can hydrate in the background while a query proceeds. It owns retrieval coverage reporting and
opaque program-index rows. It does not choose a neural embedding model, interpret programs,
write canonical program source, or own named-Brain event journals.

Two callers show the ownership boundary:

1. The [interactive REPL](../../src/cli/repl.rs) chooses an embedding engine from application
   configuration, calls `MemorySystem::open_connection`, and injects that connection into
   `MemorySystem::new_with_connection`. It then asks the application `ProgramRegistry` to index
   selected authored roots. Memory owns the SQLite/MemTree mechanics; the REPL owns model
   selection, startup timing, and which roots to synchronize.
2. The [program registry](../../src/program_registry.rs) writes canonical authored source and
   converts `ProgramDefinition` values into opaque `ProgramIndexRecord` rows for
   `MemorySystem::index_program_record`. Memory persists and searches those rows without knowing
   Forth, Lisp, or manifest semantics. The registry rebuilds the discovery index from source.

The [agent contract](AGENTS.md) covers dependency, hydration, and persistence rules. The
[flat facade](src/lib.rs) and `cargo doc -p finch-memory --no-deps --open` provide the public API.
