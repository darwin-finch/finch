# Background Fix - Manual Test Guide

## Test Setup

**Binary:** `./target/release/finch`
**Build:** Fresh build with background fix (completed)
**Daemon:** Running on PID 80886

---

## Test Procedure

### Step 1: Start REPL

```bash
cd /Users/finch/repos/claude-proxy
./target/release/finch
```

### Step 2: Send User Messages

Type the following messages one at a time:

```
> hello
> how are you?
> testing background colors
> this is message 4
> and message 5
```

### Step 3: Visual Verification

**Check each message line for:**

1. **Grey background present?** ✓ / ✗
   - Background should be visible on user message lines
   - Color: Light grey (RGB 220, 220, 220)

2. **Full width?** ✓ / ✗
   - Background extends to right edge of terminal
   - Not just around the text

3. **Persistent?** ✓ / ✗
   - Background stays after typing next message
   - Doesn't flicker or disappear
   - Remains when scrolling

4. **Consistent?** ✓ / ✗
   - All user messages have same grey background
   - Assistant responses have NO background (white/default)

---

## Expected Result

**User messages should look like:**

```
┌─────────────────────────────────────────────────┐
│ [GREY BACKGROUND████████] hello                │
│ Response text appears here (no background)      │
│ [GREY BACKGROUND████████] how are you?         │
│ Another response (no background)                │
│ [GREY BACKGROUND████████] testing backgrounds  │
└─────────────────────────────────────────────────┘
```

**Where:**
- `[GREY BACKGROUND████████]` = Light grey background extending full width
- User message text has black text on grey background
- Assistant responses have no background (default terminal color)

---

## What Was Fixed

### Before Fix
```
┌─────────────────────────────────────────────────┐
│ [GREY] hello                                    │  ← Background appears
│ Response...                                     │
│ how are you?                                    │  ← Background LOST!
│ Response...                                     │
│ testing                                         │  ← Still no background
└─────────────────────────────────────────────────┘
```

Background would appear initially but **disappear** after the next message was typed.

### After Fix
```
┌─────────────────────────────────────────────────┐
│ [GREY BACKGROUND] hello                         │  ← Background persists
│ Response...                                     │
│ [GREY BACKGROUND] how are you?                  │  ← Background persists
│ Response...                                     │
│ [GREY BACKGROUND] testing                       │  ← Background persists
└─────────────────────────────────────────────────┘
```

Background **persists** through all messages and updates.

---

## Troubleshooting

### If backgrounds don't appear at all:

1. **Check terminal emulator:**
   - Some terminals don't support ANSI background colors
   - Try: iTerm2, Terminal.app, or modern terminals

2. **Check terminal settings:**
   - Make sure ANSI colors are enabled
   - Check color theme isn't overriding backgrounds

3. **Check code:**
   - Verify `background_style()` returns `Some(Style)` for UserMessage
   - Check shadow buffer is filling entire row with style

### If backgrounds appear but disappear:

1. **Check blit logic:**
   - Line 1371: Should NOT clear before writing
   - Should overwrite content, then clear trailing

2. **Check shadow buffer:**
   - Should fill ALL cells (0 to width) with style
   - Not just cells with text

### If backgrounds are wrong color:

1. **Check UserMessage implementation:**
   - Should return `Color::Rgb(220, 220, 220)` (light grey)
   - Check in `src/cli/messages/concrete.rs:87-92`

---

## Debug Commands

### Check if background codes are in output:

```bash
# Run REPL and check raw output
script -q /dev/null ./target/release/finch | cat -v | less
# Look for ANSI escape codes like: ^[[48;2;220;220;220m
```

### Check shadow buffer width:

```bash
# Check terminal width
tput cols

# Shadow buffer should match terminal width
# Verify in logs or add debug print
```

---

## Report Results

After testing, report:

1. **Did grey backgrounds appear?** Yes / No
2. **Did they persist through messages?** Yes / No
3. **Did they extend full width?** Yes / No
4. **Any flickering or disappearing?** Yes / No
5. **Terminal emulator used:** (e.g., iTerm2, Terminal.app)

---

## Quick Test Script

```bash
#!/bin/bash
echo "Starting REPL test..."
./target/release/finch << 'EOF'
hello
test message 2
another test
exit
EOF

echo ""
echo "Did you see grey backgrounds on user messages? (y/n)"
read response
if [ "$response" = "y" ]; then
    echo "✅ Background fix verified!"
else
    echo "❌ Background issue persists"
fi
```

---

## Technical Details

**Background color:** RGB(220, 220, 220) = Light grey
**ANSI code:** `\x1b[48;2;220;220;220m` (24-bit color)
**Applied to:** User messages only (not assistant responses)
**Extent:** Full terminal width (all cells in row)

**Code locations:**
- Background style: `src/cli/messages/concrete.rs:87-92`
- Shadow buffer fill: `src/cli/tui/shadow_buffer.rs:114-117`
- Blit logic: `src/cli/tui/mod.rs:1369-1407`

---

## Success Criteria

**PASS if:**
- ✅ Grey backgrounds appear on ALL user messages
- ✅ Backgrounds extend to full terminal width
- ✅ Backgrounds persist through multiple messages
- ✅ No flickering or disappearing
- ✅ Assistant messages have no background

**FAIL if:**
- ❌ No backgrounds appear
- ❌ Backgrounds disappear after first message
- ❌ Backgrounds only around text (not full width)
- ❌ Flickering or inconsistent rendering

---

## Next Steps

**If test passes:** Background fix is complete! ✅

**If test fails:**
1. Report exact symptoms (which criteria failed)
2. Share terminal emulator and settings
3. We'll investigate further

Ready to test? Run the REPL and type some messages!
