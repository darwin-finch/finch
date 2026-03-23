# ONNX Model Integration

*See `CLAUDE.md §Key Design Decisions` for why ONNX over Candle.*

**Purpose:** Load pre-trained models in ONNX format with KV cache support.

## Model selection by RAM

| RAM | Model | Size |
|-----|-------|------|
| 8 GB | Qwen-2.5-1.5B | 1.5 GB |
| 16 GB | Qwen-2.5-3B | 3 GB |
| 32 GB | Qwen-2.5-7B | 7 GB |
| 64 GB+ | Qwen-2.5-14B | 14 GB |

## Execution providers

- **macOS/Apple Silicon**: CoreML EP — dispatches ops to ANE/GPU/CPU per-op. In practice, LLM workloads run mostly on CPU ARM because many transformer ops aren't in CoreML's op set.
- **Linux**: CUDA → ROCm → DirectML → CPU (in priority order)

## Why ONNX (not Candle) on macOS

`candle-metal` is missing layer-norm kernels and certain matmul dimension combinations required by Qwen — causes incorrect output or crashes. `candle-coreml` requires ANEMLL `.mlpackage` format, incompatible with PyTorch/safetensors. ONNX + CoreML EP is the practical path. ONNX also supports all 6 model families vs. Candle's Qwen2-only support.

**Candle backend** (`src/models/loaders/candle.rs`): works on Linux CPU/CUDA; Qwen2 only; macOS CPU works, Metal unreliable.

**Mistral ONNX:** Models exist at `microsoft/` and `nvidia/` HuggingFace orgs, but `onnx-community` hasn't published Mistral yet. Tracked as Issue #2.

## Key files

- `src/models/loaders/onnx.rs` — `OnnxLoader`, `LoadedOnnxModel`, KV cache
- `src/models/loaders/candle.rs` — Candle backend (Linux/CPU, Qwen2 only)
- `src/models/loaders/onnx_config.rs` — Configuration types
- `src/models/unified_loader.rs` — Dispatches to ONNX or Candle based on config
