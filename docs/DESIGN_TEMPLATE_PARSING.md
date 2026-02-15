# Chat Template Parsing - Proper Architecture

## Current Problem

**Current approach** (as of 2026-02-15):
```rust
// HACK: Filter by text pattern during streaming
let is_special = token_text.contains("<|")
    || token_text.contains("<｜")
    || token_text.contains("</think>")
    || ...
```

**Issues:**
1. ❌ Pattern matching on decoded text, not token IDs
2. ❌ Can't handle multi-token special sequences
3. ❌ Mixes different model formats (Qwen ChatML + DeepSeek)
4. ❌ Duplicates logic between streaming filter and `clean_output()`
5. ❌ Brittle - breaks when models change token format

## Proper Solution: Template-Aware Parsing

### Architecture Overview

```
Model Config
    ↓
Template Format Specification
    ↓
├─ Special Token IDs (from tokenizer)
├─ Role Markers (system, user, assistant)
├─ Control Sequences (reasoning, end-of-text)
└─ Content Extraction Rules
    ↓
Apply at 3 Stages:
├─ 1. Tokenization: Mark special tokens
├─ 2. Streaming: Filter by token ID
└─ 3. Post-processing: Extract content
```

### Step 1: Define Template Formats

Each model family has a template format that specifies how messages are structured:

```rust
/// Template format specification for a model family
pub struct TemplateFormat {
    /// Format name (e.g., "ChatML", "DeepSeek", "Llama3")
    name: String,

    /// Special token IDs from tokenizer (loaded at runtime)
    special_tokens: SpecialTokens,

    /// Role markers and their structure
    roles: RoleMarkers,

    /// Content extraction rules
    extraction: ContentExtraction,
}

pub struct SpecialTokens {
    /// Beginning of sequence
    bos: Option<u32>,

    /// End of sequence
    eos: Vec<u32>,  // Models can have multiple EOS tokens

    /// Padding token
    pad: Option<u32>,

    /// Additional special tokens (reasoning markers, etc.)
    custom: HashMap<String, Vec<u32>>,
}

pub struct RoleMarkers {
    /// System message format: "<|im_start|>system\n{content}<|im_end|>"
    system: String,

    /// User message format: "<|im_start|>user\n{content}<|im_end|>"
    user: String,

    /// Assistant message format: "<|im_start|>assistant\n{content}<|im_end|>"
    assistant: String,
}

pub struct ContentExtraction {
    /// Reasoning markers to strip (DeepSeek: <think>...</think>)
    reasoning_markers: Vec<(String, String)>,  // (start, end)

    /// Extract only after the last occurrence of this pattern
    assistant_prefix: Option<String>,  // e.g., "assistant\n"

    /// Remove embedded role patterns
    strip_embedded_roles: bool,
}
```

### Step 2: Load Format from Tokenizer

When loading a model, parse the tokenizer's special tokens config:

```rust
impl TemplateFormat {
    /// Load template format from ONNX tokenizer
    pub fn from_tokenizer(
        family: ModelFamily,
        tokenizer: &Tokenizer,
    ) -> Result<Self> {
        // 1. Get special token IDs from tokenizer
        let special_tokens = Self::load_special_tokens(tokenizer)?;

        // 2. Get family-specific role markers
        let roles = Self::role_markers_for_family(family);

        // 3. Get family-specific extraction rules
        let extraction = Self::extraction_rules_for_family(family);

        Ok(Self {
            name: format!("{:?}", family),
            special_tokens,
            roles,
            extraction,
        })
    }

    fn load_special_tokens(tokenizer: &Tokenizer) -> Result<SpecialTokens> {
        // Parse tokenizer.json for special tokens
        // Example for Qwen:
        // - "<|im_start|>" -> 151644
        // - "<|im_end|>" -> 151645
        // - "<|endoftext|>" -> 151643

        // Example for DeepSeek:
        // - "<｜begin▁of▁sentence｜>" -> [token_ids...]
        // - "<｜end▁of▁sentence｜>" -> [token_ids...]
        // - "<think>" -> [token_ids...]
        // - "</think>" -> [token_ids...]

        let model = tokenizer.get_model();
        let added_tokens = tokenizer.get_added_tokens_decoder();

        // Build mapping of special token text -> token IDs
        let mut custom = HashMap::new();

        for (id, token) in added_tokens {
            if token.special {
                let token_text = token.content.clone();
                custom.entry(token_text)
                    .or_insert_with(Vec::new)
                    .push(id);
            }
        }

        Ok(SpecialTokens {
            bos: Self::find_token(tokenizer, "<|begin_of_text|>")
                .or_else(|| Self::find_token(tokenizer, "<s>")),
            eos: vec![
                Self::find_token(tokenizer, "<|im_end|>"),
                Self::find_token(tokenizer, "<|endoftext|>"),
                Self::find_token(tokenizer, "</s>"),
                Self::find_token(tokenizer, "<｜end▁of▁sentence｜>"),
            ].into_iter().flatten().collect(),
            pad: Self::find_token(tokenizer, "<pad>"),
            custom,
        })
    }
}
```

### Step 3: Family-Specific Configs

Define format configurations for each model family:

```rust
impl TemplateFormat {
    fn role_markers_for_family(family: ModelFamily) -> RoleMarkers {
        match family {
            ModelFamily::Qwen => RoleMarkers {
                system: "<|im_start|>system\n{content}<|im_end|>".to_string(),
                user: "<|im_start|>user\n{content}<|im_end|>".to_string(),
                assistant: "<|im_start|>assistant\n{content}<|im_end|>".to_string(),
            },
            ModelFamily::DeepSeek => RoleMarkers {
                system: "<｜begin▁of▁sentence｜>system\n{content}<｜end▁of▁sentence｜>".to_string(),
                user: "<｜begin▁of▁sentence｜>user\n{content}<｜end▁of▁sentence｜>".to_string(),
                assistant: "<｜begin▁of▁sentence｜>assistant\n{content}<｜end▁of▁sentence｜>".to_string(),
            },
            ModelFamily::Llama => RoleMarkers {
                system: "<|begin_of_text|><|start_header_id|>system<|end_header_id|>\n\n{content}<|eot_id|>".to_string(),
                user: "<|start_header_id|>user<|end_header_id|>\n\n{content}<|eot_id|>".to_string(),
                assistant: "<|start_header_id|>assistant<|end_header_id|>\n\n{content}<|eot_id|>".to_string(),
            },
            // ... other families
        }
    }

    fn extraction_rules_for_family(family: ModelFamily) -> ContentExtraction {
        match family {
            ModelFamily::Qwen => ContentExtraction {
                reasoning_markers: vec![],  // Qwen doesn't have reasoning markers
                assistant_prefix: Some("assistant\n".to_string()),
                strip_embedded_roles: true,
            },
            ModelFamily::DeepSeek => ContentExtraction {
                reasoning_markers: vec![
                    ("<think>".to_string(), "</think>".to_string()),
                ],
                assistant_prefix: Some("assistant\n".to_string()),
                strip_embedded_roles: true,
            },
            // ... other families
        }
    }
}
```

### Step 4: Apply During Streaming

**Current hack:**
```rust
// Filter by decoded text
let is_special = token_text.contains("<|");
```

**Proper approach:**
```rust
// Filter by token ID before decoding
pub fn should_stream_token(
    &self,
    token_id: u32,
) -> bool {
    // 1. Check if token is EOS
    if self.special_tokens.eos.contains(&token_id) {
        return false;
    }

    // 2. Check if token is a special token
    if self.is_special_token(token_id) {
        return false;
    }

    // 3. Stream content tokens
    true
}

fn is_special_token(&self, token_id: u32) -> bool {
    // Check against all known special tokens
    self.special_tokens.custom.values()
        .any(|ids| ids.contains(&token_id))
}
```

### Step 5: Apply During Post-Processing

Use extraction rules in `clean_output()`:

```rust
pub fn clean_output(&self, raw_output: &str) -> String {
    let mut cleaned = raw_output.to_string();

    // 1. Remove reasoning markers (DeepSeek <think>...</think>)
    for (start, end) in &self.extraction.reasoning_markers {
        while let Some(start_pos) = cleaned.find(start) {
            if let Some(end_pos) = cleaned[start_pos..].find(end) {
                let full_end = start_pos + end_pos + end.len();
                cleaned.replace_range(start_pos..full_end, "");
            } else {
                break;
            }
        }
    }

    // 2. Extract content after assistant prefix
    if let Some(prefix) = &self.extraction.assistant_prefix {
        if let Some(pos) = cleaned.rfind(prefix) {
            cleaned = cleaned[pos + prefix.len()..].to_string();
        }
    }

    // 3. Remove embedded role markers
    if self.extraction.strip_embedded_roles {
        for role in ["system", "user", "assistant"] {
            cleaned = cleaned.replace(&format!("\n{}\n", role), "");
        }
    }

    // 4. Final cleanup
    cleaned.trim().to_string()
}
```

## Implementation Plan

### Phase 1: Template Format Infrastructure (2-3 hours)
- [ ] Create `src/models/template_format.rs`
- [ ] Define `TemplateFormat`, `SpecialTokens`, `RoleMarkers`, `ContentExtraction` structs
- [ ] Implement `from_tokenizer()` to parse special tokens from tokenizer.json
- [ ] Add family-specific configs for Qwen, DeepSeek, Llama

### Phase 2: Adapter Integration (1-2 hours)
- [ ] Add `template_format: TemplateFormat` field to LocalModelAdapter
- [ ] Update `AdapterRegistry::get_adapter()` to load format from tokenizer
- [ ] Update `clean_output()` to use extraction rules
- [ ] Remove hardcoded token patterns from adapters

### Phase 3: Streaming Integration (2-3 hours)
- [ ] Add `should_stream_token(token_id)` method to TemplateFormat
- [ ] Update streaming callback in `src/local/generator.rs` to filter by token ID
- [ ] Remove text-based filtering hack
- [ ] Test streaming with Qwen and DeepSeek

### Phase 4: Testing & Validation (1-2 hours)
- [ ] Unit tests for each template format
- [ ] Integration tests for streaming with special tokens
- [ ] Verify output quality matches or exceeds current hack
- [ ] Test with multiple model families

**Total Estimate: 6-10 hours**

## Benefits

1. **Robust**: Filters by token ID, not fragile text patterns
2. **Model-Agnostic**: Easy to add new model families
3. **Maintainable**: Single source of truth for each format
4. **Correct**: Handles multi-token sequences properly
5. **Fast**: No string pattern matching during streaming
6. **Clear**: Separates concerns (tokenization vs extraction)

## Trade-offs

**Pros:**
- Proper architecture that scales to more models
- Fixes edge cases (multi-token sequences)
- Easier to debug (format is explicit)

**Cons:**
- More code (~300 lines vs current 10 lines)
- Requires loading tokenizer metadata
- Initial implementation time (6-10 hours)

## Current Hack vs Proper Solution

| Aspect | Current Hack | Proper Solution |
|--------|-------------|-----------------|
| **Code Size** | 10 lines | ~300 lines |
| **Correctness** | 90% | 99%+ |
| **Performance** | String matching | Token ID lookup (faster) |
| **Maintainability** | Hard to extend | Easy to add models |
| **Robustness** | Brittle | Handles edge cases |
| **Time to Implement** | 5 minutes | 6-10 hours |

## Recommendation

**Short-term:** Keep the hack, it works for 90% of cases and unblocks you now.

**Medium-term:** Implement proper template parsing when:
- You add 2+ more model families (complexity justifies it)
- Edge cases become problematic
- You have 6-10 hours for the refactor

**Long-term:** This is the right architecture for a production system that supports multiple model families.

## Related Files

- `src/models/adapters/mod.rs` - Current adapter interface
- `src/models/adapters/qwen.rs` - Qwen-specific `clean_output()`
- `src/models/adapters/deepseek.rs` - DeepSeek-specific `clean_output()`
- `src/local/generator.rs` - Streaming callback (current hack location)
- `src/models/loaders/onnx.rs` - Token generation and streaming

## References

- [ChatML Format](https://github.com/openai/openai-python/blob/main/chatml.md)
- [Llama 3 Prompt Format](https://llama.meta.com/docs/model-cards-and-prompt-formats/meta-llama-3/)
- [HuggingFace Tokenizers](https://huggingface.co/docs/tokenizers/index)
