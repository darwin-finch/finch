# Finch runtime agent contract

Supplements the root [`AGENTS.md`](../../CLAUDE.md). Read the [runtime README](README.md) for
ownership and caller workflows; the flat `pub use` surface and facade-local signatures are in
[`src/lib.rs`](src/lib.rs). Methods of re-exported types remain defined in private child files.
Use `cargo doc -p finch-runtime --no-deps --open` to read their public signatures and method
contracts without opening those implementations. Root callers may use the
`crate::runtime` compatibility re-export; crate callers use `finch_runtime`.

## Dependencies and extension rules

- The runtime may depend on `finch-vm`, `finch-language`, `finch-programs`, `finch-memory`,
  `finch-ipc`, and `finch-tools-api`. It must not import Brain, CLI, server, theme, or root tool
  implementations. Brain and other applications depend on runtime, never the reverse.
- Keep runtime-defined contracts here: submission, capability and effect authority, checkpoint
  validation, portable effect delivery, and resume. The application owns named-Brain paths,
  client identity, editor/UI decisions, and transport connections. Bind the latter through the
  existing `RuntimeMcpClient`, `ArtifactProposalHost`, and effect sink seams at composition.
  Do not add a manager, service locator, or a trait around a pure helper.
- Add an externally needed item as a flat re-export from `src/lib.rs`; do not make child modules
  public. Confirm a real caller needs it and keep the associated type beside the invariant it
  represents. Do not duplicate the facade as a generated signature catalog.

## Invariants and lifetimes

- A `ProgramRuntime` belongs to one execution session or named Brain. Its reducible checkpoint
  is separate from host-owned authority, filesystem roots, and live transport bindings. Restore
  and rebind those deliberately after restart; never infer authority from a checkpoint alone.
- A `(ProgramRun, effect sequence)` identifies one host effect. The application must resolve the
  retained continuation once; approval, cancellation, and replay must not resubmit source or
  redispatch a host effect. Delivery cursors are per Brain/client consumer.
- Runtime/Application ABI version 1 is a development seam, not a public compatibility promise.
  Bump finch-vm's `RUNTIME_APPLICATION_ABI_VERSION` and fail closed on incompatible frames; do
  not create a second journal or alternate encoding. Host effects remain subject to typed grants
  and the effect-audit fence.

## Focused proof

Run tests through the repository supervisor and Cargo slot:

```bash
.agents/skills/finch-backlog/scripts/with-cargo-slot ./scripts/test_brains.sh cargo test -p finch-runtime --lib
.agents/skills/finch-backlog/scripts/with-cargo-slot ./scripts/test_brains.sh cargo test -p finch-brain --lib
.agents/skills/finch-backlog/scripts/with-cargo-slot ./scripts/test_brains.sh cargo test -p finch --lib cli::repl_event::query_processor::tests::direct_wire_text_is_a_lisp_or_forth_submission_not_display_prose -- --exact
```

If the facade or host-effect behavior changes, also run the supervised workspace suite and the
relevant Brain/REPL integration tests. `scripts/check_subsystems.py` does not exist; use
`scripts/seam_cost.py` for dependency evidence and the crate boundary for enforced direction.
