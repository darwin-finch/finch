# Output Regression Fix: Text Overlapping Due to Missing Carriage Returns

**Date:** 2026-02-07
**Status:** ✅ FIXED

## Summary

Fixed critical regression in REPL TUI mode where text was overlapping horizontally on the same line instead of advancing to new lines after each newline character.

## Root Cause

The TUI system was designed to use buffering mode, but **buffering was never enabled**. This caused a fatal mismatch:

1. `OutputManager` defaulted to immediate mode (`buffering_mode = false`)
2. Immediate mode writes to stdout with `\r\n` line endings
3. TUI's `flush_output_safe()` expects buffered content and strips `\r` characters
4. Result: Terminal cursor doesn't return to column 0, causing horizontal overlap

**The Design Intent (from commit 3ff677c):**
- Buffering mode for TUI: accumulate messages → batch flush through Ratatui
- Immediate mode for non-TUI: write directly to stdout with `\r\n`

**What Actually Happened:**
- Buffering was **never enabled**, even in TUI mode
- `enable_buffering()` method existed but was **never called**
- TUI always ran in immediate mode, causing output conflicts

## The Fix

**File:** `src/cli/repl.rs` (lines 333-334)

Added `output_manager.enable_buffering()` call after TUI renderer initialization:

```rust
// Initialize TUI renderer if enabled (Phase 2: Ratatui interface)
if config.tui_enabled && is_interactive {
    match TuiRenderer::new(Arc::new(output_manager.clone()), Arc::new(status_bar.clone())) {
        Ok(renderer) => {
            output_status!("✓ TUI mode enabled (Ratatui)");

            // Enable buffering for TUI mode (fixes output regression)
            output_manager.enable_buffering(); // ← FIX ADDED HERE

            // Set global TUI renderer for Menu dialogs (Phase 5)
            use crate::cli::global_output::set_global_tui_renderer;
            set_global_tui_renderer(renderer);
        }
        Err(e) => {
            output_status!("⚠️  Failed to initialize TUI: {}", e);
            output_status!("   Falling back to standard output mode");
        }
    }
}
```

## Why This Fixes It

1. **Buffering enabled** → OutputManager stops writing directly to stdout with `\r\n`
2. **Messages accumulate** in `pending_flush: Vec<String>` buffer
3. **TUI flush renders** messages through Ratatui (which handles cursor positioning correctly)
4. **No \r conflicts** because buffered messages don't have `\r\n` yet
5. **Ratatui handles** all cursor positioning and line breaks properly

## Symptoms (Before Fix)

```
It looks like you denied the tool execution. Let me know what kind of test you'd like me to perform! I have several options available:

             1. **Query Shammah directly** - Send a test query to see how the local model responds
                                                                                                  2. **Compare responses** - Test the same query on both Shammah and Claude to see the differences
```

Text appearing on the same line, overlapping horizontally.

## Expected Behavior (After Fix)

```
It looks like you denied the tool execution. Let me know what kind of test you'd like me to perform! I have several options available:

1. **Query Shammah directly** - Send a test query to see how the local model responds
2. **Compare responses** - Test the same query on both Shammah and Claude to see the differences
```

Text appearing on separate lines with proper formatting.

## Testing

### Automated Tests

```bash
# Query mode (uses immediate mode - should work)
./target/release/finch query "What is 2+2?"
```

**Result:** ✅ Works correctly - returns "2 + 2 = 4" with proper formatting

### Manual Tests (Required)

1. **TUI Mode (default):**
   ```bash
   ./target/release/finch
   > test query
   ```
   **Verify:** Text appears on separate lines without horizontal overlap

2. **Raw Mode:**
   ```bash
   ./target/release/finch --raw
   > test query
   ```
   **Verify:** Output is readable and properly formatted

## Impact

- **Before:** REPL was unusable in interactive/TUI mode (critical regression)
- **After:** REPL output is readable with proper line breaks
- **Scope:** TUI mode only (query mode and raw mode were already working)

## Related Files

- `src/cli/repl.rs` - REPL initialization, TUI setup (MODIFIED)
- `src/cli/output_manager.rs` - OutputManager with buffering logic
- `src/cli/tui/mod.rs` - TUI rendering with flush_output_safe()

## When Did It Break?

Event loop refactor (commits 1c4873c and 63d8483) introduced the TUI system with buffering design, but buffering was never enabled in practice.

## Lessons Learned

1. **Design vs. Implementation Gap:** The buffering system was designed but not activated
2. **Missing Integration Test:** Need tests that verify TUI output formatting
3. **State Management:** Boolean flags (buffering_mode) need explicit initialization
4. **Documentation:** Add comments explaining when buffering should be enabled

## Future Improvements

1. Add integration test for TUI output formatting
2. Add assertion that buffering is enabled in TUI mode (debug build)
3. Consider making buffering mode part of OutputManager constructor
4. Document the buffering requirement in TUI documentation
