# Teacher Configuration

## Overview

Shammah uses a **student-teacher architecture**:
- **Student**: Local Qwen model with LoRA adapters (learns over time)
- **Teacher**: Cloud LLM provider that the student learns from

The teacher configuration allows you to:
1. Configure multiple cloud providers at once (Claude, OpenAI, Gemini, etc.)
2. Easily switch between them by reordering the array
3. The **first provider in the array is the active teacher**

---

## Configuration Format

### New Format (Array of Teachers)

```toml
# Configure multiple teachers in priority order
# The FIRST one is the active teacher

[[teachers]]
provider = "claude"
api_key = "sk-ant-..."
model = "claude-sonnet-4-20250514"
name = "Claude Sonnet (best quality)"

[[teachers]]
provider = "openai"
api_key = "sk-proj-..."
model = "gpt-4o-mini"
name = "GPT-4o-mini (cheaper)"

[[teachers]]
provider = "gemini"
api_key = "..."
model = "gemini-2.0-flash-exp"
name = "Gemini Flash (fast)"
```

### Legacy Format (Still Supported)

```toml
[teacher]
provider = "claude"

[teacher.claude]
api_key = "sk-ant-..."
model = "claude-sonnet-4-20250514"
```

**Note**: Old configs using `[fallback]` will still work (backwards compatible).

---

## Supported Providers

| Provider | Default Model | Notes |
|----------|---------------|-------|
| `claude` | `claude-sonnet-4-20250514` | Best reasoning, supports prompt caching |
| `openai` | `gpt-4o` | High quality, widely available |
| `grok` | `grok-beta` | X.AI's model, OpenAI-compatible API |
| `gemini` | `gemini-2.0-flash-exp` | Fast, good for multimodal |
| `mistral` | `mistral-large-latest` | European provider |
| `groq` | `llama-3.1-70b-versatile` | Extremely fast inference |

---

## How It Works

### Runtime Behavior

```
User Query
    ↓
Try Local Model (Qwen + LoRA)
    ↓
Is local confident?
    ├─ YES → Return local response (fast, free, private)
    │        ↓
    │        [User can provide feedback for fine-tuning]
    │
    └─ NO  → Ask Teacher (first in [[teachers]] array)
             ↓
             Return teacher's response
             ↓
             Log (query, response) for LoRA training
             ↓
             Background fine-tuning updates local model
             ↓
             Next time: Local model handles it better!
```

### Key Points:

1. **Only ONE teacher is active** (the first in the array)
2. **No automatic failover** between teachers at runtime
3. **Array is for configuration management**, not runtime fallback
4. **Local model learns** from teacher's responses via LoRA

---

## Switching Teachers

### Option 1: Reorder the Array

Move your preferred teacher to the top:

```toml
# GPT-4o-mini is now the active teacher
[[teachers]]
provider = "openai"
api_key = "sk-proj-..."
model = "gpt-4o-mini"
name = "GPT-4o-mini (cheaper) - ACTIVE"

# Claude is now backup (not used)
[[teachers]]
provider = "claude"
api_key = "sk-ant-..."
model = "claude-sonnet-4-20250514"
name = "Claude Sonnet (backup)"
```

### Option 2: Use Setup Wizard (Future)

A future setup wizard will provide a UI to:
- View configured teachers
- Drag-and-drop to reorder
- Set active teacher (moves to top)
- Add/remove teachers

---

## Examples

### Use Case: Cost Optimization

Start with GPT-4o-mini (cheap), switch to Claude for complex tasks:

```toml
[[teachers]]
provider = "openai"
api_key = "sk-proj-..."
model = "gpt-4o-mini"  # $0.15/1M input tokens
name = "GPT-4o-mini (daily driver)"

[[teachers]]
provider = "claude"
api_key = "sk-ant-..."
model = "claude-sonnet-4-20250514"  # $3/1M input tokens
name = "Claude Sonnet (for hard problems)"
```

When you hit a hard problem, reorder the config and restart Shammah.

### Use Case: A/B Testing

Compare two models by switching between them:

```toml
[[teachers]]
provider = "claude"
api_key = "sk-ant-..."
model = "claude-sonnet-4-20250514"
name = "Claude Sonnet (testing)"

[[teachers]]
provider = "openai"
api_key = "sk-proj-..."
model = "gpt-4o"
name = "GPT-4o (testing)"
```

Train LoRA adapters on each, compare which one produces better local model results.

### Use Case: Same Provider, Different Models

```toml
[[teachers]]
provider = "openai"
api_key = "sk-proj-..."
model = "gpt-4o"
name = "GPT-4o (best quality)"

[[teachers]]
provider = "openai"
api_key = "sk-proj-..."
model = "gpt-4o-mini"
name = "GPT-4o-mini (faster/cheaper)"
```

---

## Migration from Old Config

### Old Config:

```toml
[fallback]
provider = "claude"

[fallback.claude]
api_key = "sk-ant-..."
```

### New Config (Equivalent):

```toml
[[teachers]]
provider = "claude"
api_key = "sk-ant-..."
```

**Backwards Compatibility**: Old `[fallback]` configs still work! The loader checks for both `[teacher]` and `[fallback]`.

---

## API

### In Rust Code:

```rust
use finch::config::TeacherConfig;
use finch::providers::create_provider;

// Load config
let config = load_config()?;

// Get active teacher (first in array)
let teacher = create_provider(&config.teacher)?;

// Use teacher
let response = teacher.send_message(&request).await?;
```

### Helper Methods:

```rust
// Get all configured teachers
let teachers = config.teacher.get_teachers();

// Get active teacher
let active = config.teacher.active_teacher();

// Check if using legacy format
let is_legacy = config.teacher.teachers.is_empty();
```

---

## Philosophy

The teacher configuration is designed for:

1. **Flexibility**: Configure multiple providers once, switch easily
2. **Simplicity**: One active teacher at a time, no complex fallback logic
3. **Learning**: Local model learns from teacher's responses
4. **Cost Control**: Switch to cheaper models when appropriate
5. **Experimentation**: A/B test different teachers

**Not designed for**:
- Automatic failover (use simple retry on same provider instead)
- Load balancing across providers
- Runtime provider selection based on query type

---

## Future Enhancements

Potential future features:

1. **Setup Wizard**: Interactive UI for managing teachers
2. **Automatic Switching**: Based on query complexity or cost
3. **Multi-Teacher Training**: Learn from multiple teachers simultaneously
4. **Teacher Comparison**: Built-in A/B testing tools
5. **Cost Tracking**: Monitor spending per teacher

---

## FAQ

**Q: Why array format if only the first teacher is used?**
A: Makes configuration management easy. Configure all your API keys once, switch by reordering.

**Q: What happens if the active teacher fails?**
A: Simple retry logic with exponential backoff (same provider). No automatic switch to next teacher.

**Q: Can I use multiple teachers simultaneously?**
A: No. One conversation = one teacher. Switching mid-conversation loses context.

**Q: Do I need to delete old teacher configs when switching?**
A: No! Keep them all configured. Just reorder the array.

**Q: What's the difference between "teacher" and "fallback"?**
A: Just naming. "Teacher" is more accurate for the student-teacher architecture. "Fallback" is supported for backwards compatibility.
