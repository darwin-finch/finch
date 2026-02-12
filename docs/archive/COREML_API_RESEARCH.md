# CoreML API Research Summary

**Date:** 2026-02-10
**Status:** API discovered, implementation plan defined

## candle-coreml Crate API

### Key Discovery

The `candle-coreml` crate (v0.3) provides a high-level `UnifiedModelLoader` that simplifies CoreML model usage significantly.

### Core API

```rust
use candle_coreml::UnifiedModelLoader;

// 1. Initialize loader
let loader = UnifiedModelLoader::new()?;

// 2. Load model from HuggingFace (handles download + config)
let mut model = loader.load_model("anemll/Qwen2.5-3B-Instruct")?;

// 3. Generate text (simple API)
let response = model.complete_text(
    "Hello, how are you?",  // prompt
    50,                      // max tokens
    0.8,                     // temperature
)?;

// 4. Advanced generation with top-k
let tokens = model.generate_tokens_topk_temp(
    "Hello, how are you?",
    50,        // max tokens
    0.8,       // temperature
    Some(50),  // top_k
)?;
```

### Low-Level API

```rust
use candle_coreml::CoreMLModel;

// Load from local .mlpackage
let config = ModelConfig::load_from_file("model_config.json")?;
let model = CoreMLModel::load_from_file("model.mlpackage", &config)?;

// Forward pass with tensors
let output = model.forward(&[input_tensor])?;
```

## Model Format

### Repository Structure

CoreML models from `anemll` organization:
- `anemll/Qwen2.5-1.5B-Instruct`
- `anemll/Qwen2.5-3B-Instruct`
- `anemll/Qwen2.5-7B-Instruct`
- `anemll/Qwen2.5-14B-Instruct`

### File Structure

```
model/
├── model.mlpackage/       # CoreML model package
├── config.json            # Model configuration
└── tokenizer.json         # Tokenizer (optional with UnifiedModelLoader)
```

## Implementation Plan

### Approach 1: High-Level API (Recommended)

**Pros:**
- Simple, clean API
- Handles tokenization internally
- Automatic HuggingFace downloads
- Built-in sampling

**Cons:**
- Less control over generation
- May not match existing TextGeneration trait exactly

**Implementation:**

```rust
pub struct LoadedCoreMLModel {
    model: UnifiedModelLoader, // or whatever type load_model returns
    max_length: usize,
}

impl LoadedCoreMLModel {
    pub fn load(model_repo: &str) -> Result<Self> {
        let loader = UnifiedModelLoader::new()?;
        let model = loader.load_model(model_repo)?;
        Ok(Self { model, max_length: 2048 })
    }

    pub fn generate(&mut self, prompt: &str, max_tokens: usize) -> Result<String> {
        self.model.complete_text(prompt, max_tokens, 0.8)
    }
}
```

### Approach 2: Low-Level API

**Pros:**
- Full control over inference
- Can match TextGeneration trait exactly
- Custom sampling strategies

**Cons:**
- More complex
- Must handle tokenization separately
- Must implement sampling logic

**Implementation:**

```rust
pub struct LoadedCoreMLModel {
    model: CoreMLModel,
    tokenizer: Tokenizer,
    max_length: usize,
}

impl LoadedCoreMLModel {
    pub fn generate(&mut self, prompt: &str, max_tokens: usize) -> Result<String> {
        // 1. Tokenize input
        let tokens = self.tokenizer.encode(prompt, true)?;
        let input_ids = tokens.get_ids();

        // 2. Convert to Candle Tensor
        let input_tensor = Tensor::from_vec(
            input_ids.to_vec(),
            (1, input_ids.len()),
            &Device::Cpu,
        )?;

        // 3. Autoregressive generation loop
        let mut generated_ids = input_ids.to_vec();
        for _ in 0..max_tokens {
            let output = self.model.forward(&[&input_tensor])?;

            // 4. Sample next token from logits
            let next_token = sample_from_logits(&output)?;

            if next_token == EOS_TOKEN {
                break;
            }

            generated_ids.push(next_token);
        }

        // 5. Decode to text
        self.tokenizer.decode(&generated_ids, true)
    }
}
```

## Recommended Approach

**Use Approach 1 (High-Level API)** for initial implementation:

1. It's simpler and faster to implement
2. We can always refactor to low-level later if needed
3. The high-level API handles many edge cases for us
4. Better match for anemll pre-converted models

### Integration with TextGeneration Trait

The challenge: `TextGeneration` trait expects:
- Input: `&[u32]` (token IDs)
- Output: `Vec<u32>` (token IDs)

But `complete_text` expects:
- Input: `&str` (text)
- Output: `String` (text)

**Solution Options:**

1. **Keep CoreML separate from TextGeneration trait**
   - Create CoreMLGenerator that doesn't implement TextGeneration
   - Handle text I/O at a higher level
   - Simpler, cleaner

2. **Add tokenizer to CoreMLGenerator**
   - Decode input token IDs to text
   - Call complete_text()
   - Encode output text back to token IDs
   - Matches existing pattern, but adds overhead

3. **Modify TextGeneration trait**
   - Add text-based generation method
   - Both Qwen and CoreML implement it differently
   - More flexible, but bigger refactor

**Recommendation:** Option 1 - Keep CoreML separate initially.

## Next Steps

1. **Update CoreML Loader**
   - Use UnifiedModelLoader
   - Implement complete_text wrapper
   - Test with anemll models

2. **Integration**
   - Create CoreMLGenerator (no TextGeneration trait)
   - Update GeneratorModel to handle both types
   - Test with actual CoreML models

3. **Testing**
   - Download anemll/Qwen2.5-3B-Instruct
   - Test generation
   - Verify ANE usage (Activity Monitor)
   - Benchmark performance

4. **Documentation**
   - Update COREML_STATUS.md
   - Add usage examples
   - Document API choices

## References

- **crates.io:** https://crates.io/crates/candle-coreml
- **GitHub:** https://github.com/mazhewitt/candle-cormel
- **docs.rs:** https://docs.rs/candle-coreml
- **anemll models:** https://huggingface.co/anemll

## API Characteristics

### Thread Safety
- CoreML models aren't inherently thread-safe
- Need `unsafe impl Send + Sync`
- Wrap in `Arc<RwLock<>>` at higher level

### Device Handling
- CoreML doesn't use Candle's Device enum
- Creates tensors on CPU or Metal internally
- ANE usage is automatic if available

### Performance
- Optimized for Apple Neural Engine
- Expected 2-10x faster than Metal (if Metal worked)
- Much faster than CPU
- Lower battery usage than GPU

## Implementation Status

- ✅ API discovered and documented
- ✅ High-level approach identified
- ✅ Integration strategy defined
- ⏳ Loader implementation (stub exists)
- ⏳ Generator integration (stub exists)
- ⏳ Testing with real models
- ⏳ Performance benchmarking
