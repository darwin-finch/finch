# planning capsule: IMPCPD iterative plan refinement

Supplements the root [`AGENTS.md`](../../CLAUDE.md), which still applies in full.

**Owns** `src/planning/` (the `/plan` IMPCPD loop, critique types, persona selection, and the
embedded methodology spec). DESIGN.md lists this tree on the providers row; this capsule is
`src/planning/` only. Provider transports, OAuth, and the Claude client live outside this subtree.

**Interface:** child modules are private, so the `pub use` list in `src/planning/mod.rs` is the
whole public surface — read it directly for exact signatures. Callers outside this directory use
`crate::planning::Item`; they must not name `loop_runner`, `personas`, or `types`.

**Dependencies:** `claude` (`Message`), `cli` (`OutputManager`, TUI dialogs), `generators`
(`Generator`), and `providers` (`UNIVERSAL_ALIGNMENT_PROMPT`). Do not extract `finch-planning`
until those edges are measured and the providers-row ownership is a crate-level contract.

**IMPCPD behavior is not this commit.** Persona activation, scoring, convergence, and the
methodology spec stay as they are. Do not change plan generation, critique, or steering in a
facade commit.

**Focused tests:** `./scripts/test_brains.sh cargo test --lib -- planning::`. Run CLI plan-mode
tests when changing a re-exported `pub` item.
