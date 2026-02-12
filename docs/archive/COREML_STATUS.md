# CoreML Implementation Status

**Date:** 2026-02-10
**Current Phase:** Implementing CoreML backend for Apple Neural Engine support

## Problem Summary

### Metal Backend Limitation
- **Issue:** Candle Metal backend missing RMS-norm kernel
- **Error:** "Metal error no metal implementation for rms-norm"
- **Impact:** Cannot run Qwen 2.5 models on Metal GPU
- **Tested Versions:** Candle 0.8.4 → 0.9.2 (still missing)
- **Root Cause:** Qwen 2.5 uses RMS normalization, not implemented in Metal backend

### CPU Backend Issue
- **Issue:** CPU generation hangs indefinitely in forward pass
- **Impact:** CPU fallback unusable (either bug or extreme slowness)
- **Conclusion:** Need CoreML/ANE as primary solution

## Completed Work

### 1. Metal Investigation (Commit: 7fa82c1)
- ✅ Switched Metal to F16 precision (proper for Apple Silicon)
- ✅ Added error logging to file (metal_error.txt)
- ✅ Updated Candle 0.8 → 0.9.2
- ✅ Identified RMS-norm as blocker

### 2. Backend Configuration System (Commit: 98c130b)
- ✅ Created `BackendDevice` enum (CoreML/Metal/CUDA/CPU/Auto)
- ✅ Device availability detection
- ✅ Auto-select best available device
- ✅ Fallback chain configuration
- ✅ Model repository mapping:
  - CoreML: `anemll/Qwen2.5-{size}-Instruct` (.mlpackage)
  - Others: `Qwen/Qwen2.5-{size}-Instruct` (.safetensors)

### 3. Setup Wizard (Commit: 98c130b)
- ✅ Multi-step TUI wizard:
  - Step 1: Claude API key (masked input)
  - Step 2: HuggingFace token (optional)
  - Step 3: Device selection with descriptions
  - Step 4: Confirmation summary
- ✅ Saves to `~/.shammah/config.toml`
- ✅ Persists across runs

### 4. Config Management
- ✅ Added `backend: BackendConfig` to Config struct
- ✅ TOML serialization/deserialization
- ✅ Load backend config from existing files
- ✅ Save backend config to TOML

## In Progress

### CoreML Backend Implementation
**Current Step:** Implementing CoreML support for Apple Neural Engine

**Tasks:**
- [ ] Add CoreML dependency (candle-coreml or direct bindings)
- [ ] Implement CoreMLGenerator struct
- [ ] Implement TextGeneration trait for CoreML
- [ ] Load .mlpackage models from anemll org
- [ ] Handle component splitting for large models
- [ ] Test with Qwen2.5-3B-Instruct CoreML variant

## Next Steps (After CoreML)

1. **Model Downloader Updates**
   - Update bootstrap to check backend device
   - Download from anemll org for CoreML
   - Download from Qwen org for other backends
   - Handle .mlpackage vs .safetensors formats

2. **Startup Integration**
   - Check if config exists
   - If not, show setup wizard
   - If device is Auto, resolve to concrete device
   - Load appropriate model for selected backend

3. **Dtype Handling**
   - CoreML: F16 or FP32 (depending on ANE optimization)
   - Metal: F16 (GPU optimized)
   - CPU: F32 (better compatibility)
   - CUDA: F16 or mixed precision

4. **Testing & Validation**
   - Test CoreML generation on Apple Silicon
   - Verify ANE is actually used (Activity Monitor)
   - Benchmark vs Metal/CPU
   - Test fallback chain (CoreML → CPU if CoreML fails)
   - Test model downloads for different backends

## Technical Details

### File Structure
```
src/
├── config/
│   ├── backend.rs         # NEW: BackendDevice enum, BackendConfig
│   ├── loader.rs          # UPDATED: Load backend from TOML
│   ├── settings.rs        # UPDATED: Added backend field, save()
│   └── mod.rs             # UPDATED: Export BackendConfig
├── cli/
│   ├── setup_wizard.rs    # NEW: Multi-step TUI wizard
│   └── mod.rs             # UPDATED: Export show_setup_wizard
├── models/
│   ├── generator_new.rs   # TO UPDATE: Add CoreML variant
│   ├── qwen_loader.rs     # CURRENT: Loads .safetensors via Candle
│   └── coreml_loader.rs   # TO CREATE: Loads .mlpackage via CoreML
```

### Config File Format
```toml
# ~/.shammah/config.toml
api_key = "sk-ant-..."
streaming_enabled = true

[backend]
device = "CoreML"  # or "Metal", "Cuda", "Cpu"
model_repo = "anemll/Qwen2.5-3B-Instruct"
model_path = "~/.cache/shammah/models/coreml/qwen-3b.mlpackage"
fallback_chain = ["CoreML", "Metal", "Cpu"]
```

### CoreML Model Sources
- **Organization:** anemll (HuggingFace)
- **Models:** Qwen2.5-1.5B/3B/7B/14B-Instruct
- **Format:** .mlpackage (CoreML native format)
- **Optimization:** Pre-converted for Apple Neural Engine
- **Repo Examples:**
  - `anemll/Qwen2.5-1.5B-Instruct`
  - `anemll/Qwen2.5-3B-Instruct`

## Dependencies to Add

### Option 1: Use candle-coreml (if exists)
```toml
[target.'cfg(target_os = "macos")'.dependencies]
candle-coreml = { version = "0.9", optional = true }
```

### Option 2: Direct CoreML bindings
```toml
[target.'cfg(target_os = "macos")'.dependencies]
metal = "0.27"
objc = "0.2"
cocoa = "0.25"
core-foundation = "0.9"
# Use objc2 for modern CoreML bindings
objc2 = "0.5"
objc2-foundation = "0.2"
```

## Questions to Resolve

1. **Does candle-coreml exist?**
   - If yes: Use it directly
   - If no: Create direct CoreML bindings

2. **CoreML model precision?**
   - Check anemll models: FP16 or FP32?
   - ANE prefers FP16 for compute, FP32 for weights (quantized at runtime)

3. **Component splitting?**
   - Do anemll models use component splitting for large models?
   - How to handle multi-file .mlpackage?

## References

- **Roadmap:** COREML_MULTIBACKEND_ROADMAP.md
- **Metal Issue:** metal_error.txt (RMS-norm missing)
- **Qwen Models:** https://huggingface.co/Qwen
- **CoreML Models:** https://huggingface.co/anemll
- **Previous Work:**
  - QWEN_INTEGRATION_COMPLETE.md (Phases 1-4)
  - PHASE_3_BOOTSTRAP_COMPLETE.md (Progressive bootstrap)

## Success Criteria

✅ **Phase Complete When:**
1. CoreML backend loads .mlpackage models
2. Generation works on Apple Neural Engine
3. Activity Monitor confirms ANE usage (not CPU/GPU)
4. Performance is 2-10x better than Metal (if Metal worked)
5. Fallback to CPU works if CoreML unavailable
6. Setup wizard configures backend on first run
7. Model downloads work for all backends

## Current Session Context

- Working on Apple Silicon Mac (16GB RAM, selects Qwen-3B)
- Metal RMS-norm confirmed missing in Candle 0.9.2
- CPU generation hangs (unusable)
- CoreML is the only viable path forward for local inference
- Setup wizard and config system ready for integration
- Next: Implement CoreML backend itself
