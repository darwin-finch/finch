# finch-vm-core: shared typed machine contract

This crate is the one thing both language frontends and the VM agree on: the provider-neutral
typed IR, type/signature model, verifier, diagnostics, capability/effect descriptions, and the
syntax-neutral semantic-construction protocol frontends submit to. It exists so CoLisp and
Co-Forth can be two independent readers that still produce byte-identical typed IR, verified once,
by one shared verifier. It owns no frontend, interpreter, runtime, or checkpoint codec.

Current concise ownership and test instructions are in [`AGENTS.md`](AGENTS.md); the exact Rust
surface is [`src/lib.rs`](src/lib.rs).

## Further documentation

Planned cross-frontend language semantics remain in the shared
[Finch language design](../../docs/language/README.md) until they are implemented and stable
enough to extract into an IR/verifier reference here.
