# planning capsule: IMPCPD iterative plan refinement

Supplements the root [`AGENTS.md`](../../CLAUDE.md), which still applies in full.

**Owns** `src/planning/` (the `/plan` IMPCPD loop, critique types, persona selection, and the
embedded methodology spec). DESIGN.md lists this tree on the providers row; this capsule is
`src/planning/` only. Provider transports, OAuth, and the Claude client live outside this subtree.

**Boundary:** the [README](README.md) traces the interactive and live-contract callers.
[`mod.rs`](mod.rs) is the flat callable facade; rustdoc supplies methods on exported types.
Child modules are private. Callers outside this directory use `crate::planning::Item`; they must
not name `loop_runner`, `personas`, or `types`. Do not recreate a signature catalog.
Persona selection, convergence classification, and steering feedback are internal loop details;
do not re-export them merely because the implementation uses them.

**Dependencies:** `cli` (`OutputManager`, TUI dialogs), `generators` (`Generator`), and `providers`
(`Message`, `UNIVERSAL_ALIGNMENT_PROMPT`). There is no direct `claude` import. The event loop
owns REPL mode transitions and the timestamped plan file; this module owns generation, critique,
convergence, and user steering within one loop. Do not extract `finch-planning`
until those edges are measured and the providers-row ownership is a crate-level contract.

**Invariants and lifetimes:** `PlanLoop` holds shared generator and output handles for one
bounded run; the caller owns the TUI, REPL mode, and plan file. `run` clears the TUI operation
status even when the loop errors. Cancellation, user approval, convergence, and the iteration cap
remain distinct `PlanResult` outcomes. Do not write a plan file or select a provider here.

**Extension:** Keep persona activation, critique scoring, JSON parsing, and the embedded
methodology in agreement. Add focused parser/convergence tests for a changed critique contract;
use the ignored live tests only when provider credentials and dated conformance evidence are
actually available. Do not change plan generation or steering in a documentation-only edit.

**Focused tests:** `./scripts/test_brains.sh cargo test -p finch --lib planning::`. Run CLI
plan-mode tests when changing a re-exported `pub` item. The ignored live IMPCPD tests require
provider credentials and are not part of the default focused suite.
