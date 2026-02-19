# Fix: Enable Custom Text Input in Dialogs

**Date**: 2026-02-13
**Priority**: 🟡 MEDIUM (UX - Missing Feature)
**Status**: ✅ FIXED

## Problem

**User Report**: "I also can't answer free-form text as an option in the dialogs yet."

**Impact**: Users couldn't provide custom text responses in dialogs, even though the dialog system had the infrastructure for it (pressing 'o' for "Other").

## Root Cause

The tool approval dialogs were using `Dialog::select()` instead of `Dialog::select_with_custom()`, which disables the "Other" option that allows free-form text input.

**Code Location**: `src/cli/repl_event/event_loop.rs` line 1211

## The Fix

### 1. Enable Custom Text in Tool Approval Dialogs (Line 1211)

**Before**:
```rust
let dialog = Dialog::select(
    format!("Tool '{}' requires approval\n{}", tool_name, summary),
    options,
);
```

**After**:
```rust
let dialog = Dialog::select_with_custom(
    format!("Tool '{}' requires approval\n{}", tool_name, summary),
    options,
);
```

### 2. Handle CustomText Dialog Results (Lines 1276-1280)

**Added**:
```rust
crate::cli::tui::DialogResult::CustomText(text) => {
    // User provided custom response - log it and deny for safety
    tracing::info!("Tool approval custom response: {}", text);
    ConfirmationResult::Deny
}
```

**Note**: For tool approvals, custom text defaults to "Deny" for safety, since the user must explicitly choose an approval level. Custom text is logged for debugging/feedback purposes.

## How It Works

### For Users:

1. **In any dialog**, you'll see: `Press 'o' for Other (custom response)`
2. **Press 'o'**: Enters custom text input mode
3. **Type your message**: Character-by-character input with backspace support
4. **Press Enter**: Submit your custom text
5. **Press Esc**: Cancel and return to selection mode

### Dialog System Features:

**Already implemented** in `src/cli/tui/dialog.rs`:
- ✅ `allow_custom` flag enables "Other" option
- ✅ `custom_mode_active` tracks input state
- ✅ `custom_input` stores the typed text
- ✅ 'o' key triggers custom mode (line 186)
- ✅ Character input with backspace (lines 227-240)
- ✅ Enter submits, Esc cancels (lines 241-260)
- ✅ Returns `DialogResult::CustomText(String)` (line 245)

**Already rendered** in `src/cli/tui/dialog_widget.rs`:
- ✅ Shows "Press 'o' for Other" prompt (lines 92-95)
- ✅ Renders input field when active (lines 98-110)
- ✅ Highlights active input with colors (lines 84-91)

## Files Changed

- `src/cli/repl_event/event_loop.rs` (lines 1211, 1276-1280)

## Verification

✅ **Code compiles**: `cargo build --bin finch` succeeds
✅ **Infrastructure exists**: Dialog system fully supports custom text input
✅ **Safety preserved**: Tool approvals with custom text default to Deny

## Testing Instructions

1. **Start finch**: `./target/debug/finch`
2. **Trigger a tool approval dialog**: Ask Claude to read a file
3. **Verify "Other" option visible**: Look for "Press 'o' for Other (custom response)"
4. **Press 'o'**: Should enter custom text input mode
5. **Type some text**: Should appear at bottom of dialog
6. **Press Enter**: Should submit (will deny tool for safety)
7. **Press Esc during input**: Should cancel and return to selection

## Expected Behavior

### Before Fix
- ❌ No "Press 'o' for Other" message shown
- ❌ Pressing 'o' does nothing
- ❌ Cannot enter custom text

### After Fix
- ✅ "Press 'o' for Other (custom response)" shown at bottom
- ✅ Pressing 'o' enters custom text mode
- ✅ Can type characters with backspace support
- ✅ Enter submits, Esc cancels
- ✅ Custom text logged for feedback

## Use Cases

### Tool Approvals (Current Dialog):
- Custom text is logged but defaults to Deny for safety
- User should use numbered options (1-6) for specific approval levels
- Custom text can provide feedback about why they're denying

### Plan Approvals (Existing Dialog):
- Custom text already works for "Request Changes" option
- User can provide detailed feedback about what needs changing
- See lines 1517-1544 for existing CustomText handling

### Future Dialogs:
- Any dialog created with `select_with_custom()` or `multiselect_with_custom()`
- Enables user feedback, custom reasons, or flexible responses

## Dialog Types Supporting Custom Text

1. **Single-Select**: `Dialog::select_with_custom()`
   - Press 'o' to enter custom text
   - Returns `DialogResult::CustomText(String)`

2. **Multi-Select**: `Dialog::multiselect_with_custom()`
   - Press 'o' to enter custom text alongside selections
   - Returns `DialogResult::CustomText(String)`

3. **Text Input**: `Dialog::text_input()`
   - Direct text input (no need for 'o' key)
   - Returns `DialogResult::TextEntered(String)`

4. **Confirmation**: `Dialog::confirm()`
   - Yes/No only (no custom text option)
   - Returns `DialogResult::Confirmed(bool)`

## Architecture

```
Dialog::select_with_custom()
    ↓
allow_custom: true
    ↓
DialogWidget renders: "Press 'o' for Other"
    ↓
User presses 'o'
    ↓
custom_mode_active = true
    ↓
DialogWidget shows input field
    ↓
User types: "This is my custom response"
    ↓
User presses Enter
    ↓
DialogResult::CustomText("This is my custom response")
    ↓
Handler processes custom text appropriately
```

## Impact

**Before**: Users couldn't provide free-form text in dialogs
**After**: Users can press 'o' to enter custom text in any dialog that supports it

**UX**: More flexible, allows feedback and custom reasons

## Credits

- **Reported**: User feedback (2026-02-13)
- **Fixed**: Claude Sonnet 4.5 (2026-02-13)
- **Tested**: Compilation successful
