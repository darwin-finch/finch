# ONNX Model Integration

*Background: [local backend investigation](../../DESIGN.md#local-backend-investigation) in the design index.*

**Purpose:** Load pre-trained models in ONNX format with KV cache support.

## Model selection by RAM

`ModelSelector::select_for_system` (`src/models/model_selector.rs`) picks the
largest Qwen-2.5 variant the machine's RAM admits; below 3 GB it selects
cloud-only mode. Download sizes come from the same module.

| System RAM | Selected model | Download size |
|------------|----------------|---------------|
| < 3 GB | cloud-only (teacher API) | — |
| 3–6 GB | Qwen-2.5-0.5B | 0.5 GB |
| 6–12 GB | Qwen-2.5-1.5B | 1.5 GB |
| 12–24 GB | Qwen-2.5-3B | 3 GB |
| 24–48 GB | Qwen-2.5-7B | 7 GB |
| 48 GB+ | Qwen-2.5-14B | 14 GB |

## Execution providers

- **macOS/Apple Silicon**: CoreML EP — dispatches ops to ANE/GPU/CPU per-op. In practice, LLM workloads run mostly on CPU ARM because many transformer ops aren't in CoreML's op set.
- **Linux**: CUDA (only when the non-default `cuda` cargo feature is enabled) → CPU fallback. Nothing in `src` registers a ROCm provider; DirectML is Windows-only and only when explicitly configured (`src/models/loaders/onnx.rs::get_execution_providers`).

## Why ONNX (not Candle) on macOS

`candle-metal` is missing layer-norm kernels and certain matmul dimension combinations required by Qwen — causes incorrect output or crashes. `candle-coreml` requires ANEMLL `.mlpackage` format, incompatible with PyTorch/safetensors. ONNX + CoreML EP is the practical path. ONNX loaders cover the five `QwenSize` variants (`Qwen500M` through `Qwen14B` in `src/models/model_selector.rs`); the Candle backend loads the Qwen2 family only. Loader coverage is a configuration surface, not end-to-end conformance.

**Candle backend** (`src/models/loaders/candle.rs`): works on Linux CPU; Qwen2 only; macOS CPU works, Metal unreliable.

**Mistral ONNX:** Models exist at `microsoft/` and `nvidia/` HuggingFace orgs, but `onnx-community` hasn't published Mistral yet. Tracked as Issue #2.

## Key files

- `src/models/loaders/onnx.rs` — `OnnxLoader`, `LoadedOnnxModel`, KV cache
- `src/models/loaders/candle.rs` — Candle backend (Linux/CPU, Qwen2 only)
- `src/models/loaders/onnx_config.rs` — Configuration types
- `src/models/unified_loader.rs` — Dispatches to ONNX or Candle based on config
