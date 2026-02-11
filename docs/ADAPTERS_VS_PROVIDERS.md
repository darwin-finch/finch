# LocalModelAdapters vs TeacherProviders

**Clear Separation of Concerns**

## Two Distinct Systems

### 1. LocalModelAdapter (Local ONNX Inference)

**Location**: `src/models/adapters/`
**Purpose**: Handle model-specific behavior for **local ONNX inference**
**Network**: No network calls, pure local formatting
**Async**: No, synchronous operations only

**Responsibilities**:
- Format chat prompts with model-specific templates
- Provide model-specific token IDs (EOS, BOS)
- Clean output artifacts (remove template markers)
- Provide generation config recommendations

**Examples**:
- `QwenAdapter` - ChatML format for Qwen models
- `LlamaAdapter` - Llama 3 format for Llama models
- `MistralAdapter` - Mistral instruction format

**Code**:
```rust
pub trait LocalModelAdapter: Send + Sync {
    fn format_chat_prompt(&self, system: &str, user: &str) -> String;
    fn eos_token_id(&self) -> u32;
    fn clean_output(&self, raw: &str) -> String;
    fn family_name(&self) -> &str;
}
```

**Usage**:
```rust
// Select adapter based on model name
let adapter = AdapterRegistry::get_adapter("Qwen2.5-1.5B-Instruct");

// Format prompt for ONNX inference
let prompt = adapter.format_chat_prompt(system_prompt, user_query);

// Run local ONNX inference
let raw_output = onnx_model.generate(&prompt)?;

// Clean the output
let clean_output = adapter.clean_output(&raw_output);
```

---

### 2. TeacherProvider (External API Services)

**Location**: `src/providers/`
**Purpose**: Make HTTP requests to **external API services** for training/fallback
**Network**: Makes HTTP API calls
**Async**: Yes, all methods are async

**Responsibilities**:
- Send requests to external AI services
- Handle authentication (API keys)
- Stream responses (if supported)
- Manage rate limits and retries

**Examples**:
- `ClaudeProvider` - Anthropic Claude API
- `OpenAIProvider` - OpenAI GPT API
- `GrokProvider` - xAI Grok API
- `GeminiProvider` - Google Gemini API

**Code**:
```rust
pub trait TeacherProvider: Send + Sync {
    async fn send_message(&self, messages: Vec<Message>) -> Result<String>;
    fn provider_name(&self) -> &str;
    fn supports_streaming(&self) -> bool;
}
```

**Usage**:
```rust
// Create teacher provider with API key
let teacher = ClaudeProvider::new(api_key);

// Make async API call
let response = teacher.send_message(messages).await?;

// Learn from high-quality response
local_generator.learn_from_claude(&response);
```

---

## Clear Distinction Table

| Aspect | LocalModelAdapter | TeacherProvider |
|--------|-------------------|----------------|
| **Location** | `src/models/adapters/` | `src/providers/` |
| **Purpose** | Format prompts for local ONNX | Call external AI APIs |
| **Network** | No network, local only | Makes HTTP requests |
| **Async** | No (sync formatting) | Yes (async HTTP) |
| **Cost** | Free (local compute) | Money per API request |
| **Privacy** | Data stays local | Data sent to cloud |
| **Speed** | Fast (microseconds) | Slower (network latency) |
| **Dependencies** | None (just formatting) | HTTP client, API keys |
| **Examples** | Qwen, Llama, Mistral adapters | Claude, OpenAI, Grok APIs |
| **Used For** | Prompt formatting, output cleaning | Training, fallback, complex queries |

---

## Architecture Diagram

```
┌─────────────────────────────────────────────────────────────┐
│                       User Query                             │
└──────────────────────────┬──────────────────────────────────┘
                           │
                           v
                    ┌─────────────┐
                    │   Router    │
                    └──────┬──────┘
                           │
              ┌────────────┴────────────┐
              │                         │
              v                         v
    ┌─────────────────┐       ┌─────────────────┐
    │ Local Path      │       │ Forward Path    │
    │ (95% of queries)│       │ (5% of queries) │
    └────────┬────────┘       └────────┬────────┘
             │                         │
             v                         v
    ┌──────────────────┐      ┌──────────────────┐
    │ LocalModelAdapter│      │ TeacherProvider  │
    │ (Formatting)     │      │ (API Call)       │
    └────────┬─────────┘      └────────┬─────────┘
             │                         │
             v                         v
    ┌──────────────────┐      ┌──────────────────┐
    │ ONNX Runtime     │      │ External API     │
    │ (Local Inference)│      │ (Claude/OpenAI)  │
    └────────┬─────────┘      └────────┬─────────┘
             │                         │
             └────────────┬────────────┘
                          │
                          v
                    ┌─────────────┐
                    │  Response   │
                    └─────────────┘
```

---

## When to Use Each

### Use LocalModelAdapter When:
- ✅ Formatting prompts for local ONNX inference
- ✅ Cleaning output from local model generation
- ✅ Getting model-specific token IDs
- ✅ Switching between model families (Qwen → Llama)

### Use TeacherProvider When:
- ✅ Forwarding complex queries to cloud APIs
- ✅ Learning from high-quality API responses
- ✅ Fallback when local model fails
- ✅ Training local model on expert responses

---

## Adding New Models

### Add a New LocalModelAdapter:

```rust
// src/models/adapters/phi.rs
pub struct PhiAdapter;

impl LocalModelAdapter for PhiAdapter {
    fn format_chat_prompt(&self, system: &str, user: &str) -> String {
        format!("System: {}\n\nUser: {}\n\nAssistant:", system, user)
    }

    fn eos_token_id(&self) -> u32 { 50256 }
    fn family_name(&self) -> &str { "Phi" }
}

// Register in AdapterRegistry
if name_lower.contains("phi") {
    Box::new(PhiAdapter)
}
```

### Add a New TeacherProvider:

```rust
// src/providers/cohere.rs
pub struct CohereProvider {
    api_key: String,
    client: reqwest::Client,
}

impl TeacherProvider for CohereProvider {
    async fn send_message(&self, messages: Vec<Message>) -> Result<String> {
        // Make HTTP request to Cohere API
        let response = self.client
            .post("https://api.cohere.ai/v1/chat")
            .header("Authorization", format!("Bearer {}", self.api_key))
            .json(&request_body)
            .send()
            .await?;
        // Parse and return
    }

    fn provider_name(&self) -> &str { "Cohere" }
    fn supports_streaming(&self) -> bool { true }
}
```

---

## Implementation Notes

### LocalModelAdapter
- **Stateless**: No instance state, can be shared
- **Fast**: Just string formatting, no I/O
- **Deterministic**: Same input always produces same output
- **Testable**: Easy to unit test with examples

### TeacherProvider
- **Stateful**: Has API keys, HTTP clients
- **Slow**: Network I/O, rate limits
- **Variable**: May fail, timeout, rate limit
- **Complex**: Requires mocking for tests

---

## Summary

**LocalModelAdapter = Local ONNX behavior**
**TeacherProvider = External API calls**

These are completely separate concerns with different responsibilities, interfaces, and use cases. The naming makes the distinction clear.

---

**Last Updated**: 2026-02-11
**See Also**:
- `src/models/adapters/mod.rs` - LocalModelAdapter trait
- `src/providers/mod.rs` - TeacherProvider trait
- `CLAUDE.md` - Overall architecture
