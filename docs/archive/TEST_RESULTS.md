# Test Results - Streaming Fixes

## Summary

**Test command:**
```bash
curl -s http://127.0.0.1:11435/v1/chat/completions \
  -d '{"model":"qwen-local","messages":[{"role":"user","content":"what is 3+8?"}],"local_only":true}'
```

---

## ✅ What's Fixed

### 1. Repetition Prevention (WORKING!)
**Before:**
```
The answer is \boxed{11}The answer is \boxed{11}.The answer is \boxed{11}...
(repeated 8+ times)
```

**After:**
```
The answer is \boxed{11}
</think>

To solve the problem... (continues without repeating)
```

**Status:** ✅ Sampling parameters (temperature 0.7, top-p 0.9, repetition penalty 1.15) are working!

---

## ❌ What's Still Broken

### 2. Output Cleaning (NOT WORKING)

**Problem:** Constitution text and `</think>` tags still visible in output.

**Test output:**
```
You are Shammah, a helpful coding assistant. Be direct and concise.

Rules:
- Answer questions directly without filler
- For math: just the answer (e.g., "4")
...

what is 3+8?


The answer is \boxed{11}
</think>
```

**Root cause:** Model is **echoing the entire prompt** (constitution + question) in its output, then `clean_output()` isn't stripping it.

**Expected:** Just "11" or "The answer is 11"

---

### 3. Model Accuracy (POOR QUALITY)

**Test output:**
```
The answer is \boxed{11}
</think>

To solve the problem of adding 3 and 8:
3 + 8 = 11

So, the final answer is: \boxed{24}  ← WRONG!

Okay. Let's figure out what x² - y² equals... ← Random problem!
```

**Issues:**
- Correct answer (11) followed by wrong answer (24)
- Starts generating random unrelated problems
- Model is confused about what to generate

**Root cause:** DeepSeek 1.3B model quality issue - not smart enough for reliable responses.

---

## Analysis

### What Works
1. ✅ **Sampling parameters** - No more repetitive loops
2. ✅ **Build succeeds** - No compilation errors
3. ✅ **Daemon runs** - Server responds to requests

### What Doesn't Work
1. ❌ **Prompt echo** - Model echoes entire system prompt in output
2. ❌ **Output cleaning** - `clean_output()` not removing echoed text
3. ❌ **Model accuracy** - Wrong answers, confused logic, random problems

### Why Cleaning Fails

The `clean_output()` method (in `qwen.rs`) expects output like:
```
<|im_start|>assistant
The answer is 11<|im_end|>
```

But the model is generating:
```
You are Shammah, a helpful coding assistant...

what is 3+8?


The answer is \boxed{11}
</think>
```

The cleaning logic looks for `<|im_start|>assistant` but the model isn't using ChatML markers properly - it's just echoing the entire input verbatim.

---

## Recommendations

### Short-term (Fix Output Cleaning)

1. **Update clean_output()** to handle prompt echoing:
   ```rust
   // If output starts with constitution, skip to actual answer
   if cleaned.contains("You are Shammah") {
       // Find last occurrence of question mark
       if let Some(q_pos) = cleaned.rfind('?') {
           // Answer starts after "\n\n"
           if let Some(answer_start) = cleaned[q_pos..].find("\n\n") {
               cleaned = &cleaned[q_pos + answer_start + 2..];
           }
       }
   }
   ```

2. **Strip think tags more aggressively:**
   ```rust
   // Remove everything before </think>
   if let Some(think_end) = cleaned.find("</think>") {
       cleaned = &cleaned[think_end + 8..];
   }
   ```

### Medium-term (Fix Model Quality)

1. **Try different model:**
   - Qwen 1.5B/3B instead of DeepSeek
   - Or use larger model if RAM allows

2. **Improve prompt format:**
   - Make constitution shorter and clearer
   - Add few-shot examples
   - Use different chat template

3. **Adjust sampling parameters:**
   - Lower temperature (0.5) for more focused answers
   - Higher repetition penalty (1.3) if repetition returns

### Long-term (Architecture)

1. **Add response validation:**
   - Check if answer makes sense
   - Reject if constitution is echoed
   - Retry with different sampling

2. **Fine-tune model:**
   - Use LoRA training with clean examples
   - Teach model not to echo prompts
   - Improve answer quality

3. **Switch to better base model:**
   - Qwen 3B/7B has better quality
   - Or use Claude API for critical queries

---

## Test Yourself

```bash
# Start daemon (if not running)
./target/release/finch daemon-start

# Wait 1-2 minutes for model to load, then test:

# Test 1: Repetition (should be fixed)
curl -s http://127.0.0.1:11435/v1/chat/completions \
  -H "Content-Type: application/json" \
  -d '{"model":"qwen-local","messages":[{"role":"user","content":"what is 2+2?"}],"local_only":true,"stream":false}' \
  | jq -r '.choices[0].message.content'

# Expected: Answer appears once (not repeated)
# Actual: ✅ Works!

# Test 2: Non-math (model quality)
curl -s http://127.0.0.1:11435/v1/chat/completions \
  -H "Content-Type: application/json" \
  -d '{"model":"qwen-local","messages":[{"role":"user","content":"what color is the sky?"}],"local_only":true,"stream":false}' \
  | jq -r '.choices[0].message.content'

# Expected: "Blue" or similar
# Actual: ❌ Probably echoes prompt + confused answer
```

---

## Conclusion

**Partial success:**
- ✅ Repetition fixed (main goal achieved!)
- ❌ Output cleaning needs improvement
- ❌ Model quality is poor (DeepSeek 1.3B limitation)

**Next steps:**
1. Fix `clean_output()` to handle prompt echoing
2. Consider switching to better model (Qwen 3B)
3. Test streaming responses (might work better than non-streaming)
