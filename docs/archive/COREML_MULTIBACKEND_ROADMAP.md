# CoreML Multi-Backend Support Roadmap

**Status:** Planning Phase
**Priority:** High (Core feature for Shammah vision)
**Target:** Phase 5 (after Metal fix + tool execution stability)

## Vision

Enable Shammah to use the best available compute backend on any platform:
- **macOS**: Apple Neural Engine (CoreML) → fastest, best battery
- **macOS**: Metal GPU (Candle) → fast, flexible fallback
- **Windows/Linux**: NVIDIA CUDA → fast on compatible hardware
- **Universal**: CPU fallback → works everywhere

## Why This Matters

1. **ANE was always the goal** - Original Shammah vision includes CoreML/ANE support
2. **Cross-platform** - Same codebase works on Mac/Windows/Linux
3. **Optimal performance** - Users get best speed for their hardware
4. **Graceful degradation** - Falls back automatically if preferred unavailable

## Architecture

### Multi-Backend Enum

```rust
pub enum InferenceBackend {
    #[cfg(target_os = "macos")]
    CoreML(CoreMLModel),      // .mlpackage from anemll org

    #[cfg(target_os = "macos")]
    Metal(MetalModel),         // .safetensors via Candle

    #[cfg(feature = "cuda")]
    Cuda(CudaModel),           // .safetensors via Candle

    Cpu(CpuModel),             // .safetensors via Candle
}
```

### Model Variants by Backend

| Backend | Model Format | HuggingFace Repo | Notes |
|---------|--------------|------------------|-------|
| CoreML | `.mlpackage` | `anemll/Qwen2.5-3B-Instruct` | Pre-converted for ANE |
| Metal | `.safetensors` | `Qwen/Qwen2.5-3B-Instruct` | Official Qwen repo |
| CUDA | `.safetensors` | `Qwen/Qwen2.5-3B-Instruct` | Same as Metal |
| CPU | `.safetensors` | `Qwen/Qwen2.5-3B-Instruct` | Same as Metal |

## Implementation Milestones

### Milestone 1: First-Run Device Selection ⏳

**Goal:** Interactive setup that detects and lets user choose their backend

**Tasks:**
- [ ] Detect available backends at runtime
  - [ ] Check for ANE support (macOS only)
  - [ ] Check for Metal support (macOS only)
  - [ ] Check for CUDA support (if feature enabled)
  - [ ] CPU always available
- [ ] Interactive TUI prompt on first run
  - [ ] List available backends with descriptions
  - [ ] Show performance/battery tradeoffs
  - [ ] Get user selection
- [ ] Save backend preference to config
  - [ ] `~/.shammah/config.toml` → `[backend] type = "..."`
  - [ ] Allow manual editing for advanced users

**Deliverable:** `shammah` runs setup wizard on first launch

---

### Milestone 2: Model Download per Backend ⏳

**Goal:** Download correct model format for chosen backend

**Tasks:**
- [ ] Implement model variant detection
  - [ ] Map backend → (repo, format)
  - [ ] CoreML → `anemll` org, `.mlpackage`
  - [ ] Others → official Qwen, `.safetensors`
- [ ] Download logic per format
  - [ ] `.mlpackage` download (might be multi-file)
  - [ ] `.safetensors` download (existing code)
- [ ] Progress indicators
  - [ ] Show download progress with `indicatif`
  - [ ] Estimate time remaining
- [ ] Caching strategy
  - [ ] Store models in `~/.cache/shammah/models/`
  - [ ] Separate dirs per backend: `coreml/`, `metal/`, etc.

**Deliverable:** Correct model downloaded based on user's backend choice

---

### Milestone 3: CoreML Backend Implementation 🚧

**Goal:** Add `candle-coreml` support for ANE inference

**Dependencies:**
- `candle-coreml` crate (if available, or create wrapper)
- Pre-converted models from `anemll` org

**Tasks:**
- [ ] Add `candle-coreml` dependency
  - [ ] Feature flag: `coreml` (optional, macOS only)
  - [ ] Conditional compilation: `#[cfg(target_os = "macos")]`
- [ ] Implement `CoreMLModel` struct
  - [ ] Load `.mlpackage` files
  - [ ] Handle component splitting (large models)
  - [ ] Implement inference API
- [ ] Unified `TextGeneration` trait
  - [ ] `fn generate(&mut self, input: &str, max_tokens: usize) -> Result<String>`
  - [ ] All backends implement same trait
- [ ] Test with Qwen-3B CoreML
  - [ ] Verify ANE is actually being used
  - [ ] Benchmark vs Metal/CPU
  - [ ] Check power consumption

**Deliverable:** Working CoreML backend with ANE acceleration

---

### Milestone 4: Unified Backend Interface 🎯

**Goal:** Rest of codebase doesn't care which backend is used

**Tasks:**
- [ ] Refactor `LocalGenerator` to use trait
  - [ ] Replace direct Qwen model calls
  - [ ] Use `Box<dyn TextGeneration>` or similar
- [ ] Runtime backend selection
  - [ ] `BackendConfig::best_available()` → auto-detect
  - [ ] Respect user preference from config
  - [ ] Graceful fallback if preferred unavailable
- [ ] Backend switching (advanced)
  - [ ] Allow switching backends without restart
  - [ ] Useful for testing/benchmarking

**Deliverable:** Seamless backend abstraction

---

### Milestone 5: Cross-Platform Build System 🌍

**Goal:** Single codebase builds correctly on all platforms

**Tasks:**
- [ ] Cargo feature flags
  ```toml
  [features]
  default = ["cpu"]
  coreml = ["candle-coreml"]  # macOS only
  metal = []                   # macOS only
  cuda = ["candle-core/cuda"]  # NVIDIA
  ```
- [ ] Platform-specific dependencies
  ```toml
  [target.'cfg(target_os = "macos")'.dependencies]
  candle-coreml = { version = "...", optional = true }
  ```
- [ ] CI/CD for multiple platforms
  - [ ] GitHub Actions: macOS, Linux, Windows
  - [ ] Test each backend where available
- [ ] Binary distribution
  - [ ] macOS binary with CoreML support
  - [ ] Windows binary with CUDA support
  - [ ] Linux binary (CPU + CUDA)

**Deliverable:** Builds and runs on Mac/Windows/Linux

---

## Configuration Schema

### `~/.shammah/config.toml`

```toml
[backend]
# Selected backend ("CoreML", "Metal", "Cuda", or "Cpu")
type = "CoreML"

# Model repository and path
model_repo = "anemll/Qwen2.5-3B-Instruct"
model_path = "~/.cache/shammah/models/coreml/qwen-3b.mlpackage"

# Preference order (fallback chain)
prefer = ["CoreML", "Metal", "Cuda", "Cpu"]

[backend.coreml]
# CoreML-specific settings
use_ane = true  # Enable Neural Engine
component_splitting = true  # For large models
```

## First-Run Experience

### Example Flow (macOS)

```
🚀 Welcome to Shammah!

Detecting available compute devices...

Available acceleration:
  1. Apple Neural Engine (ANE) - Fastest, best battery life ⚡️
  2. Metal GPU - Fast, flexible 🚀
  3. CPU - Slow, works everywhere 🐌

Select device (1-3): 1

✓ Apple Neural Engine selected

📥 Downloading CoreML model from anemll/Qwen2.5-3B-Instruct...
⏳ Downloading qwen-3b.mlpackage (1.2 GB)...
[████████████████████████████████] 100% • 45s

✓ Model ready!
✓ Configuration saved to ~/.shammah/config.toml

🎉 Setup complete! Starting Shammah...
```

### Example Flow (Windows)

```
🚀 Welcome to Shammah!

Detecting available compute devices...

Available acceleration:
  1. NVIDIA CUDA (RTX 3080) - Very fast 🚀
  2. CPU (Intel i7) - Slow 🐌

Select device (1-2): 1

✓ NVIDIA CUDA selected

📥 Downloading model from Qwen/Qwen2.5-3B-Instruct...
⏳ Downloading model.safetensors (5.4 GB)...
[████████████████████████████████] 100% • 2m 15s

✓ Model ready!

🎉 Setup complete! Starting Shammah...
```

## Testing Strategy

### Backend-Specific Tests

- [ ] **CoreML**: Verify ANE is used (not CPU/GPU fallback)
- [ ] **Metal**: Verify GPU utilization
- [ ] **CUDA**: Verify GPU utilization
- [ ] **CPU**: Baseline performance

### Cross-Platform CI

```yaml
# .github/workflows/test.yml
jobs:
  test-macos-coreml:
    runs-on: macos-latest
    steps:
      - run: cargo test --features coreml

  test-windows-cuda:
    runs-on: windows-gpu
    steps:
      - run: cargo test --features cuda

  test-linux-cpu:
    runs-on: ubuntu-latest
    steps:
      - run: cargo test
```

### Benchmarks

Track performance across backends:

| Backend | Tokens/sec | Power (W) | Startup (s) |
|---------|-----------|-----------|-------------|
| CoreML (ANE) | ~50-100 | ~5-10 | ~1-2 |
| Metal (GPU) | ~30-60 | ~15-25 | ~2-3 |
| CUDA (RTX 3080) | ~80-120 | ~150-200 | ~3-5 |
| CPU (M3 Max) | ~5-12 | ~30-40 | ~5-10 |

## Dependencies

### New Crates

```toml
[dependencies]
# CoreML support (macOS only)
candle-coreml = { version = "0.1", optional = true }

# Or if candle-coreml doesn't exist, use direct bindings:
coreml-rs = { version = "0.1", optional = true, target = "macos" }
```

### Research Needed

- [ ] Does `candle-coreml` crate exist? (Gemini suggests yes)
- [ ] If not, can we create Rust bindings to CoreML?
- [ ] Compatibility with `anemll` models?
- [ ] Component splitting for large models?

## Success Criteria

✅ **Milestone 1 Complete When:**
- User sees device selection on first run
- Choice is saved to config
- Preference is respected on subsequent runs

✅ **Milestone 2 Complete When:**
- Correct model format downloaded per backend
- Models cached efficiently
- Download progress shown to user

✅ **Milestone 3 Complete When:**
- CoreML backend loads and runs inference
- ANE is actually being used (verify with Activity Monitor)
- Performance is 2-3x better than Metal

✅ **Milestone 4 Complete When:**
- All backends use same `TextGeneration` trait
- Switching backends doesn't break anything
- Fallback chain works automatically

✅ **Milestone 5 Complete When:**
- Builds on macOS, Windows, Linux
- CI passes on all platforms
- Release binaries available per platform

## Future Enhancements

### Phase 6+
- [ ] Multiple model support (1.5B, 3B, 7B, 14B)
- [ ] Quantization options (INT8, FP16, FP32)
- [ ] Model hot-swapping
- [ ] Benchmark mode (compare all backends)
- [ ] Power profiling integration
- [ ] Remote backend support (API fallback)

## Related Documents

- `CLAUDE.md` - Original Shammah vision (mentions CoreML)
- `QWEN_INTEGRATION_COMPLETE.md` - Current Metal/CPU implementation
- `PHASE_3_BOOTSTRAP_COMPLETE.md` - Progressive bootstrap pattern

## Notes

- CoreML support is **not a replacement** for Metal/CUDA - it's an **addition**
- Cross-platform compatibility is **non-negotiable**
- First-run UX must be **simple and fast** (< 5 minutes)
- Fallback chain ensures it **always works** somewhere
