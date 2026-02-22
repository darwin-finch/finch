# Multi-Provider Configuration

Finch supports multiple cloud AI providers (Claude, OpenAI, Grok, Gemini, Mistral, Groq) and the
local ONNX model, all configured through a unified `[[providers]]` array in `~/.finch/config.toml`.

The easiest way to configure providers is the interactive setup wizard:

```bash
finch setup
```

## Configuration Format

Providers are declared as a TOML array of tables. Each entry has a `type` field that identifies
the provider, plus provider-specific fields.

### Claude (Anthropic)

```toml
[[providers]]
type = "claude"
api_key = "sk-ant-..."
model = "claude-sonnet-4-6"   # optional — default: claude-sonnet-4-6
name = "Claude"               # optional display name
```

Get an API key: https://console.anthropic.com/

### OpenAI

```toml
[[providers]]
type = "openai"
api_key = "sk-proj-..."
model = "gpt-4o"              # optional — default: gpt-4o
```

Get an API key: https://platform.openai.com/

### Grok (xAI)

```toml
[[providers]]
type = "grok"
api_key = "xai-..."
model = "grok-code-fast-1"    # optional — default: grok-beta
```

Get an API key: https://console.x.ai/
Also available via X Premium+ subscription.

### Gemini (Google)

```toml
[[providers]]
type = "gemini"
api_key = "AIza..."
model = "gemini-2.0-flash-exp"  # optional
```

Get an API key: https://aistudio.google.com/apikey

### Mistral

```toml
[[providers]]
type = "mistral"
api_key = "..."
model = "mistral-large-latest"  # optional
```

### Groq

```toml
[[providers]]
type = "groq"
api_key = "gsk_..."
model = "llama-3.3-70b-versatile"  # optional
```

### Local Model (ONNX)

```toml
[[providers]]
type = "local"
inference_provider = "onnx"
execution_target = "coreml"   # "coreml" (Apple Silicon) | "cpu"
model_family = "qwen2"
model_size = "medium"         # "small"=1.5B "medium"=3B "large"=7B "xlarge"=14B
enabled = true
```

## Multi-Provider Example

You can list multiple cloud providers. The first one in the array is the active provider;
use `/teacher <name>` in the REPL or re-run `finch setup` to switch.

```toml
[[providers]]
type = "claude"
api_key = "sk-ant-..."
model = "claude-sonnet-4-6"

[[providers]]
type = "grok"
api_key = "xai-..."
model = "grok-code-fast-1"
name = "Grok (fast)"

[[providers]]
type = "openai"
api_key = "sk-proj-..."

[[providers]]
type = "local"
inference_provider = "onnx"
execution_target = "coreml"
model_family = "qwen2"
model_size = "medium"
enabled = true
```

## How Provider Selection Works

1. **Startup**: Finch reads all `[[providers]]` entries.
2. **Active provider**: The first cloud entry with a non-empty `api_key` is the default teacher.
3. **Local model**: The `local` entry runs in the background; the REPL routes to it when ready.
4. **Runtime switching**: `/model list` and `/model <name>` change the active provider mid-session.
5. **Tool execution**: All providers support tool calling.

## Migration from the Old `[fallback]` Format

The old format is still accepted and migrates transparently:

```toml
# Old format (still works, auto-migrated on save)
[fallback]
provider = "claude"

[fallback.claude]
api_key = "sk-ant-..."
```

This is equivalent to:

```toml
# New format
[[providers]]
type = "claude"
api_key = "sk-ant-..."
```

The file is rewritten to the new format the next time config is saved (e.g. after `finch setup`).

## Provider Capabilities

| Provider | Streaming | Tool Calling | Notes |
|----------|-----------|--------------|-------|
| Claude   | ✅        | ✅           | Primary; best tool use quality |
| OpenAI   | ✅        | ✅           | GPT-4o default |
| Grok     | ✅        | ✅           | Fast; good for code |
| Gemini   | ✅        | ✅           | Free tier available |
| Mistral  | ✅        | ✅           | EU-hosted option |
| Groq     | ✅        | ✅           | Very fast inference |
| Local    | ✅        | ✅ (limited) | No API cost; requires download |

## Architecture

All providers implement the `LlmProvider` trait. The factory in `src/providers/factory.rs`
converts `ProviderEntry` values into provider instances:

```
[[providers]] entries in config.toml
    ↓
Config::with_providers(Vec<ProviderEntry>)
    ↓
create_providers_from_entries(&[ProviderEntry])
    ↓
Vec<Arc<dyn LlmProvider>>
    ↓
Active provider selected by index
    ↓
ClaudeClient::with_provider(provider)
```

**Key files:**
- `src/config/provider.rs` — `ProviderEntry` tagged enum with conversion helpers
- `src/providers/factory.rs` — `create_provider_from_entry()`, `create_providers_from_entries()`
- `src/providers/mod.rs` — `LlmProvider` trait
