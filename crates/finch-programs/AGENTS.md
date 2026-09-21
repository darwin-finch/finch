# Finch programs agent contract

Supplements the root [agent rules](../../AGENTS.md). The [README](README.md) explains why this
crate exists and traces two callers. [`src/lib.rs`](src/lib.rs) is its flat facade; use
`cargo doc -p finch-programs --no-deps --open` for public methods on re-exported types.

## Dependencies and extension rules

- `finch-vm` supplies typed execution and wire-failure contracts, `finch-language` compiles
  source, and `finch-tools-api` supplies shared effect vocabulary. The application may compose
  programs with `finch-memory` and `finch-runtime`; this crate must not depend on their stores,
  live runtime sessions, CLI, or UI.
- Own stable program identity and source metadata, script envelopes, the model vocabulary
  manifest, Co-Forth streaming tokens, and source-only wire-corpus capture. Language meaning
  belongs to the Forth/Lisp frontends; there is no second evaluator here.
- Keep the catalog a discovery *model*, not an eager collection of preloaded programs. Canonical
  authored source and rebuildable index persistence belong to the application registry and
  memory. Export only real caller capabilities as flat items from `src/lib.rs`.

## Invariants and lifetimes

- Corpus capture gets its `ProgramCompilerContext` lazily: with capture disabled, a caller must
  not build or clone a live `ProgramRuntime` just for telemetry.
- A rejected program may get one source-only repair at the compile/link boundary. Runtime
  limits, approvals, cancellation, and host-effect failures must never become implicit retries
  of a program that may already have caused effects.
- Provider-stream tokenization is for safe preview, not incremental execution. Full source is
  compiled and verified before the VM runs it.

## Focused proof

```bash
.agents/skills/finch-backlog/scripts/with-cargo-slot ./scripts/test_brains.sh cargo test -p finch-programs --lib
.agents/skills/finch-backlog/scripts/with-cargo-slot ./scripts/test_brains.sh cargo test -p finch --lib program_registry::
```

If a public contract, script envelope, or corpus format changes, run the supervised workspace
suite and relevant CLI wire-repair tests. `scripts/check_subsystems.py` does not exist; use
`scripts/seam_cost.py` for dependency evidence.
