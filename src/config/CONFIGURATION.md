# Configuration

**Config file:** `~/.finch/config.toml`

## Format — `[[providers]]`

```toml
[[providers]]
type = "claude"
api_key = "sk-ant-..."
model = "claude-sonnet-4-6"   # optional override

[[providers]]
type = "local"
inference_provider = "onnx"
execution_target = "coreml"   # "coreml" | "cpu"
model_family = "qwen2"
model_size = "medium"         # small=1.5B medium=3B large=7B xlarge=14B
enabled = true

[lora]
rank = 16
alpha = 32.0
learning_rate = 1e-4
batch_size = 4
auto_train = true
auto_train_threshold = 10
high_weight = 10.0
medium_weight = 3.0
normal_weight = 1.0
adapters_dir = "~/.finch/adapters"
```

**Supported `type` values:** `claude`, `openai`, `grok`, `gemini`, `mistral`, `groq`, `local`

**Backwards-compatible:** Old `[[teachers]]` format still loads correctly; auto-rewritten to `[[providers]]` on next save.

## Key files

- `src/config/mod.rs` — Config loading, validation, migration
- `src/config/provider.rs` — `ProviderEntry` tagged enum
- `src/config/settings.rs` — `TeacherEntry` (legacy), `LicenseConfig`, `LicenseType`
