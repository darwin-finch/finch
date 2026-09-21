# cli capsule: terminal application and interactive session orchestration

Supplements the root [`AGENTS.md`](../../CLAUDE.md), which still applies in full.

**Owns** `src/cli/`: command handling, the interactive REPL and event loop, terminal rendering,
typed presentation messages, setup and provider-login flows, output routing and dialogs,
and conversation projection. Nested capsules document the event loop, message model, and TUI
details. Application startup and daemon composition remain in the root package.

**Facade:** child modules are private. Callers outside this directory use flat `crate::cli::Item`
imports from the `pub use` list in `mod.rs`; they must not select implementation paths such as
`cli::repl_event`, `cli::tui`, `cli::diff`, or `cli::setup_wizard`. A public item needed by another
root module is re-exported deliberately; root-only helpers should be `pub(crate)`.

**Dependencies:** the CLI is an application-facing composition layer over Brain, IPC, runtime,
programs, providers, tools, memory, scheduler, configuration, and rendering support. Those lower
layers must not acquire CLI dependencies to reuse presentation helpers; move neutral contracts to
their owning lower layer or inject a port instead.

**Focused tests:** `./scripts/test_brains.sh cargo test --lib -- cli::`,
`cargo test --test tui_integration_test`, and `cargo test --test tabbed_dialog_test`. Run
`python3 scripts/check_facade_boundaries.py` whenever the module surface changes, and run the full
supervised workspace suite before extraction.
