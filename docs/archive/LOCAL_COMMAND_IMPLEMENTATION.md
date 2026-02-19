# `/local` Command Implementation Complete

## Summary

Implemented the `/local` command for direct local model testing and removed active learning tools for privacy. This change gives users full control over when their local model is accessed.

## Changes Made

### Phase 1: Remove Active Learning Tools ✅

**Privacy Fix: Removed Claude's ability to query local model without consent**

**Files Deleted:**
- `src/tools/implementations/query_local.rs`
- `src/tools/implementations/compare_responses.rs`
- `src/tools/implementations/generate_training.rs`
- `src/tools/implementations/analyze_model.rs`
- `src/tools/implementations/train.rs`

**Files Modified:**
- `src/tools/implementations/mod.rs` - Removed module declarations and re-exports
- `src/cli/repl.rs` - Removed tool registrations (lines 186-191, 216-220) and imports

**Impact:** Claude can no longer access the local model via tools. User must explicitly use `/local` command.

### Phase 2: Add `/local` Command Definition ✅

**File:** `src/cli/commands.rs`

**Changes:**
1. Added `Local { query: String }` variant to `Command` enum
2. Added parsing logic for `/local <query>` syntax
3. Updated help text to document the new command
4. Added handler placeholder in `handle_command()` function

**Usage:**
```bash
/local What is 2+2?
/local Explain async/await in Rust
```

### Phase 3: Add Daemon API Support ✅

**File:** `src/server/openai_types.rs`

**Changes:**
Added `local_only` field to `ChatCompletionRequest`:
```rust
/// Bypass routing and query local model directly (for testing)
#[serde(default, skip_serializing_if = "Option::is_none")]
pub local_only: Option<bool>,
```

**File:** `src/server/openai_handlers.rs`

**Changes:**
1. Added check for `local_only` flag in `handle_chat_completions()`:
   ```rust
   if request.local_only.unwrap_or(false) {
       return handle_local_only_query(server, request).await;
   }
   ```

2. Added new handler function `handle_local_only_query()`:
   - Checks generator state (Ready, Initializing, Failed, NotAvailable)
   - Returns appropriate HTTP status codes:
     - `503 SERVICE_UNAVAILABLE` - Model not ready yet
     - `501 NOT_IMPLEMENTED` - Model not available
     - `500 INTERNAL_SERVER_ERROR` - Generation failed
     - `200 OK` - Success
   - Bypasses routing logic (no crisis detection, no threshold checks)
   - Direct local model generation (no tools for simplicity)

### Phase 4: Add CLI Client Method ✅

**File:** `src/client/daemon_client.rs`

**Changes:**
Added `query_local_only()` method:
```rust
pub async fn query_local_only(&self, query: &str) -> Result<String>
```

**Features:**
- Sends request with `local_only: Some(true)` flag
- Handles all error cases with clear error messages:
  - "Local model not ready (initializing/downloading/loading)"
  - "Local model not available"
  - "Local model generation failed: {details}"
- Returns response text on success

**Also fixed:** Added `local_only: None` to two existing `ChatCompletionRequest` initializers to prevent compilation errors.

### Phase 5: Handle `/local` Command in REPL ✅

**File:** `src/cli/repl.rs`

**Changes:**
1. Added command handler in main loop (line ~1421):
   ```rust
   Command::Local { ref query } => {
       self.handle_local_query(query).await?;
       continue;
   }
   ```

2. Added `handle_local_query()` method (line ~2718):
   - Checks if daemon client is available
   - Shows clear error message if daemon not running
   - Queries local model with timing
   - Uses `output_claude()` for response, `output_error()` for errors
   - Displays helpful status messages

**Error Handling:**
- If no daemon: "⚠️  Daemon not available" + instructions to start it
- If model not ready: Shows daemon's 503 error message
- If generation fails: Shows error details with "⚠️  Local model query failed"

## Testing

### Test 1: Active Learning Tools Removed

**Expected:** Claude should NOT have access to:
- QueryLocalModelTool
- CompareResponsesTool
- GenerateTrainingDataTool
- AnalyzeModelTool
- TrainTool

**Verification:**
```bash
finch
> List all available tools
```

Expected tools: Read, Bash, Glob, Grep, WebFetch, Restart, SaveAndExec

### Test 2: `/local` Command Works (Model Ready)

**Setup:**
```bash
# Terminal 1: Start daemon
finch daemon --bind 127.0.0.1:11435

# Terminal 2: Wait for model to load, then test
finch
```

**Test:**
```bash
> /local What is 2+2?
```

**Expected Output:**
```
🔧 Local Model Query (bypassing routing)
4
✓ Local model (0.15s)
```

### Test 3: `/local` Command Error Handling (Model Not Ready)

**Setup:**
```bash
# Start daemon and query immediately
finch daemon --bind 127.0.0.1:11435 &
finch
```

**Test:**
```bash
> /local test
```

**Expected Output:**
```
🔧 Local Model Query (bypassing routing)
Error: Local model not ready (initializing/downloading/loading)
⚠️  Local model query failed
```

### Test 4: `/local` Command Error Handling (No Daemon)

**Setup:**
```bash
# No daemon running
finch
```

**Test:**
```bash
> /local test
```

**Expected Output:**
```
🔧 Local Model Query (bypassing routing)
⚠️  Daemon not available
    Start the daemon: finch daemon --bind 127.0.0.1:11435
```

### Test 5: Normal Queries Still Work

**Test:**
```bash
> What is 2+2?
```

**Expected:** Normal routing (forwards to Claude if model not ready)

### Test 6: Help Text Updated

**Test:**
```bash
> /help
```

**Expected:** Help text includes:
```
/local <query>    - Query local model directly (bypass routing)
```

## Architecture

### Before (Privacy Issue)

```
User Query → Claude (Teacher)
               ↓
         Can use tools:
         - QueryLocalModelTool
         - CompareResponsesTool
         - etc.
               ↓
         Accesses local model without user consent
```

### After (Privacy Fixed)

```
User Query → Router → Crisis? → Forward to Claude
                   ↓
                   No → Threshold? → Local/Forward

User /local → Daemon API (local_only=true)
                   ↓
              Direct local model (bypass routing)
                   ↓
              User gets raw model output
```

## Benefits

1. **Privacy**: User has full control over local model access
2. **Testing**: Can test raw model behavior without routing filters
3. **Transparency**: Clear feedback about model state and errors
4. **Simplicity**: Single command instead of complex tools
5. **Safety**: Normal queries still have crisis detection and routing

## Design Decisions

### Why Remove Tools Instead of Disable?

**Decision:** Completely remove tool files

**Rationale:**
- Clean removal for privacy (no accidental re-enable)
- Can restore from git if needed later
- Simpler than conditional compilation or feature flags
- Clear message: local model access is user-controlled only

### Why `local_only: bool` Field?

**Decision:** Use optional field in `ChatCompletionRequest`

**Rationale:**
- Maintains OpenAI API compatibility
- Easy to extend later (e.g., add `force_forward` flag)
- Backward compatible (defaults to None/false)
- Simpler than creating a new endpoint

### Why Skip Crisis Detection for `/local`?

**Decision:** Direct model access, no filtering

**Rationale:**
- Command is for testing raw model behavior
- User explicitly wants unfiltered output
- Privacy-safe (no code sent to external API)
- User can see what model actually produces

### Why Error on Model Not Ready?

**Decision:** Don't wait, return error immediately

**Rationale:**
- User can see status and retry manually
- Blocking/waiting would be confusing in REPL
- Consistent with HTTP semantics (503 = try again later)
- Provides clear feedback about system state

## Implementation Status

✅ Phase 1: Remove Active Learning Tools
✅ Phase 2: Add `/local` Command Definition
✅ Phase 3: Add Daemon API Support
✅ Phase 4: Add CLI Client Method
✅ Phase 5: Handle `/local` Command in REPL

**Compilation Status:** ✅ No errors related to new code
**Pre-existing Errors:** Some unrelated `GeneratorState` enum matching issues

## Next Steps

1. Test with actual daemon and local model
2. Add integration test for `/local` command
3. Update user documentation (README.md)
4. Consider adding `/local` to completion/suggestions

## Files Modified

- `src/tools/implementations/mod.rs` - Removed tool modules
- `src/cli/repl.rs` - Removed tool registrations, added command handler
- `src/cli/commands.rs` - Added command definition and parsing
- `src/server/openai_types.rs` - Added `local_only` field
- `src/server/openai_handlers.rs` - Added local-only handler
- `src/client/daemon_client.rs` - Added client method, fixed initializers

## Files Deleted

- `src/tools/implementations/query_local.rs`
- `src/tools/implementations/compare_responses.rs`
- `src/tools/implementations/generate_training.rs`
- `src/tools/implementations/analyze_model.rs`
- `src/tools/implementations/train.rs`

---

**Implementation completed on:** 2026-02-11
**Total changes:** 6 files modified, 5 files deleted
**Lines added:** ~200
**Lines removed:** ~500 (tool implementations)
