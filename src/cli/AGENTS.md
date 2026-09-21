# cli capsule: terminal application and interactive session orchestration

Supplements the root [`AGENTS.md`](../../CLAUDE.md), which still applies in full.

**Owns** `src/cli/`: command handling, the interactive REPL and event loop, terminal rendering,
setup and provider-login flows, output routing and dialogs, and conversation composition.
Typed presentation messages live in `finch-messages`; `cli::messages` is a flat compatibility
facade. Nested capsules document the event loop and TUI. Application startup and daemon
composition remain in the root package.

**Facade:** child modules are private. Callers outside this directory use flat `crate::cli::Item`
imports from the `pub use` list in `mod.rs`; they must not select implementation paths such as
`cli::repl_event`, `cli::tui`, `cli::diff`, or `cli::setup_wizard`. A public item needed by another
root module is re-exported deliberately; root-only helpers should be `pub(crate)`. The
[README](README.md) traces two caller workflows; do not recreate a generated signature catalog.

**Dependencies:** the CLI is an application-facing composition layer over Brain, IPC, runtime,
programs, providers, tools, memory, scheduler, configuration, and rendering support. Those lower
layers must not acquire CLI dependencies to reuse presentation helpers; move neutral contracts to
their owning lower layer or inject a port instead.
`llm_dialogs` owns the AskUserQuestion wire schema, validation, and answer annotation; it maps
questions to the renderer-owned `QuestionView` when constructing a tabbed dialog. The renderer
must not import those wire types.
`StatusBar` implements the renderer-owned `TuiStatusPort` here: status ordering and line policy
remain application concerns, while the TUI receives only rendered snapshots and sends back its
own child-activity and operation updates. `OutputManager` implements the renderer-owned
`TuiOutputPort`: the application retains messages and controls stdout, while the renderer reads
snapshots and records settled dialog answers through that narrow stateful seam.

**Focused tests:** `./scripts/test_brains.sh cargo test --lib -- cli::`,
`./scripts/test_brains.sh cargo test --test tui_integration_test`, and
`./scripts/test_brains.sh cargo test --test tabbed_dialog_test`. Run
`python3 scripts/check_facade_boundaries.py` whenever the module surface changes, and run the full
supervised workspace suite before extraction.
