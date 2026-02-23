# Model Backend Status

**Last Updated**: 2026-02-22
**Purpose**: Document what works, what doesn't, and why — for both local inference and LoRA fine-tuning.

---

## Current Status: ONNX Runtime is the working path

| Backend | Status | Notes |
|---------|--------|-------|
| **ONNX + CoreML EP (macOS)** | ✅ Working | Primary path; ops dispatch to ANE/GPU/CPU per-op |
| **ONNX + CPU (Linux)** | ✅ Working | Clean CPU fallback on Linux |
| **ONNX + CUDA (Linux)** | ✅ Working | Standard CUDA execution provider |
| **Candle CPU (Linux)** | ✅ Working | Alternative backend; Qwen2 only |
| **Candle CUDA (Linux)** | ✅ Working | `--features candle-cuda` |
| **Candle Metal (macOS)** | ❌ Broken | Missing layer-norm kernel; matmul edge cases; wrong/no output |
| **candle-coreml (ANEMLL)** | ❌ Not viable | Wrong model format; niche 3rd-party crate |

---

## Candle Metal — Why It Doesn't Work

`candle-metal` is Candle's Metal GPU backend (macOS GPU path). It fails for Qwen models because:

- **Missing layer normalisation kernel** — `"Error: Metal error no metal implementation for layer-norm"` (tracked upstream: [candle#2832](https://github.com/huggingface/candle/issues/2832))
- **Missing I64 strided affine** — `"Metal strided affine I64 not implemented"` — blocks int64 indexing
- **Matmul edge cases** — certain dimension combinations fail
- **Correctness bugs** — even when it runs, generation produces garbage output (observed on M2: ~0.03 tok/s and incorrect)

These are fundamental backend gaps, not configuration issues. Candle's Metal backend is still a work-in-progress; do not suggest it as a workaround.

---

## candle-coreml — Why It Doesn't Work

`candle-coreml` is a **third-party community crate** (`mazhewitt/candle-cormel`, ~4 GitHub stars). It is **not** maintained by HuggingFace.

Key differences from ONNX + CoreML EP:
- Requires models in **ANEMLL `.mlpackage` / `.mlmodelc` format** (not PyTorch safetensors)
- Models must be sourced from the `anemll` HuggingFace org — a completely separate ecosystem
- We tried it: loaded successfully but hit tensor dimension mismatch at generation time (`narrow invalid args`)
- The format incompatibility is a fundamental issue, not a configuration fix

Do not conflate `candle-coreml` with ONNX's CoreML execution provider. They are unrelated technologies.

---

## ONNX + CoreML Execution Provider — What It Actually Does

ONNX Runtime's [CoreML EP](https://onnxruntime.ai/docs/execution-providers/CoreML-ExecutionProvider.html) is Microsoft-maintained and calls into Apple's CoreML framework. On Apple Silicon it can dispatch to:
- **ANE** (Apple Neural Engine)
- **GPU** (via Metal internally, managed by CoreML)
- **CPU** (ARM)

The dispatch is **per-operation**, not whole-model. CoreML decides which device handles each op based on its supported op set.

**For LLM workloads specifically:** Many transformer ops (complex attention patterns, dynamic reshapes, KV cache operations) are not in CoreML's op set. The ONNX graph gets partitioned: unsupported ops run on CPU ARM, supported ops go to CoreML. In practice, the majority of LLM computation runs on CPU ARM.

**Practical implication:** The CoreML EP provides some acceleration for ops it supports, but the "ANE acceleration" framing overstates the benefit for full LLM workloads. On a MacBook Air with limited RAM, the bottleneck is primarily memory bandwidth, not compute — the difference between CoreML EP and CPU-only is modest.

**Configuration options** (via `execution_target` in config):
- `"coreml"` — use CoreML EP (best effort; partial ANE/GPU dispatch)
- `"cpu"` — force CPU ARM only

---

## LoRA Fine-Tuning: What Works Where

| Step | On macOS | On Linux |
|------|----------|----------|
| **Training** | MLX (`mlx-lm`, Python, Apple Silicon native) | PyTorch + PEFT (`transformers`, CUDA) |
| **Adapter format** | `.safetensors` (MLX output) | `.safetensors` (PEFT output) |
| **Convert to ONNX** | Olive toolchain (`olive finetune` needs CUDA for this step) | Olive toolchain |
| **Load at inference** | `onnxruntime-genai` Adapters API (`.onnx_adapter`) | `onnxruntime-genai` Adapters API |

**Key point:** ONNX Runtime itself has no training API. The training step is external. But `onnxruntime-genai` (the high-level LLM generate API built on top of onnxruntime) does support loading pre-trained LoRA adapters at inference time.

**Candle LoRA on macOS:** Candle does support in-process LoRA training in Rust, but Candle Metal has the same missing-op problems as inference. Candle CPU on macOS works but is too slow for practical training runs. Candle is not the LoRA path on macOS.

This is the core of **Issue #1**: wire up the MLX training pipeline and the adapter conversion + loading flow.

---

## Historical Context

Before ONNX, we tried two paths that both failed:

1. **Candle Metal** — the go-to option for macOS GPU, but missing ops for Qwen made it non-functional. Multiple attempts across several sessions all hit the same layer-norm / matmul gaps.

2. **candle-coreml (ANEMLL)** — appeared promising because it claimed ANE acceleration. But it requires models in a completely different format, and the tensor layout assumptions in `candle-coreml 0.3.1` didn't match the anemll Qwen3 model we tried.

ONNX + CoreML EP was the path that actually ran Qwen models successfully on macOS.

---

## References

- candle Metal tracking issue: https://github.com/huggingface/candle/issues/2832
- ONNX Runtime CoreML EP docs: https://onnxruntime.ai/docs/execution-providers/CoreML-ExecutionProvider.html
- onnxruntime-genai LoRA tutorial: https://onnxruntime.ai/docs/genai/tutorials/finetune.html
- candle-coreml crate: https://crates.io/crates/candle-coreml
- MLX (Apple Silicon LoRA training): https://github.com/ml-explore/mlx-lm
