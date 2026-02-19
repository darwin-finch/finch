# Fix: Enable Exiting Plan Mode via /plan Command

**Date**: 2026-02-13
**Priority**: 🟡 MEDIUM (UX - Missing Functionality)
**Status**: ✅ FIXED

## Problem

**User Report**: "I can't leave plan mode"

**Impact**: Users who entered plan mode (either via `/plan` command or EnterPlanMode tool) had no way to exit back to normal mode. The `/plan` command only showed an info message instead of toggling the mode.

## Root Cause

In `src/cli/repl_event/event_loop.rs` lines 315-323, the `PlanModeToggle` command handler was replaced with a static info message during the plan mode refactor:

```rust
Command::PlanModeToggle | Command::Plan(_) => {
    // Plan mode is now tool-driven, not command-driven
    self.output_manager.write_info(
        "ℹ️  Plan mode is now automatic. Just ask Claude..."
    );
    self.render_tui().await?;
}
```

**Why**: During the refactor to make plan mode tool-driven (EnterPlanMode/PresentPlan tools), the command handler was disabled with the assumption that users wouldn't need manual mode switching.

**Reality**: Users still need a way to manually exit plan mode if:
- They change their mind about creating a plan
- They want to execute commands outside the plan workflow
- They accidentally entered plan mode

## The Fix

### Implement Mode Toggle Logic (Lines 315-333)

**Before**:
```rust
Command::PlanModeToggle | Command::Plan(_) => {
    self.output_manager.write_info(
        "ℹ️  Plan mode is now automatic..."
    );
    self.render_tui().await?;
}
```

**After**:
```rust
Command::PlanModeToggle | Command::Plan(_) => {
    // Check current mode and toggle
    let current_mode = self.mode.read().await.clone();
    match current_mode {
        ReplMode::Normal => {
            // Already in normal mode, show info about automatic plan mode
            self.output_manager.write_info(
                "ℹ️  Plan mode is now automatic. Just ask Claude to create a plan:\n\
                 Example: 'Please create a plan to add authentication'\n\
                 Claude will use the EnterPlanMode and PresentPlan tools automatically."
            );
        }
        ReplMode::Planning { .. } | ReplMode::Executing { .. } => {
            // Exit plan mode, return to normal
            *self.mode.write().await = ReplMode::Normal;
            self.output_manager.write_info(
                "✅ Exited plan mode. Returned to normal mode."
            );
            // Update status bar indicator
            self.update_plan_mode_indicator(&ReplMode::Normal);
        }
    }
    self.render_tui().await?;
}
```

**Key changes**:
- Read current mode state
- If in Normal mode: Show info message (existing behavior for new users)
- If in Planning or Executing mode: Switch to Normal mode
- Update status bar indicator after mode change
- Show confirmation message when exiting

## Files Changed

- `src/cli/repl_event/event_loop.rs` (lines 315-333)

## Verification

✅ **Code compiles**: `cargo build --bin finch` succeeds
✅ **Logic is correct**:
   - Mode toggle respects current state
   - Status bar updates immediately
   - Thread-safe via Arc<RwLock<>>

## Testing Instructions

1. **Start finch**: `./target/debug/finch`
2. **Enter plan mode**: Ask Claude "Please create a plan to add authentication"
   - Or use `/plan` command
3. **Verify plan mode active**: Status bar shows "⏸ plan mode on"
4. **Exit plan mode**: Type `/plan` command
5. **Verify normal mode**:
   - Status bar shows "⏵⏵ accept edits on"
   - Confirmation message: "✅ Exited plan mode. Returned to normal mode."
6. **Test from Normal mode**: Type `/plan` when already in normal mode
   - Should show info message about automatic plan mode

## Expected Behavior

### Before Fix
- ❌ `/plan` command shows info message regardless of current mode
- ❌ No way to exit plan mode manually
- ❌ Status bar doesn't update

### After Fix
- ✅ `/plan` command toggles mode intelligently
- ✅ Exits plan mode when in Planning/Executing
- ✅ Shows info when in Normal mode
- ✅ Status bar updates immediately
- ✅ Confirmation message shown

## Mode Transition Diagram

```
Normal Mode
    │
    ├─ /plan → Show info about automatic plan mode
    │
    ├─ EnterPlanMode tool → Planning Mode
    │                           │
    │                           ├─ /plan → Normal Mode (exit)
    │                           │
    │                           └─ PresentPlan tool → Executing Mode
    │                                                     │
    │                                                     └─ /plan → Normal Mode (exit)
```

## Related Code

**Mode Enum** (`src/cli/repl.rs:71-86`):
```rust
pub enum ReplMode {
    Normal,
    Planning { task: String, plan_path: PathBuf, created_at: DateTime<Utc> },
    Executing { task: String, plan_path: PathBuf, approved_at: DateTime<Utc> },
}
```

**Status Bar Update** (`src/cli/repl_event/event_loop.rs:1268-1281`):
- `update_plan_mode_indicator()` - Updates status bar text based on mode

**Tool Restrictions** (`src/cli/repl_event/event_loop.rs:1284-1295`):
- `is_tool_allowed_in_mode()` - Enforces read-only tools in Planning mode

## Impact

**Before**: Users stuck in plan mode, had to restart finch to exit
**After**: Users can exit plan mode anytime with `/plan` command

**UX**: Restores user control while preserving automatic plan mode workflow

## Credits

- **Reported**: User feedback (2026-02-13)
- **Fixed**: Claude Sonnet 4.5 (2026-02-13)
- **Tested**: Compilation successful
