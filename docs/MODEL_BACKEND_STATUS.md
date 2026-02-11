# Model Backend Status and Limitations

**Last Updated**: 2026-02-10
**Purpose**: Document what works and what doesn't for different model backends

## Summary

| Backend | Status | Notes |
|---------|--------|-------|
| **CoreML (ANE)** | ⚠️ Partially Working | Loads but runtime tensor mismatch during generation |
| **Metal (GPU)** | ❌ Not Working | Missing operation support (likely rms_norm) |
| **CPU** | ❌ Not Practical | Works but too slow for real-time use |
| **CUDA** | N/A | Not available on macOS |

## Detailed Status

### CoreML (Apple Neural Engine)

**Status**: ⚠️ Model loads successfully, generation fails at runtime

**What Works**:
- ✅ Model download from HuggingFace (anemll/anemll-Qwen-Qwen3-0.6B-ctx512_0.3.4)
- ✅ Component discovery from meta.yaml
- ✅ Metadata parsing from .mlmodelc files
- ✅ Tensor spec population (inputs/outputs from metadata.json)
- ✅ Model loading without panics
- ✅ Configuration properly populated

**What Doesn't Work**:
- ❌ Runtime generation fails with tensor dimension mismatch
- Error: `narrow invalid args start + len > dim_len: [1, 1, 1, 512], dim: 2, start: 1, len:1`
- Location: `candle_coreml::qwen::inference::run_chatpy_prefill` (line 368)

**Root Cause**:
- Incompatibility between anemll CoreML model format and candle-coreml 0.3.1
- The library expects different tensor dimensions than what this model provides
- Hardcoded batch_size=1 may conflict with model's internal expectations
- Tensor slicing operations assume shapes that don't match this model

**Technical Details**:
```rust
// Model loads with:
- batch_size: 1 (overridden from meta.yaml's 64)
- context_length: 512
- Components: embeddings, lm_head, ffn_prefill, ffn_infer

// Runtime error during narrow() operation:
- Tensor shape: [1, 1, 1, 512]
- Attempting: narrow(dim=2, start=1, len=1)
- Problem: dim 2 has size 1, so valid indices are [0] only
```

**Possible Solutions** (not yet tried):
1. Try different CoreML model (not anemll format)
2. Use candle-coreml from git main (may have fixes)
3. Patch candle-coreml to handle anemll's tensor layout
4. Contact anemll/candle-coreml maintainers about compatibility

**Why Not Auto-Discovery**:
- Passing `None` config → "ModelConfig missing 'embeddings' component"
- Library's default config doesn't scan directory properly
- Must explicitly provide component paths and tensor specs
- Cannot rely on library's auto-discovery for this model format

### Metal (Apple GPU)

**Status**: ❌ Does not work - missing operation support

**Error**: Model loading fails due to unsupported operations

**Root Cause**:
- Qwen models use operations not supported by candle's Metal backend
- Likely missing: rms_norm or other layer normalization operations
- Metal backend has limited operation coverage compared to CPU/CUDA

**User Quote**:
> "METAL DOES NOT WORK"

**Why This Fails**:
- candle-core's Metal backend doesn't implement all operations
- Qwen model architecture requires operations Metal doesn't have
- Not a configuration issue - fundamental backend limitation

**NOT A SOLUTION**: Do not suggest Metal as a fallback

### CPU

**Status**: ❌ Not practical - works but too slow

**Technical Status**: Functionally works, but performance unacceptable

**Why This Fails**:
- CPU inference on even small LLMs (0.6B params) is extremely slow
- Real-time generation requires GPU/ANE acceleration
- Would work for testing but not production use

**NOT A SOLUTION**: Do not suggest CPU as a fallback for real use

### Current Workaround

**Graceful Degradation**: Forward to Claude API when local generation fails

```rust
// Router behavior:
if model_ready && try_local_generation() {
    return local_response;
} else {
    return forward_to_claude_api();
}
```

**This means**:
- System remains functional even with broken local model
- Users get responses from Claude API as fallback
- No blocking errors - degraded but working

## Lessons Learned

### 1. CoreML Model Compatibility is Complex

CoreML models come in different formats and versions:
- anemll models use specific conventions
- candle-coreml expects certain formats
- Not all CoreML → candle-coreml combinations work
- Tensor specs must be manually extracted from metadata.json

### 2. Backend Support Varies Widely

Different backends support different operations:
- CoreML: Good operation coverage, but model format issues
- Metal: Limited operation support, missing key ops
- CPU: Complete operation support, but too slow
- CUDA: Not available on macOS

### 3. Configuration is Not Enough

Even with correct configuration:
- Models can load successfully
- But fail at runtime with tensor/shape mismatches
- Must test generation, not just loading

### 4. Auto-Discovery is Unreliable

candle-coreml's auto-discovery (passing None config):
- Doesn't work for anemll models
- Requires explicit component paths
- Requires explicit tensor specifications
- Cannot rely on default behavior

### 5. Batch Size Matters

Setting batch_size=1 vs 64:
- meta.yaml says 64
- CoreML single-query inference expects 1
- Wrong batch_size causes shape mismatches
- Not clear which is "correct"

## What Actually Works Today

**Current Working Setup**:
- Local model: None (all backends have issues)
- Fallback: Claude API (always works)
- Router: Forwards all queries to Claude

**This is acceptable because**:
- System remains functional
- No degraded user experience (Claude is high quality)
- Progressive enhancement approach (local model is a bonus)
- When local model works, it will be transparent

## Future Paths Forward

### Path 1: Different CoreML Model
- Try non-anemll CoreML Qwen models
- Look for models explicitly tested with candle-coreml
- Check candle-coreml examples for compatible models

### Path 2: Upgrade candle-coreml
- Current version: 0.3.1
- Check git main for anemll compatibility fixes
- May require building from source

### Path 3: Fix Metal Support
- Contribute missing operations to candle-core Metal backend
- Would benefit entire Rust ML ecosystem
- Significant engineering effort

### Path 4: Use Different Model Family
- Try Llama/Mistral/Gemma instead of Qwen
- May have better Metal/CoreML support
- Different architecture might avoid problematic operations

### Path 5: Accept Current State
- Claude API fallback works well
- Focus on other features (LoRA training, tool execution, etc.)
- Revisit local inference when ecosystem matures

## Testing Checklist

When trying new model backends, verify:

- [ ] Model downloads successfully
- [ ] Configuration loads without errors
- [ ] Components discovered/loaded
- [ ] Tensor specs populated
- [ ] Model initialization completes
- [ ] **Test generation with simple prompt** (don't stop at loading!)
- [ ] Check for runtime errors (not just load errors)
- [ ] Verify output quality
- [ ] Measure inference speed

**Critical**: Don't declare success until generation works!

## References

- CoreML loading implementation: `src/models/loaders/coreml.rs`
- Bootstrap/loading orchestration: `src/models/bootstrap.rs`
- Unified model loader: `src/models/unified_loader.rs`
- Router with fallback: `src/router/decision.rs`

- anemll model repo: https://huggingface.co/anemll/anemll-Qwen-Qwen3-0.6B-ctx512_0.3.4
- candle-coreml crate: https://crates.io/crates/candle-coreml
- Qwen model architecture: https://huggingface.co/Qwen

---

**Remember**: Metal and CPU are NOT viable fallbacks. Only CoreML (with fixes) or Claude API work for production use.
