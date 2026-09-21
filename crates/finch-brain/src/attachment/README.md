# Brain attachments

An attachment is one participant's projection of a named Brain, separate from the live
transport connection that currently carries it. This module owns those identities, the
participant record, the approval audience value, and the `attachments.json` acknowledgement
cursors used on reconnect. It does not decide who may attach, grant a runner lease, or append
the canonical Brain journal; those decisions belong to the store and its authority checks.

Two callers show the division:

1. [`BrainStore`](../store.rs) validates an attach request and reserves a new connection. It
   records a `ClientAttached` event only when that exact connection becomes active, and folds
   attach/detach events into its live projection. On reconnect it reads and writes acknowledgement
   cursors through this module; a Brain identity mismatch is an error rather than a rewind.
2. [Journal replay](../journal/persist.rs) folds committed attach/detach events into the
   reconstructed attachment map. A detach for an older connection cannot disconnect a newer
   connection on the same attachment identity. Replay owns event ordering; this module owns
   the attachment-state transition.

The [agent contract](AGENTS.md) states the dependency and persistence rules.
[`mod.rs`](mod.rs) is the nested facade; external crates use the flat
[`finch-brain` facade](../lib.rs) rather than this internal path.
