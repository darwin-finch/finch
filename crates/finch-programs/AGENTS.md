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
- **Narrowed dead public surface (#949 bounded pass, 2026-09-29).** Issue #949 followed up on
  #869/#945's extraction and explicitly warned that the LSP audit run at extraction time missed
  known, compiling root-package callers of `ProgramDefinition`/`ProgramValue` — the audit result
  for this crate specifically is unreliable, and #949 says not to classify anything as dead
  solely from it. This pass therefore re-ran the item-by-item audit from scratch, using
  whole-workspace `grep` (`src/`, `crates/`, `tests/`) as the primary tool against every `pub`
  item in `src/lib.rs`'s facade and everything it re-exports from `corpus.rs`, rather than
  trusting LSP alone. Every currently-public item in the facade (`ExecutionEffect`,
  `ProgramLanguage`, `ProgramValue`, `ProgramRef`, `ProgramCompilerContext`, `ProgramDefinition`
  and its constructors, `ProgramScope`/`TrustState` and their `as_str`/`FromStr` impls,
  `ProgramSummary`, `VmManifest` and `prompt_block`, `LanguagePackageIdentity` and
  `FinchScript` (both locked to `pub` anyway as the return-type element/field of an
  externally-called `pub fn`), `WireCorpusAudit`/`WireCorpusCounts` (same reasoning via
  `audit()`'s return type), `hash_text`, `language_package_identities`, `load_program_files`,
  `project_program_root`, `parse_finch_script`, `is_repairable_wire_diagnostic`,
  `wire_repair_request`, `capture_from_env`, `capture_with_compiler_context_from_env`, `audit`,
  the four `BOOT_CAPSULE`/`*_LANGUAGE_DEFINITION`/`LANGUAGE_SCHEMA` constants, and
  `MANIFEST_PROTOCOL_VERSION`) has a confirmed real external caller and is unchanged.
  **Narrowed to crate-private** (no caller anywhere outside this crate, confirmed by
  whole-workspace grep, not deleted): `ForthWireToken`, `ForthWireBuffer` and its `push`/`finish`
  methods (both `lib.rs`; used only by this crate's own `#[cfg(test)] mod tests`, kept
  `#[allow(dead_code)]` rather than `#[cfg(test)]`-gated so the code stays compiled in every
  profile — the more conservative option given this issue's own audit-reliability warning) and
  the free functions they alone call (`collect_complete`, `complete_forth_wire_tokens`,
  `looks_like_streamed_json_object`, `streamed_json_object_end`); and, in `corpus.rs`,
  `WireCorpusEntry`, `WireCorpusLogger`, `WIRE_CORPUS_FORMAT_VERSION`, and `WIRE_CORPUS_PATH_ENV`
  — none of these four were ever named in `lib.rs`'s `pub use corpus::{..}` list and `mod corpus`
  is itself private, so they were already unreachable from outside the crate; the `pub` on them
  was corrected to say so. **No deletions were made in this pass**, per #949's own conservatism
  note: this crate's audit history already contains one confirmed set of LSP false negatives, so
  the risk of a bad deletion here is higher than usual. The one item worth flagging as a
  stronger-confidence future deletion candidate is `ForthWireBuffer::source()` — unlike
  `push`/`finish`, nothing calls it anywhere, including this crate's own tests. **Naming/repartition
  question (#949 item 1) is untouched** — not evaluated or acted on in this pass. One data point
  for whoever picks it up: `scripts/seam_cost.py crates/finch-programs/` currently reports 3
  outgoing subsystems (`finch-vm`, `finch-language`, `finch-tools-api`) against the tool's own
  "above two is not a cheap cut" heuristic, and the heaviest incoming caller by far is
  `finch-brain`'s own test module (45 of 200 incoming references, all from
  `crates/finch-brain/src/store/tests.rs`) — worth a look before deciding whether/how to split
  the corpus-capture concern out, but not itself evidence for or against a rename. Verified with
  `cargo check --workspace --all-targets` (clean) and this crate's `--lib` test suite.

## Invariants and lifetimes

- Corpus capture gets its `ProgramCompilerContext` lazily: with capture disabled, a caller must
  not build or clone a live `ProgramRuntime` just for telemetry.
- A rejected program may get one source-only repair at the compile/link boundary. Runtime
  limits, approvals, cancellation, and host-effect failures must never become implicit retries
  of a program that may already have caused effects.
- `wire_repair_request`'s repair prompt carries a targeted correction hint per
  `WireFailureClass`, not just the raw compiler diagnostic — the diagnostic alone does not say
  *why* a rejection happened, so a model that made a specific misconception gets no better
  information on its repair attempt than on its first without one. `WireFailureClass::MarkdownFence`
  (issue #1230/#1231) and `WireFailureClass::RawProse` (found live: Claude Sonnet 5 replying in
  bare prose, e.g. "Hi — what do you need?", with no corrective hint available on this class)
  both have their own hint function; a new failure class that similarly represents a recurring,
  correctable model misconception should get the same treatment rather than relying on the
  generic fallback prompt text.
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
