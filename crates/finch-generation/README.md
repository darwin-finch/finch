# Finch generation contract

This crate defines a provider-neutral generation run: its request, event stream, backend
readiness, identity, cancellation, and terminal outcome. It also supplies a supervisor and
injected environmental ports so backends can be exercised without Finch application state.
Provider-specific parsing belongs to `finch-providers`; tool execution, configuration, Brain,
and presentation belong to Finch. These types are a development seam, not a durable wire format.

For a new backend, `examples/scripted_backend.rs` constructs a `BackendRef`, a ready
`ScriptedBackend`, a `GenerationRequest`, and a `GenerationSupervisor` with test ports. It reads
the event stream through its terminal outcome without a Finch `Config` or tool executor.

For lifecycle changes, `tests/lifecycle.rs` drives the same public contract from outside the
crate. Its completed-run and loading cases check exactly-one terminal events, requested versus
actual identity, and the rule that a backend still loading cannot generate. The provider adapter
cases wrap a recording `finch-providers` backend and verify requested model identity at dispatch.

The Finch compatibility facade at `src/generators/mod.rs` re-exports some contract types, but
production REPL generation still uses its older `Generator` and provider `StreamChunk` path.
There are not yet two production callers of this contract; do not describe it as the active
REPL lifecycle. Read [AGENTS.md](AGENTS.md) for constraints, [src/lib.rs](src/lib.rs) for the
facade, and `cargo doc -p finch-generation --no-deps --open` for callable methods.
