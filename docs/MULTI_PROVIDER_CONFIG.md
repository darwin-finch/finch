# Multi-Provider LLM Configuration

Shammah now supports multiple LLM providers for the fallback API (used when the local Qwen model is not ready or not confident). You can choose between Claude, OpenAI, Grok, and more.

## Configuration File Location

Configuration is stored in `~/.finch/config.toml`

## Configuration Format

### Using Claude (Default)

```toml
api_key = "sk-ant-..."  # Kept for backwards compatibility
streaming_enabled = true

[fallback]
provider = "claude"

[fallback.claude]
api_key = "sk-ant-api03-..."
model = "claude-sonnet-4-20250514"  # Optional: override default model
```

### Using OpenAI

```toml
api_key = "sk-ant-..."  # For backwards compatibility (ignored if fallback is configured)
streaming_enabled = true

[fallback]
provider = "openai"

[fallback.openai]
api_key = "sk-proj-..."
model = "gpt-4o"  # Optional: defaults to gpt-4o
```

### Using Grok (X.AI)

```toml
streaming_enabled = true

[fallback]
provider = "grok"

[fallback.grok]
api_key = "xai-..."
model = "grok-beta"  # Optional: defaults to grok-beta
```

### Using Gemini (Google)

```toml
streaming_enabled = true

[fallback]
provider = "gemini"

[fallback.gemini]
api_key = "AIza..."
model = "gemini-2.0-flash-exp"  # Optional: defaults to gemini-2.0-flash-exp
```

## Multi-Provider Configuration

You can configure multiple providers and switch between them by changing the `provider` field:

```toml
streaming_enabled = true

[fallback]
provider = "claude"  # Change to "openai", "grok", "gemini", etc.

# Configure all your providers
[fallback.claude]
api_key = "sk-ant-..."
model = "claude-sonnet-4-20250514"

[fallback.openai]
api_key = "sk-proj-..."
model = "gpt-4o"

[fallback.grok]
api_key = "xai-..."
model = "grok-beta"

[fallback.gemini]
api_key = "AIza..."
model = "gemini-2.0-flash-exp"
```

## Provider Details

### Claude (Anthropic)

- **API Key**: Get from https://console.anthropic.com/
- **Default Model**: `claude-sonnet-4-20250514`
- **Supports**: Streaming, tool calling
- **Cost**: $3/MTok input, $15/MTok output (Sonnet 4)

### OpenAI

- **API Key**: Get from https://platform.openai.com/
- **Default Model**: `gpt-4o`
- **Supports**: Streaming, tool calling
- **Cost**: $2.50/MTok input, $10/MTok output (GPT-4o)

### Grok (X.AI)

- **API Key**: Get from https://console.x.ai/
- **Default Model**: `grok-beta`
- **Supports**: Streaming, tool calling
- **Cost**: Check X.AI pricing

### Gemini (Google)

- **API Key**: Get from https://aistudio.google.com/apikey
- **Default Model**: `gemini-2.0-flash-exp`
- **Supports**: Streaming, tool calling (function declarations)
- **Cost**: Free tier available, check Google AI Studio pricing
- **Note**: Experimental models may have rate limits

## How Provider Selection Works

1. **Startup**: Shammah reads the configuration file
2. **Provider Creation**: Creates the configured provider (Claude, OpenAI, Grok, etc.)
3. **Routing**: When local model is not ready or not confident, routes to the configured provider
4. **Tool Execution**: All providers support tool calling, so tools work regardless of provider

## Migration Guide

### From Old Config (Claude Only)

**Old format:**
```toml
api_key = "sk-ant-..."
streaming_enabled = true
```

**New format (equivalent):**
```toml
api_key = "sk-ant-..."  # Kept for backwards compatibility
streaming_enabled = true

[fallback]
provider = "claude"

[fallback.claude]
api_key = "sk-ant-..."
```

The old format still works! If no `[fallback]` section is present, Shammah will use the `api_key` field for Claude.

## Troubleshooting

### Error: "No settings found for provider"

Make sure you have a `[fallback.{provider}]` section with an `api_key` field:

```toml
[fallback]
provider = "openai"

[fallback.openai]
api_key = "sk-proj-..."  # Don't forget this!
```

### Error: "Unknown provider"

Check that the `provider` field matches one of: `"claude"`, `"openai"`, `"grok"`, `"gemini"`.

### Testing Provider Configuration

Run a simple query to test your provider configuration:

```bash
finch query "What is 2+2?"
```

Check the logs to see which provider was used.

## Future Providers

Potential additions:
- Custom OpenAI-compatible endpoints (e.g., local LLMs with OpenAI API)
- Azure OpenAI
- Anthropic Claude via AWS Bedrock
- Cohere Command models

## Architecture

Under the hood, Shammah uses a provider abstraction layer:

```
User Request
    ↓
Local Qwen Model (if ready and confident)
    ↓ (fallback)
Provider Factory
    ↓
ClaudeProvider / OpenAIProvider / GrokProvider
    ↓
LLM API
    ↓
Response to User
```

All providers implement the same `LlmProvider` trait, ensuring consistent behavior across different APIs.
