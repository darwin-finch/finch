# CoLisp frontend

`finch-colisp` reads CoLisp source with spans and lowers its forms directly into the shared typed
construction protocol. It does not translate Lisp to Forth text, run programs, choose capability
grants, or implement a private verifier.

The [language compilation facade](../finch-language/src/lib.rs) uses `compile_lisp` or
`compile_lisp_with_functions` when the requested source language is Lisp. This frontend owns
Lisp syntax and source-origin preservation; `finch-vm-core` owns the type rules and certificate.

The same facade's [compact-wire recognizer](../finch-language/src/wire.rs) uses `parse_str` to
check that a complete provider response is syntactically Lisp, and `lisp_lexicon` when rendering
the published grammar. That check is not compilation or permission to execute: unknown words
can still fail when the whole source reaches the typed compiler.

The [agent contract](AGENTS.md) gives dependency and lowering rules. The [flat
facade](src/lib.rs) and `cargo doc -p finch-colisp --no-deps --open` show the callable API.
Shared planned semantics live in the [language design](../../docs/language/README.md).
