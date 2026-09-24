# Finch terminal application

This module owns command handling, the interactive REPL, setup and login flows, conversation
presentation, and terminal output coordination. It composes lower-level Brain, provider, runtime,
tool, UI, and conversation-state capabilities; it does not own their execution or storage rules.
`finch-conversation` owns provider-visible history and ordered tool-round staging, while the CLI
decides when to generate summaries and admit checkpoints. `mod.rs` is the flat
facade for other root-package callers. The message and TUI crates own their narrower
presentation contracts.

For an interactive turn, `src/main.rs` constructs `Repl` from configuration and providers. The
REPL's event loop routes input and tool results, updates message snapshots, and asks `TuiRenderer`
to draw them. The CLI owns turn timing and application decisions; the renderer owns terminal
layout and input, and `finch-ui-model` owns pure WorkUnit projection.

For the in-terminal diagnostic console, `diagnostic_console` tails bounded portions of this
frontend's local log and the local daemon-log path, strips terminal controls, and implements the
renderer-owned `DiagnosticConsolePort`. The renderer decides how Ctrl+` presents and scrolls that
snapshot. It is intentionally file-backed today: a daemon running on another host does not send
its logs through this port.

For an `AskUserQuestion` tool call, `repl_event::plan_handler` parses the wire request owned by
`llm_dialogs`. A single question becomes an inline `Dialog`; multiple questions become
renderer-owned `QuestionView` values in a tabbed dialog. The resulting answers and markdown
annotations are assembled back in the CLI; the renderer never interprets the tool's request or
response schema.

Read [AGENTS.md](AGENTS.md) for allowed dependencies and focused tests, and [mod.rs](mod.rs) for
the callable facade. The [conversation-state README](../../crates/finch-conversation/README.md),
[message README](../../crates/finch-messages/README.md), and
[TUI README](../../crates/finch-tui/README.md) explain their narrower boundaries without an API
catalog.
