# Finch terminal application

This module owns command handling, the interactive REPL, setup and login flows, conversation
presentation, and terminal output coordination. It composes lower-level Brain, provider, runtime,
tool, and UI capabilities; it does not own their execution or storage rules. `mod.rs` is the flat
facade for other root-package callers. The message and TUI crates own their narrower
presentation contracts.

For an interactive turn, `src/main.rs` constructs `Repl` from configuration and providers. The
REPL's event loop routes input and tool results, updates message snapshots, and asks `TuiRenderer`
to draw them. The CLI owns turn timing and application decisions; the renderer owns terminal
layout and input, and `finch-ui-model` owns pure WorkUnit projection.

For an `AskUserQuestion` tool call, `repl_event::plan_handler` parses the wire request owned by
`llm_dialogs`. A single question becomes an inline `Dialog`; multiple questions become
renderer-owned `QuestionView` values in a tabbed dialog. The resulting answers and markdown
annotations are assembled back in the CLI; the renderer never interprets the tool's request or
response schema.

Read [AGENTS.md](AGENTS.md) for allowed dependencies and focused tests, and [mod.rs](mod.rs) for
the callable facade. The [message README](../../crates/finch-messages/README.md) and [TUI README](../../crates/finch-tui/README.md)
explain those two presentation boundaries without an API catalog.
