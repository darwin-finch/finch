# Co-Forth frontend

`finch-coforth` parses Co-Forth source and lowers structured syntax to Finch's shared typed
construction protocol. It owns reader lexicon and syntactic completeness, but no interpreter,
capability authority, or separate type system.

The [language compilation facade](../finch-language/src/lib.rs) calls `compile_forth` or
`compile_forth_with_functions` for a Co-Forth submission. This frontend preserves source spans
and produces shared typed IR; `finch-vm-core` supplies verification and certification.

The same facade's [compact-wire recognizer](../finch-language/src/wire.rs) calls
`read_forth_source` to decide whether a provider response is a complete syntactic submission,
and uses `forth_lexicon` to render the published grammar. A syntactically complete response can
still fail type or capability verification; no prefix executes during streaming.

The [agent contract](AGENTS.md) gives dependency and lowering rules. The [flat
facade](src/lib.rs) and `cargo doc -p finch-coforth --no-deps --open` show the callable API.
Shared planned semantics live in the [language design](../../docs/language/README.md).
