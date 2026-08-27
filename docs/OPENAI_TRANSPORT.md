# Outbound OpenAI transport

Finch selects its outbound wire contract from the exact endpoint and model,
not from the provider display name alone.

- `https://api.openai.com/v1/chat/completions` with `gpt-5.6-sol` (or its
  `gpt-5.6` alias) uses Finch's canonical GPT-5.6 Chat Completions contract.
  It sends developer instructions, `max_completion_tokens`, reasoning effort,
  function calls/results, and typed PNG/JPEG `image_url` data URLs up to 8 MB.
  PNG CRC and compressed image data are verified before network access.
  Streaming requests disable obfuscation explicitly, and require one known
  terminal status, one terminal usage-only chunk, and exactly one `[DONE]`
  marker. The first valid stream event also publishes the provider-reported
  actual model through the provider-neutral stream metadata chunk; the same
  model field is returned by the non-streaming path and survives daemon IPC.
- GPT-4o and OpenAI-compatible xAI, Groq, Mistral, Ollama, remote-Finch, and
  custom endpoints retain the historical compatible request and parser shape.

Chat Completions is intentionally the only protocol Finch advertises for this
path. The Responses API is preferred by OpenAI for GPT-5.6 reasoning, tools,
and multi-turn workflows, but correct stateless continuation requires retaining
every response output item (including encrypted reasoning items) or a durable
`previous_response_id`. Finch's current provider-neutral history has neither
representation. Adding a provider cursor here would incorrectly make mutable
upstream state authoritative over Brain history. Responses support must wait
for the atomic-history and run-provenance abstractions to define that durable
representation; Finch does not claim Responses continuity in the meantime.
Consumers without a durable response-provenance owner may explicitly ignore
the optional stream metadata chunk; it is not a Responses cursor or history
authority.

The canonical boundary validates images, call IDs, tool arguments, request
size, SSE line/event/total size, response object, actual model consistency,
usage chunks, and terminal state. Errors never include canonical upstream
response bodies, prompts, images, tool arguments, credentials, or reasoning
payloads.
