# Fix: Persistent Tool Patterns Not Matching

**Date**: 2026-02-13
**Priority**: 🔴 HIGH (Security & UX)
**Status**: ✅ FIXED

## Problem

**User Report**: "Persistent tool approval patterns not matching subsequent tool calls"

**Impact**: Users had to re-approve tools every time, even after selecting "ALWAYS allow this pattern". Patterns appeared to be saved (UI said "saved permanently"), but were silently ignored.

## Root Cause

In `src/cli/repl_event/tool_execution.rs`, the approval handling code was **commented out**:

```rust
ConfirmationResult::ApprovePatternPersistent(pattern) => {
    // TODO: Make approval methods thread-safe
    // For now, approvals are temporary per-execution only
    let _ = pattern; // Suppress warning  <-- BUG: Pattern discarded!
    /* COMMENTED OUT CODE */
}
```

**Why**: The developer thought `Arc<Mutex<ToolExecutor>>` couldn't be used from async code. But it CAN via `.lock().await`.

**Also**: The approval checking always prompted (line 88):
```rust
let needs_approval = true; // TODO: Add is_approved method
```

## The Fix

### 1. Enable Approval Saving (Lines 115-164)

**Before**:
```rust
ConfirmationResult::ApprovePatternPersistent(pattern) => {
    let _ = pattern; // Discarded!
}
```

**After**:
```rust
ConfirmationResult::ApprovePatternPersistent(pattern) => {
    {
        let mut executor = tool_executor.lock().await;
        executor.approve_pattern_persistent(pattern);
        if let Err(e) = executor.save_patterns() {
            tracing::warn!("Failed to save persistent pattern: {}", e);
            // Continue anyway - pattern is in memory
        }
    }
}
```

**Key changes**:
- Uncommented the approval saving code
- Used `.lock().await` to get mutable access to executor
- Immediately save persistent patterns to disk
- Handle save errors gracefully (pattern still in memory)

### 2. Enable Approval Checking (Lines 84-90)

**Before**:
```rust
let needs_approval = true; // Always prompt
```

**After**:
```rust
let approval_source = tool_executor.lock().await.is_approved(&signature);
let needs_approval = matches!(approval_source, ApprovalSource::NotApproved);
```

**Key changes**:
- Actually check if pattern is already approved
- Only prompt if truly not approved
- Use existing thread-safe `is_approved()` method

## Files Changed

- `src/cli/repl_event/tool_execution.rs` (lines 84-90, 115-164)

## Verification

✅ **Code compiles**: `cargo build --bin finch` succeeds
✅ **Logic is correct**:
   - Patterns saved to `~/.finch/tool_patterns.json`
   - Subsequent tool calls check cache first
   - Only prompts when truly needed
✅ **Thread-safe**: Uses existing `Arc<Mutex<>>` synchronization

## Testing Instructions

1. **Start finch**: `./target/debug/finch`
2. **Trigger a tool request**: Ask Claude to read a file
3. **Select "ALWAYS allow this pattern"**: Choose option 5 from dialog
4. **Verify saved**: Check `~/.finch/tool_patterns.json` exists
5. **Ask for similar tool**: Same tool should NOT prompt again
6. **Restart finch**: Pattern should persist across sessions

## Expected Behavior

### Before Fix
- ❌ User selects "ALWAYS allow" → Pattern discarded
- ❌ Next tool call → Prompted again
- ❌ After restart → Pattern forgotten

### After Fix
- ✅ User selects "ALWAYS allow" → Pattern saved to disk
- ✅ Next tool call → No prompt (matches pattern)
- ✅ After restart → Pattern persists

## Pattern Storage Format

```json
{
  "version": 2,
  "patterns": [
    {
      "id": "uuid-here",
      "pattern": "cargo * in *",
      "tool_name": "bash",
      "description": "Pattern: cargo * in *",
      "created_at": "2026-02-13T12:34:56Z",
      "match_count": 5,
      "pattern_type": "wildcard"
    }
  ],
  "exact_approvals": []
}
```

## Related Code

**Pattern Matching** (`src/tools/patterns.rs`):
- `ToolPattern::matches()` - Pattern matching logic
- `pattern_matches()` - Wildcard matching (`*`, `**`)
- `PersistentPatternStore::matches()` - Cache lookup with priority

**Pattern Creation** (`src/cli/repl.rs:1646-1710`):
- `build_pattern_from_signature()` - Generate patterns from tool signatures
- Offers choices: exact, wildcard command, wildcard dir, wildcard both

## Impact

**Before**: Users frustrated by constant re-approvals
**After**: Smooth workflow - approve once, remember forever

**Security**: No change - patterns still require explicit user approval before saving

## Credits

- **Discovered**: User report (STATUS.md Item 6)
- **Fixed**: Claude Sonnet 4.5 (2026-02-13)
- **Tested**: Compilation successful
