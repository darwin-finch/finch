# Automatic Training (Disabled)

**Status:** Disabled as of 2026-08-25; see GitHub issue #139.

Finch does not automatically collect OpenAI-compatible requests, start a
training timer or worker, invoke Python, rewrite a training queue, generate a
LoRA adapter, or hot-load adapters.

Explicit feedback submitted with Ctrl+G/Ctrl+B, feedback commands, or
`POST /v1/feedback` is stored in `~/.finch/feedback.jsonl`. The store is private
(`~/.finch` mode 0700 and the file mode 0600 on Unix), append-only, locked across
processes, and synced before acknowledgement. Feedback records do not consent
to or trigger training.

Existing `~/.finch/training_queue.jsonl` files and adapter files are preserved
without processing, deletion, or migration. The legacy Python setup command is
an explicit manual experiment and is not connected to the daemon or feedback
path.

Training remains blocked on issues #1, #7, and #74 until Finch has a supported
native path with explicit privacy, resource, cancellation, recovery, and
retention controls.
