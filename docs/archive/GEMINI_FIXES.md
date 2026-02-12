# Gemini Provider Bug Fixes

## Summary

Fixed critical bugs in the Gemini provider implementation that would have broken multi-turn tool calling and caused tool call ID collisions.

## Bugs Fixed

### 🔴 CRITICAL Bug 1: Message Conversion Lost Tool Blocks

**Issue**: Lines 75-77 in `src/providers/gemini.rs` only extracted text from messages, discarding `ToolUse` and `ToolResult` blocks. This broke multi-turn tool calling.

**Impact**:
- Tool results from previous turns were lost
- Multi-turn tool execution would fail
- Gemini couldn't see the results of tools it had called

**Root Cause**:
```rust
// BEFORE (BROKEN):
parts: vec![GeminiPart::Text {
    text: msg.text(),  // Only extracts text, discards tool blocks!
}],
```

**Fix**: Properly convert all ContentBlock types to Gemini parts:
```rust
// AFTER (FIXED):
let parts: Vec<GeminiPart> = msg
    .content
    .iter()
    .map(|block| match block {
        ContentBlock::Text { text } => GeminiPart::Text {
            text: text.clone(),
        },
        ContentBlock::ToolUse { id: _, name, input } => GeminiPart::FunctionCall {
            function_call: GeminiFunctionCall {
                name: name.clone(),
                args: input.clone(),
            },
        },
        ContentBlock::ToolResult { tool_use_id, content, is_error } => {
            GeminiPart::FunctionResponse {
                function_response: GeminiFunctionResponse {
                    name: tool_use_id.clone(),
                    response: serde_json::json!({
                        "content": content,
                        "is_error": is_error.unwrap_or(false),
                    }),
                },
            }
        },
    })
    .collect();
```

**Files Changed**: `src/providers/gemini.rs` (lines 62-122)

---

### 🔴 CRITICAL Bug 2: Tool Call ID Collisions

**Issue**: Line 141 generated tool call IDs using only the function name: `format!("gemini_{}", function_call.name)`. Multiple calls to the same tool would get identical IDs.

**Impact**:
- Tool executor relies on unique IDs to track which tool call each result belongs to
- Multiple simultaneous calls to the same tool would collide
- Tool results could be mismatched with wrong tool calls

**Root Cause**:
```rust
// BEFORE (BROKEN):
id: format!("gemini_{}", function_call.name),  // Same ID for same tool!
```

**Fix**: Use UUID to ensure unique IDs:
```rust
// AFTER (FIXED):
let unique_id = format!("gemini_{}_{}", function_call.name, Uuid::new_v4());
content.push(ContentBlock::ToolUse {
    id: unique_id,
    name: function_call.name,
    input: function_call.args,
});
```

**Files Changed**:
- `src/providers/gemini.rs` (added `use uuid::Uuid;` at line 14)
- `src/providers/gemini.rs` (lines 171-178)

---

### 🟡 MEDIUM Bug 3: Empty Candidates Create Fake Response

**Issue**: Lines 118-127 created a fake empty candidate when Gemini returned no candidates, hiding the error condition.

**Impact**:
- Errors were masked as empty responses
- Hard to debug why Gemini returned nothing
- Could lead to confusion (empty response vs. error)

**Root Cause**:
```rust
// BEFORE (BROKEN):
let candidate = response.candidates.into_iter().next().unwrap_or_else(|| {
    GeminiCandidate {  // Create fake candidate!
        content: GeminiContent {
            role: "model".to_string(),
            parts: vec![],
        },
        finish_reason: Some("ERROR".to_string()),
        safety_ratings: None,
    }
});
```

**Fix**: Return proper error when no candidates present:
```rust
// AFTER (FIXED):
fn from_gemini_response(
    &self,
    response: GeminiResponse,
    model: String,
) -> Result<ProviderResponse> {  // Now returns Result
    let candidate = response
        .candidates
        .into_iter()
        .next()
        .context("Gemini returned no candidates in response")?;  // Proper error!
    // ...
}
```

**Files Changed**: `src/providers/gemini.rs` (lines 148-163)

---

### 🟢 LOW Enhancement 4: Log Tool Schema Conversion Errors

**Issue**: Lines 88-90 silently ignored errors when converting tool schemas to JSON, using `unwrap_or(json!({}))`.

**Impact**:
- Tool definitions could be silently broken
- No visibility into what went wrong
- Gemini would receive empty schemas

**Fix**: Log warnings when schema conversion fails:
```rust
// AFTER (IMPROVED):
let parameters = match serde_json::to_value(&tool.input_schema) {
    Ok(value) => value,
    Err(e) => {
        tracing::warn!(
            "Failed to convert tool schema for '{}': {}",
            tool.name,
            e
        );
        serde_json::json!({})
    }
};
```

**Files Changed**: `src/providers/gemini.rs` (lines 119-134)

---

## Code Quality Improvements

### Clippy Warnings Fixed

1. **Removed unused `mut`**: `generation_config` didn't need to be mutable (line 140)
2. **Ignored unused field**: `id` field in pattern match was unused (line 82)

---

## Testing Status

### ✅ Compilation
- Code compiles successfully with no errors
- All type changes propagate correctly
- UUID dependency already present in Cargo.toml

### ⚠️ Runtime Testing Required

**The fixes have NOT been tested with real Gemini API calls.** To validate:

1. **Simple Query Test** (non-streaming)
   ```bash
   # Configure Gemini
   cat > ~/.shammah/config.toml <<EOF
   [fallback]
   provider = "gemini"

   [fallback.gemini]
   api_key = "YOUR_KEY"
   model = "gemini-2.0-flash-exp"
   EOF

   # Test basic query
   shammah query "What is 2+2?"
   ```
   **Expected**: Gemini responds with answer

2. **Streaming Test**
   ```bash
   shammah query "Write a haiku about coding"
   ```
   **Expected**: Text appears incrementally

3. **Tool Calling Test** (Critical - tests Fix #1 and #2)
   ```bash
   shammah
   > Read the file at src/main.rs and tell me what it does
   ```
   **Expected**:
   - Gemini calls Read tool with unique ID
   - Tool result is sent back to Gemini
   - Gemini responds based on file contents

4. **Multi-turn Tool Test** (Critical - tests Fix #1)
   ```bash
   shammah
   > Read src/main.rs
   > Now read src/lib.rs and compare them
   ```
   **Expected**:
   - First turn: Read tool called, result returned
   - Second turn: Gemini sees previous tool results and calls Read again
   - Gemini compares both files

5. **Multiple Simultaneous Tools** (Critical - tests Fix #2)
   ```bash
   shammah
   > Read both src/main.rs and src/lib.rs, then compare them
   ```
   **Expected**:
   - Gemini calls Read tool twice in same turn (different tool call IDs)
   - Both results returned without collision
   - Gemini receives both results correctly

---

## Files Modified

1. **src/providers/gemini.rs** (primary changes)
   - Added `use uuid::Uuid;`
   - Fixed `to_gemini_request()` to preserve all content blocks
   - Fixed `from_gemini_response()` to use UUID for unique IDs
   - Fixed `from_gemini_response()` to return Result for empty candidates
   - Added logging for tool schema conversion errors
   - Fixed clippy warnings

---

## Verification Checklist

- [x] Code compiles without errors
- [x] Clippy warnings addressed
- [x] Type changes propagate correctly
- [ ] Simple query works (requires API key)
- [ ] Streaming works (requires API key)
- [ ] Tool calling works (requires API key)
- [ ] Multi-turn tool calling works (requires API key)
- [ ] Multiple simultaneous tools work (requires API key)

---

## Next Steps

1. **Obtain Gemini API key** for testing
2. **Run test suite** (checklist above)
3. **Verify tool calling** specifically (most critical)
4. **Update status** in GEMINI_FIXES.md after testing
5. **Document any new issues** discovered during testing

---

## Conclusion

The Gemini provider now has correct implementations for:
- ✅ Message conversion (preserves tool blocks)
- ✅ Tool call ID generation (unique IDs)
- ✅ Error handling (no fake responses)
- ✅ Logging (tool schema errors visible)

**Status**: Ready for API testing with real Gemini calls.
