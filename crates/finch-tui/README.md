# Finch terminal UI

This crate owns interactive terminal painting and input: the live composer, completion pane,
dialog widgets, transcript viewport, native-scrollback commit, and the setup wizard's widget host.
It turns caller-supplied presentation state into terminal rows. It does not own a conversation,
Brain, tool execution, provider request, or project-file discovery. The application supplies
those through message snapshots, status state, and narrow ports such as `MentionPort`.
The diagnostic console follows the same boundary: the application supplies bounded, sanitised
log snapshots through `DiagnosticConsolePort`, while the renderer owns its Ctrl+` reader in the
transcript viewport, visible-range indicator, and keyboard/wheel scroll state. Focused diagnostic
and tool-result readers replace the conversation rows above the separator rather than rendering
inside the composer/status chrome below it.

Two callers show the boundary:

1. The [interactive REPL](../../src/cli/repl.rs) constructs `TuiRenderer` with application-owned
   `OutputManager` through `TuiOutputPort`, `StatusBar` through `TuiStatusPort`, and colors. The
   [event loop](../../src/cli/repl_event/event_loop.rs) updates the session and
   asks the renderer to draw; the renderer projects each message's presentation snapshot and
   commits completed rows to terminal scrollback once. An AskUserQuestion request is converted by
   the CLI into `QuestionView` before the renderer presents tabbed cards; answers and annotations
   return through the CLI, not the renderer. The REPL owns conversation timing and Brain/provider
   events and status-line policy; it also supplies startup version/tagline text and the
   external-editor activity query. If a mention submission fails, the event loop asks the
   renderer to restore the draft; render failures are recorded and acknowledged through
   renderer methods. This module owns layout, input polling, and terminal lifecycle.
2. The [setup wizard driver](../../src/cli/setup_wizard/driver.rs) converts its form state into a `WizardView` of
   styled span lines (stage 4, #1141 — no terminal bytes in the props), calls `plan_wizard_frame`, and lets
   `WizardHost` lower the spans and paint the frame. The driver owns provider setup, credential flow, and
   key-driven state transitions; this module owns the span lowering, the shared claiming tree, shadow-buffer
   diff, and terminal blit.

A third consumer is the future GUI client (#808): the engine lowers the same widget tree and
component snapshots to the versioned DOM manifest (`dom_manifest.rs`; contract in
[docs/UI_MANIFEST.md](../../docs/UI_MANIFEST.md), generated TS types committed under
`bindings/dom/`). Components never write HTML; the golden JSON test pins the say card's wire
shape.

The [agent contract](AGENTS.md) states dependency and rendering invariants. [`src/lib.rs`](src/lib.rs)
is the callable facade; [ARCHITECTURE.md](ARCHITECTURE.md) explains the paint pipeline. Completion
state is internal to this crate, and the old contextual-suggestion manager had no production
reader, so neither belongs in the public surface.
