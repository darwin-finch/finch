# Automatic Training Collection & Adapter Reload

**Implemented**: 2026-02-11
**Status**: ✅ Auto-collection works | ⚠️ Adapter reload placeholder

## What Was Added

### 1. Automatic Collection (✅ Complete)

**Every query/response is now automatically collected for training**

```
User query → Daemon responds → Auto-collected (weight: 1.0)
                                      ↓
                            Training queue counter++
                                      ↓
                            Every 100 examples → Train!
```

#### Implementation

**File**: `src/server/openai_handlers.rs`

```rust
// After generating response
if !has_tool_calls(&content_blocks) {
    let example = WeightedExample {
        query: user_query.to_string(),
        response: response_text,
        weight: 1.0,  // Normal weight
        feedback: None,
    };

    training_tx.send(example)?;  // Non-blocking
}
```

**What Gets Collected**:
- ✅ User query (last user message)
- ✅ Response text (extracted from content blocks)
- ✅ Weight: 1.0 (normal priority)
- ❌ Tool calls (skipped - not useful for training)

**When Training Triggers**:
- Every 100 collected examples (configurable via `batch_threshold`)
- Or after timeout (e.g., 60 minutes with partial batch)

### 2. Adapter Reload (⚠️ Placeholder)

**Checks for newer adapters before each generation**

```
try_generate_with_tools() called
    ↓
check_and_reload_adapter()
    ↓
Find latest adapter in ~/.finch/adapters/
    ↓
⚠️  Log found adapter (reload not implemented yet)
    ↓
Continue with generation
```

#### Implementation

**File**: `src/local/mod.rs`

```rust
fn check_and_reload_adapter(&mut self) -> Result<()> {
    // Find latest .safetensors file
    if let Some(latest_adapter) = self.find_latest_adapter(&adapters_dir)? {
        // TODO: Actually load the adapter
        debug!("Found adapter: {} (reload not implemented)", latest_adapter.display());
    }
    Ok(())
}
```

**Why Not Fully Implemented**:
- ONNX Runtime doesn't have built-in LoRA adapter support
- Need to implement adapter merging/swapping logic
- Risk of memory leaks or crashes during hot-swap
- Need validation before switching

## Current Workflow

### Automatic Training

```bash
# 1. Start daemon
finch daemon --bind 127.0.0.1:8080

# 2. Use it (queries auto-collected)
curl http://127.0.0.1:8080/v1/chat/completions -d '{
  "messages": [{"role": "user", "content": "What is 2+2?"}]
}'

# 3. After 100 queries → training starts automatically!
# Background: Python subprocess trains LoRA adapter
# Saved to: ~/.finch/adapters/adapter_<timestamp>.safetensors

# 4. ⚠️  Adapter saved but not used yet
# Need to manually restart daemon or implement hot-swap
```

### Manual Feedback (Higher Weight)

Users can still provide explicit feedback for important corrections:

```bash
POST /v1/feedback
{
  "query": "What is 2+2?",
  "response": "5",
  "weight": "high",  # 10x weight!
  "feedback": "Wrong answer! Should be 4"
}
```

## Configuration

```toml
# ~/.finch/config.toml
[lora]
enabled = true
auto_train_threshold = 100  # Train every N examples
batch_timeout_minutes = 60   # Or after timeout

# Weights for different priorities
normal_weight = 1.0   # Auto-collected
medium_weight = 3.0   # Explicit feedback (medium)
high_weight = 10.0    # Critical corrections
```

## What's Next

### To Make Adapter Reload Actually Work:

**Option 1: Lazy Reload on Daemon Restart**
- ✅ Simple and safe
- ✅ Works immediately
- ❌ Requires restart

```rust
// On daemon startup
fn load_generator() -> Result<LocalGenerator> {
    let mut gen = LocalGenerator::new();
    if let Some(adapter) = find_latest_adapter()? {
        gen.load_adapter(adapter)?;
    }
    Ok(gen)
}
```

**Option 2: Hot-Swap on Next Query**
- ✅ No restart needed
- ✅ Automatic
- ⚠️ Need ONNX adapter loading

```rust
fn check_and_reload_adapter(&mut self) -> Result<()> {
    if has_newer_adapter(&self.last_adapter_timestamp)? {
        let adapter = find_latest_adapter()?;
        self.load_and_merge_adapter(adapter)?;  // TODO: Implement
        self.last_adapter_timestamp = now();
    }
    Ok(())
}
```

**Option 3: Background Watcher**
- ✅ Most automatic
- ✅ Immediate updates
- ❌ Most complex

```rust
// Separate task watches filesystem
tokio::spawn(async move {
    let mut watcher = notify::watcher()?;
    watcher.watch("~/.finch/adapters", Recursive)?;

    for event in watcher {
        if event.kind == EventKind::Create {
            reload_adapter_in_generator(event.path)?;
        }
    }
});
```

### Recommended: Option 1 (Restart) + Option 2 (Hot-swap)

1. **Short-term**: Load latest adapter on daemon startup (easy win)
2. **Long-term**: Implement hot-swap for seamless updates

## Benefits

### Automatic Collection
✅ No manual feedback needed (learns from all interactions)
✅ Builds training dataset naturally
✅ 100 examples = 1 training run (adapter improves regularly)
✅ Non-blocking (doesn't slow down queries)

### Weighted System Still Works
✅ Auto-collected: weight 1.0 (normal)
✅ Explicit feedback: weight 3.0-10.0 (prioritized)
✅ Critical corrections get 10x more impact
✅ Model learns faster from important feedback

## Testing

```bash
# Start daemon
./target/release/finch daemon --bind 127.0.0.1:8080

# Send 100 queries (will trigger training)
for i in {1..100}; do
  curl -s http://127.0.0.1:8080/v1/chat/completions \
    -H "Content-Type: application/json" \
    -d "{\"messages\": [{\"role\": \"user\", \"content\": \"Query $i\"}]}"
done

# Check training queue
cat ~/.finch/training_queue.jsonl | wc -l  # Should be 100

# Check if adapter created
ls -lt ~/.finch/adapters/

# Restart daemon to use new adapter (once loading is implemented)
pkill finch
./target/release/finch daemon --bind 127.0.0.1:8080
```

## Files Modified

- `src/server/openai_handlers.rs` - Auto-collection logic
- `src/local/mod.rs` - Adapter reload placeholder

## Logs

When auto-collection happens:

```
DEBUG finch::server::openai_handlers: Auto-collected query/response for training
```

When adapter found:

```
DEBUG finch::local: Found adapter: /Users/user/.finch/adapters/adapter_2026-02-11.safetensors (reload not yet implemented)
```

When training triggers:

```
INFO finch::server::training_worker: Batch threshold reached, triggering training count=100
INFO finch::server::training_worker: Starting LoRA training subprocess
```

---

**Summary**: Auto-collection ✅ works! Adapter reload ⚠️ needs ONNX implementation.
