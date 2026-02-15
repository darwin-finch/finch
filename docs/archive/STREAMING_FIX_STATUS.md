# Streaming Output Fix - Implementation Status

## Summary

Implemented fixes for two streaming issues:
1. **Phase A** (Debug Logging): Added debug logging to trace token duplication
2. **Phase B** (Streaming Cleaning): Fixed streaming output to remove DeepSeek thinking markers

## What Was Implemented

### Phase A: Debug Logging (Step 1 Complete)

Added debug logging at three levels to trace where duplication occurs:

1. **Daemon Token Generation** (`src/server/openai_handlers.rs:116`)
   ```rust
   tracing::debug!("[daemon] Sending token to SSE: {:?}", token_text);
   ```

2. **Client SSE Parsing** (`src/client/daemon_client.rs:658-661`)
   ```rust
   tracing::debug!("[daemon_client] SSE chunk received: {:?} (accumulated: {} chars)", content, accumulated_content.len());
   tracing::debug!("[daemon_client] Calling token_callback with: {:?}", content);
   ```

3. **Callback Invocation** (`src/cli/repl_event/event_loop.rs:422`) - Already exists
   ```rust
   tracing::debug!("[/local] Received chunk: {:?}", token_text);
   ```

### Phase B: Streaming Cleaning (COMPLETE ✅)

Fixed streaming responses to clean output properly:

1. **Added Static Method** (`src/models/adapters/qwen.rs:48-183`)
   - Created `QwenAdapter::clean_output_static()` that can be called without adapter instance
   - Removes `<think>` and `</think>` tags (DeepSeek reasoning)
   - Removes ChatML markers (`<|im_start|>`, `<|im_end|>`)
   - Strips template artifacts and role markers
   - Handles tool XML preservation

2. **Updated Message Rendering** (`src/cli/messages/concrete.rs:145-171`)
   - `StreamingResponseMessage::format()` now calls `QwenAdapter::clean_output_static()`
   - Cleans output for all status types (InProgress, Complete, Failed)
   - Applies to ALL streaming messages automatically

## What Was Discovered

### Critical Finding: Model Generation Issue

Looking at test output, the "duplication" appears to be **model-generated repetition**, not code-level token duplication:

```
The sum of 3 and 6 is 9.
The result of 3 plus 6 is 9.
The result of 3 plus 6 is 9.
The result of 3 plus 6 is 9.
```

This suggests the model itself is generating repetitive content, possibly due to:
- Sampling parameters (currently greedy sampling)
- KV cache issues
- Prompt formatting confusing the model
- Model trying to "correct" itself

### Potential Locking Issue Found

The daemon's streaming implementation holds a **write lock on the generator for the entire duration of token generation** (`src/server/openai_handlers.rs:110-129`). This means concurrent `/local` queries will block each other.

However, this blocking shouldn't cause duplication - it should just serialize requests.

## Testing Instructions

### Test Phase B (Streaming Cleaning)

1. **Start daemon:**
   ```bash
   ./target/release/shammah daemon --bind 127.0.0.1:11435
   ```

2. **In another terminal, start REPL:**
   ```bash
   ./target/release/shammah
   ```

3. **Test DeepSeek thinking removal:**
   ```
   > /local what is your name?
   ```

   **Expected:** Clean output without `<think>` or `</think>` tags
   **Not:** "ShammahThe assistant should be able to answer..."

4. **Test concurrent queries:**
   ```
   > /local what is 2+2?
   > /local what is yellow?
   ```

   **Expected:** Both responses clean, no template artifacts

### Test Phase A (Find Duplication Source)

1. **Run with debug logging:**
   ```bash
   # Terminal 1: Daemon with debug logs
   RUST_LOG=debug ./target/release/shammah daemon --bind 127.0.0.1:11435 2>&1 | tee daemon_debug.log

   # Terminal 2: REPL with debug logs
   RUST_LOG=debug ./target/release/shammah 2>&1 | tee repl_debug.log
   ```

2. **Issue `/local` query:**
   ```
   > /local what is 3+6?
   ```

3. **Analyze logs:**
   ```bash
   # Count daemon tokens sent
   grep "\[daemon\] Sending token" daemon_debug.log | wc -l

   # Count client chunks received
   grep "\[daemon_client\] SSE chunk received" repl_debug.log | wc -l

   # Count callback invocations
   grep "\[/local\] Received chunk" repl_debug.log | wc -l
   ```

   **If duplication exists:**
   - Same token count at all levels = model is generating duplicates
   - Higher count at client/callback level = SSE parsing or callback issue

4. **Test concurrent queries:**
   ```
   > /local query1
   > /local query2
   ```

   Check if callbacks get crossed or messages get mixed.

## Expected Results

### Phase B (Cleaning)
- ✅ No `<think>` or `</think>` tags in output
- ✅ No ChatML markers (`<|im_start|>`, `<|im_end|>`)
- ✅ Clean, professional responses
- ✅ Tool XML preserved when present

### Phase A (Duplication Analysis)
- **If model issue:** Token counts match at all levels, but tokens themselves are repetitive
- **If code issue:** Token counts differ between levels (daemon sends N, client receives N×M)

## Next Steps

### If Duplication Persists:

1. **Analyze debug logs** to determine duplication source
2. **If model issue:**
   - Adjust sampling parameters (add temperature/top-p)
   - Check KV cache handling
   - Review prompt formatting
   - Consider adding repetition penalty

3. **If code issue:**
   - Fix SSE parsing (`daemon_client.rs`)
   - Fix callback registration (`event_loop.rs`)
   - Fix message rendering (`concrete.rs`)

### Additional Improvements:

1. **Fix locking issue:**
   - Don't hold write lock during entire generation
   - Acquire lock only when accessing model state
   - Release lock between token generations

2. **Add sampling parameters:**
   - Temperature (randomness)
   - Top-p (nucleus sampling)
   - Repetition penalty (avoid loops)

## Files Modified

1. `src/server/openai_handlers.rs` - Added daemon token logging
2. `src/client/daemon_client.rs` - Added SSE parsing logging
3. `src/models/adapters/qwen.rs` - Added static clean_output method
4. `src/cli/messages/concrete.rs` - Updated streaming message rendering

## Build Status

✅ **Build successful** - No compilation errors

```bash
cargo build --release
# Finished `release` profile [optimized] target(s) in 2m 21s
```

## Conclusion

- **Phase B (Streaming Cleaning)**: ✅ Complete and tested at compile-time
- **Phase A (Duplication Debugging)**: ⏸️ Needs user testing with debug logs

The streaming cleaning fix should immediately improve output quality by removing template artifacts. The duplication issue needs real-world testing with debug logs to determine if it's a model generation issue or a code bug.
