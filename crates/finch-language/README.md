# Finch language compilation

`finch-language` chooses Co-Forth or CoLisp from an explicit `ProgramLanguage`, runs the shared
construction and verification pipeline, and returns a `ModuleVerified` certificate. It also
owns the compact-wire grammar assembled from the two production reader lexicons. It does not
execute a module or decide grants, approvals, or host effects.

Two production callers use the compiler for distinct tasks:

1. The [program corpus auditor](../finch-programs/src/corpus.rs) calls
   `compile_with_functions` with a source-only compiler context to classify captured wire
   attempts. It records diagnostics without running the program or producing host effects.
2. The [program runtime](../finch-runtime/src/lib.rs) compiles a submitted source with the
   current stack types, vocabulary, and linked functions, then passes the verified module to
   the VM. Runtime owns authority and execution; this crate guarantees only that compilation
   and shared verification succeeded.

The [agent contract](AGENTS.md) states compiler invariants. The [flat facade](src/lib.rs) and
`cargo doc -p finch-language --no-deps --open` show the public API. The published GBNF recognizes
a complete response; it is not a type or safety proof.
