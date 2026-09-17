# Atomic provider/tool history

Provider tool rounds have two representations with one authority:

1. `ConversationHistory` stages the provider's complete ordered assistant payload under the
   query UUID and a fresh `ToolRoundToken`. Staged payloads are excluded from provider reads,
   snapshots, and compaction.
2. Every inline, background, and deferred tool result carries that token. Results are accepted
   once, only for declared tool IDs, and retained in assistant declaration order.
3. Once all results exist, the LLM worker acknowledges continuation readiness. History then
   publishes the original assistant payload and one adjacent result message under a single write
   lock. The worker spawns a continuation task behind a publication permit. A failed admission
   or optional test checkpoint restores the complete round to invisible staging.

Cancellation and terminal provider failure delete only the staged publication. They do not
replace or reinterpret the durable effect audit introduced by #163: an already-started host
effect may still report its one physical outcome, but its late `ToolResult` cannot enter provider
history. A retry gets a new token, so stale and duplicate continuations cannot attach to it.

In-memory `ConversationHistory` is a provider-context projection of canonical Brain events, not a
separately durable owner. Publication still stages, commits, and rolls back in memory under one
write lock; a continuation admission failure restores the complete round to invisible staging.
Named Brains (`finch attach <name>`) are the durable resume identity. Tests may still point a
fixture file at the optional conversation checkpoint to prove atomic replace. Staged rounds are
intentionally absent after restart.

The assistant payload is stored without reconstructing its content blocks. This preserves an
ordered opaque-item seam for provider-native encrypted reasoning or output metadata (#202)
without assigning that metadata new authority in this layer.
