# Teacher Context Optimization

## Overview

The `TeacherSession` module provides three levels of context optimization to minimize token costs when calling teacher providers (Claude, OpenAI, Gemini, etc.):

1. **Level 1 (Minimal)**: Tracking and metrics
2. **Level 2 (Basic)**: Optional context truncation
3. **Level 3 (Full)**: Smart optimization strategies

---

## Level 1: Minimal - Tracking & Metrics

**What it does**: Tracks what context has been sent to the teacher and logs metrics about new vs repeated messages.

**Use case**: Understand token usage without changing behavior.

### Usage:

```rust
use finch::providers::{TeacherSession, create_provider};

// Create provider
let teacher_provider = create_provider(&config.teacher)?;

// Wrap in TeacherSession
let mut session = TeacherSession::new(teacher_provider);

// Send message (tracks metrics automatically)
let response = session.send_message(&request).await?;
```

### What it logs:

```
INFO teacher="claude" call_count=2 total_messages=8 new_messages=2 repeated_messages=6
     estimated_total_tokens=800 estimated_new_tokens=200 estimated_cached_tokens=600
     "Teacher context metrics"
```

### API:

```rust
// Get current state
let state = session.state();
println!("Teacher called {} times", state.teacher_call_count);
println!("Total tokens sent: {}", state.total_input_tokens);
println!("Estimated cached: {}", state.estimated_cached_tokens);

// Get optimization stats
let stats = session.optimization_stats();
println!("Savings: {:.1}%", stats.estimated_savings_percent);

// Reset state (for new conversation)
session.reset_state();
```

---

## Level 2: Basic - Optional Truncation

**What it does**: Optionally truncates conversation to recent N turns, always preserving system prompts.

**Use case**: Long conversations where old context isn't needed.

### Configuration:

```toml
[teacher]
max_context_turns = 10  # Only send last 10 turns (0 = unlimited)

[[teachers]]
provider = "claude"
api_key = "..."
```

### Usage:

```rust
use finch::providers::{TeacherSession, TeacherContextConfig};

// Configure truncation
let config = TeacherContextConfig {
    max_context_turns: 10,  // Keep last 10 turns
    tool_result_retention_turns: 0,  // Don't drop tool results yet
    prompt_caching_enabled: true,
};

let mut session = TeacherSession::with_config(teacher_provider, config);

// Send with truncation
let response = session.send_message_with_truncation(&request).await?;
```

### What it logs:

```
INFO teacher="claude" total_messages=50 sent_messages=20 dropped_messages=30 max_turns=10
     "Context truncated to save tokens"
```

### Behavior:

- **Keeps**: System messages (always) + last N turns
- **Drops**: Old conversation history
- **Preserves**: Recent context for continuity

**Example:**

```
Original (50 messages):
  - System prompt
  - Turn 1-20 (old conversation)
  - Turn 21-25 (recent conversation)

Truncated with max_context_turns=5 (20 messages):
  - System prompt ← preserved
  - Turn 21-25 ← recent 5 turns kept
```

---

## Level 3: Full - Smart Strategies

**What it does**: Applies multiple optimization strategies:
1. Preserves system prompts
2. Drops old tool results (keeps tool calls, just removes large result payloads)
3. Truncates to recent turns
4. Optimizes for prompt caching

**Use case**: Maximum token savings for cost-sensitive deployments.

### Configuration:

```toml
[teacher]
max_context_turns = 15              # Keep last 15 turns
tool_result_retention_turns = 5     # Only keep last 5 turns of tool results

[[teachers]]
provider = "claude"
api_key = "..."
```

### Usage:

```rust
let config = TeacherContextConfig {
    max_context_turns: 15,
    tool_result_retention_turns: 5,  // Drop tool results older than 5 turns
    prompt_caching_enabled: true,
};

let mut session = TeacherSession::with_config(teacher_provider, config);

// Send with full optimization
let response = session.send_message_with_optimization(&request).await?;
```

### What it logs:

```
INFO teacher="claude" original_messages=40 optimized_messages=25 dropped=15
     original_tool_results=8 optimized_tool_results=3 dropped_tool_results=5
     "Context optimized with smart strategies"
```

### Strategies Explained:

#### 1. **Drop Old Tool Results**

Tool results (file contents, command output, etc.) can be huge. This strategy:
- **Keeps**: Tool calls (ToolUse blocks) from all messages
- **Keeps**: Tool results from recent N turns
- **Drops**: Tool results from older turns

**Why**: The teacher needs to see what tools were called, but doesn't need the full output from 20 turns ago.

**Example:**

```
Turn 5 (old):
  Before: ToolUse(Read, "src/main.rs") + ToolResult(tool_use_id, "fn main() { ... 5000 chars ...}")
  After:  ToolUse(Read, "src/main.rs")  ← Kept call, dropped 5000-char result

Turn 15 (recent):
  Before: ToolUse(Read, "src/lib.rs") + ToolResult(tool_use_id, "pub fn test() { ... }")
  After:  ToolUse(Read, "src/lib.rs") + ToolResult(...)  ← Both kept
```

**Token savings**: Can save 1000+ tokens per dropped tool result!

#### 2. **Preserve System Prompts**

System prompts define the teacher's behavior and should never be dropped.

```
Original conversation:
  - System: "You are a helpful coding assistant..."
  - [40 turns of conversation]

Truncated to 10 turns:
  - System: "You are a helpful coding assistant..."  ← Always preserved
  - [Last 10 turns]
```

#### 3. **Truncate to Recent Turns**

After dropping tool results, truncate to `max_context_turns` most recent turns.

---

## Choosing the Right Level

| Level | Token Savings | Context Loss | Use Case |
|-------|---------------|--------------|----------|
| **Level 1 (Minimal)** | 0% (metrics only) | None | Understanding costs, debugging |
| **Level 2 (Basic)** | 30-60% | Some (old messages) | Long conversations, summarization tasks |
| **Level 3 (Full)** | 60-85% | Moderate (old tool results) | Cost-sensitive, tool-heavy conversations |

### Recommendation:

- **Start with Level 1**: Understand your baseline token usage
- **Try Level 2**: If conversations exceed 20+ turns regularly
- **Use Level 3**: If tool-heavy conversations (lots of file reads, command outputs)

---

## Cost Impact Examples

### Scenario: 10-turn conversation with tool calls

**Without optimization:**
- 20 messages (10 turns)
- 5 tool results (avg 2000 tokens each)
- Total: ~12,000 tokens/request
- Cost (Claude @ $3/1M): **$0.036 per request**

**With Level 2 (truncate to 5 turns):**
- 10 messages (5 turns)
- 3 tool results
- Total: ~7,000 tokens/request
- Cost: **$0.021 per request** (42% savings)

**With Level 3 (truncate + drop old tool results):**
- 10 messages (5 turns)
- 2 tool results (dropped 1 old result)
- Total: ~5,000 tokens/request
- Cost: **$0.015 per request** (58% savings)

**For 100 requests/day:**
- Without optimization: $3.60/day
- With Level 2: $2.10/day (**$1.50/day saved**)
- With Level 3: $1.50/day (**$2.10/day saved**)

---

## Integration Example

### In main application:

```rust
use finch::providers::{TeacherSession, TeacherContextConfig, create_provider};
use finch::config::load_config;

#[tokio::main]
async fn main() -> Result<()> {
    // Load config
    let config = load_config()?;

    // Create teacher provider
    let teacher_provider = create_provider(&config.teacher)?;

    // Configure context optimization
    let optimization_config = TeacherContextConfig {
        max_context_turns: config.teacher.max_context_turns.unwrap_or(15),
        tool_result_retention_turns: config.teacher.tool_result_retention.unwrap_or(5),
        prompt_caching_enabled: true,
    };

    // Create session
    let mut teacher_session = TeacherSession::with_config(
        teacher_provider,
        optimization_config,
    );

    // Use in conversation loop
    loop {
        let user_input = get_user_input()?;

        // Try local model first
        if let Some(response) = local_model.try_generate(&user_input)? {
            println!("{}", response);
            continue;
        }

        // Ask teacher with optimization
        let request = build_request(&conversation_history);
        let response = teacher_session
            .send_message_with_optimization(&request)
            .await?;

        println!("{}", response.text());

        // Log for training
        training_coordinator.add_example(&user_input, &response);
    }
}
```

---

## Monitoring & Metrics

### Check optimization stats:

```rust
// After some conversation
let stats = teacher_session.optimization_stats();

println!("Teacher Stats:");
println!("  Calls: {}", stats.teacher_call_count);
println!("  Total tokens: {}", stats.total_input_tokens);
println!("  Cached tokens: {}", stats.estimated_cached_tokens);
println!("  Savings: {:.1}%", stats.estimated_savings_percent);
```

### Example output:

```
Teacher Stats:
  Calls: 15
  Total tokens: 45,000
  Cached tokens: 32,000
  Savings: 71.1%
```

---

## Advanced Configuration

### Different configs for different scenarios:

```rust
// For exploratory coding (tool-heavy)
let exploratory_config = TeacherContextConfig {
    max_context_turns: 10,
    tool_result_retention_turns: 3,  // Drop tool results aggressively
    prompt_caching_enabled: true,
};

// For long discussions (text-heavy)
let discussion_config = TeacherContextConfig {
    max_context_turns: 20,           // More context
    tool_result_retention_turns: 0,  // No tool results to drop
    prompt_caching_enabled: true,
};

// Switch based on conversation type
let config = if conversation_has_tools() {
    exploratory_config
} else {
    discussion_config
};
```

---

## Best Practices

1. **Start conservative**: Use large limits initially, reduce as needed
2. **Monitor metrics**: Check `optimization_stats()` regularly
3. **Preserve critical context**: System prompts are always kept
4. **Test quality**: Ensure truncation doesn't hurt response quality
5. **Combine with caching**: Claude/Gemini prompt caching works automatically

---

## Limitations

- **Token estimates are approximate**: Based on ~100 tokens/message heuristic
- **Can't recover dropped context**: Once truncated, it's gone for that request
- **Quality trade-off**: Aggressive truncation may reduce response quality
- **Tool dependencies**: Dropping tool results might break references

---

## Future Enhancements

Potential improvements:
- [ ] Actual token counting (via tokenizer)
- [ ] Smart context compression (summarize instead of drop)
- [ ] Context importance scoring (keep "important" old messages)
- [ ] Automatic config tuning based on quality metrics
- [ ] Per-provider optimization profiles

---

## FAQ

**Q: Does this work with all providers?**
A: Yes! Level 1-3 work with any provider. Prompt caching benefits are automatic for Claude/Gemini.

**Q: Will truncation break my conversation?**
A: Potentially. Test your use case. Start with Level 1 (metrics only), then try Level 2 with conservative limits.

**Q: Should I always use Level 3?**
A: No. Use Level 1 first to understand your usage. Level 3 is for cost-sensitive deployments with tool-heavy workloads.

**Q: Can I customize what gets dropped?**
A: Not yet. Currently drops: (1) old messages, (2) old tool results. Future versions may add custom strategies.

**Q: Does this affect the local model?**
A: No. This only affects teacher API calls. Local model always sees full conversation.
