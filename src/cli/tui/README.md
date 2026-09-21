# Finch terminal UI

This module owns interactive terminal painting and input: the live composer, completion pane,
dialog widgets, transcript viewport, native-scrollback commit, and the setup wizard's widget host.
It turns caller-supplied presentation state into terminal rows. It does not own a conversation,
Brain, tool execution, provider request, or project-file discovery. The application supplies
those through message snapshots, status state, and narrow ports such as `MentionPort`.

Two callers show the boundary:

1. The [interactive REPL](../repl.rs) constructs `TuiRenderer` with application-owned
   `OutputManager` through `TuiOutputPort`, `StatusBar` through `TuiStatusPort`, and colors. The
   [event loop](../repl_event/event_loop.rs) updates the session and
   asks the renderer to draw; the renderer projects each message's presentation snapshot and
   commits completed rows to terminal scrollback once. An AskUserQuestion request is converted by
   the CLI into `QuestionView` before the renderer presents tabbed cards; answers and annotations
   return through the CLI, not the renderer. The REPL owns conversation timing and Brain/provider
   events and status-line policy, while this module owns layout and terminal lifecycle.
2. The [setup wizard driver](../setup_wizard/driver.rs) converts its form state into a `WizardView`,
   calls `plan_wizard_frame`, and lets `WizardHost` paint the frame. The driver owns provider setup,
   credential flow, and key-driven state transitions; this module owns the shared claiming tree,
   shadow-buffer diff, and terminal blit.

The [agent contract](AGENTS.md) states dependency and rendering invariants. [`mod.rs`](mod.rs)
is the callable facade; [ARCHITECTURE.md](ARCHITECTURE.md) explains the paint pipeline. Completion
state is internal to this module, and the old contextual-suggestion manager had no production
reader, so neither belongs in the public surface.
