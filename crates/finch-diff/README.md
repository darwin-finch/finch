# Bounded file-diff presentation

`finch-diff` owns Finch's bounded structured file-diff model, terminal-safe text sanitation,
summary, and themed or plain rendering. It accepts caller-supplied paths and text; it does not
read or write files, decide whether a tool is approved, own a conversation, or paint terminal
cells. Its input and output limits are part of the safety boundary, not merely display choices.

Two callers show why this is a shared leaf:

1. The [WorkUnit message](../../src/cli/messages/work_unit.rs) retains a structured `FileDiff`
   with a tool row and asks this crate for a bounded themed preview. The message owns the row's
   lifecycle and canonical transcript; this crate owns escaping, truncation, and diff formatting.
2. The [TUI dialog](../../src/cli/tui/dialog.rs) sanitizes untrusted tool names and multiline
   summaries before placing them in an approval card. The renderer decides layout and interaction;
   this crate removes terminal controls and bounds the text. The application decides what tool
   request is being reviewed.

The application-facing `cli::diff` path remains a compatibility re-export for existing event-loop
callers. Read the [agent contract](AGENTS.md) for bounds and dependency rules. [`src/lib.rs`](src/lib.rs)
is the flat callable facade; rustdoc shows methods on its exported model types.
