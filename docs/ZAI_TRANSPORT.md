# Z.ai transport

Finch models Z.ai as a first-class provider dialect. It is not an OpenAI
credential alias even though Z.ai exposes an OpenAI-SDK-compatible Chat
Completions surface.

The dated contract used by Finch is:

- API base: `https://api.z.ai/api/paas/v4`
- chat path: `/chat/completions`
- model catalogue path: `/models`
- model: `glm-5.3-flash`
- authentication: bearer API key
- reasoning efforts: `low`, `high`, or `max`
- maximum context: 1,000,000 tokens
- maximum output: 131,072 tokens

Sources reviewed on 2026-08-27:

- <https://docs.z.ai/api-reference/llm/chat-completion>
- <https://docs.z.ai/guides/vlm/glm-5.3-flash>
- <https://docs.z.ai/guides/develop/openai/python>

## Thinking continuity

GLM-5.3-Flash always thinks. Finch sends `thinking.type = "enabled"` and
`thinking.clear_thinking = true`. Z.ai documents `true` as the default and as
the mode that ignores prior `reasoning_content` while retaining visible text,
tool calls, and tool results.

Finch accepts current-turn `reasoning_content` only inside the bounded provider
response parser, then discards it. It is not shown to the user, logged, or
committed as authoritative Brain history. Finch must not set
`clear_thinking = false` until its provider projection can retain the complete,
unmodified, correctly ordered reasoning history without making that projection
authoritative.

## Named credential configuration

The built-in resolver reads an explicitly named environment variable. The API
key is not stored in `config.toml`:

```toml
[[credentials]]
name = "zai-work"
kind = "api_key"
provider = "zai"
issuer = "zai"
secret_ref = "env:ZAI_API_KEY"
scopes = []

[credentials.audience]
family = "zai_api"

[credentials.lifecycle]
state = "active"
refreshable = false

[[providers]]
type = "credentialed"
provider = "zai"
model = "glm-5.3-flash"
name = "zai-flash"
reasoning_effort = "max"

[providers.credential]
credential_ref = "zai-work"
required_scopes = []
```

Provider/credential mismatches fail during graph validation before secret
resolution, HTTP-client construction, DNS, or socket activity.

## Conformance status

Static tests cover exact endpoint composition, request fields, reasoning
policy, images, bounded reasoning projection, tools, and credential audience
binding. Finch must not advertise this preset as live-conformant until the
opt-in API-key matrix in issue #196 passes and the dated result is recorded in
issue #98.
