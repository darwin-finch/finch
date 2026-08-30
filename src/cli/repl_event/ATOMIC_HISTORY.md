# Atomic provider/tool history

Provider tool rounds have two representations with one authority:

1. `ConversationHistory` stages the provider's complete ordered assistant payload under the
   query UUID and a fresh `ToolRoundToken`. Staged payloads are excluded from provider reads,
   snapshots, compaction, and session persistence.
2. Every inline, background, and deferred tool result carries that token. Results are accepted
   once, only for declared tool IDs, and retained in assistant declaration order.
3. Once all results exist, the LLM worker acknowledges continuation readiness. History then
   publishes the original assistant payload and one adjacent result message under a single write
   lock. The worker must acknowledge admission before it can start provider work; a failed
   admission restores the complete round to invisible staging.

Cancellation and terminal provider failure delete only the staged publication. They do not
replace or reinterpret the durable effect audit introduced by #163: an already-started host
effect may still report its one physical outcome, but its late `ToolResult` cannot enter provider
history. A retry gets a new token, so stale and duplicate continuations cannot attach to it.

The active UUID-named session file is checkpointed at every provider-visible publication boundary,
including before a committed tool pair is admitted to the continuation worker. Session files use
same-directory write, file sync, atomic rename, and directory sync. A continuation admission
failure rolls the pair back to staging and atomically restores the previous checkpoint. Therefore
a restart sees either the previous committed history or the complete ordered pair; it never sees a
partially written JSON file. `--resume` retains the UUID and checkpoint instead of deleting the
only recovery copy. Staged rounds are intentionally absent after restart.

The assistant payload is stored without reconstructing its content blocks. This preserves an
ordered opaque-item seam for provider-native encrypted reasoning or output metadata (#202)
without assigning that metadata new authority in this layer.
