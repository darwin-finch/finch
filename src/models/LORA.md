# LoRA Fine-Tuning Infrastructure

**Status: Feedback collection works. Training not yet implemented.**

## What works today

- `Ctrl+G` (good) / `Ctrl+B` (bad) on any response records a weighted example
- Examples written to `~/.finch/training_queue.jsonl` as JSONL
- Three weight tiers: **high (10x)**, **medium (3x)**, **normal (1x)**
- `LoRAConfig` and `LoRAAdapter` structs exist as infrastructure placeholders
- `train()` returns `anyhow::bail!("LoRA fine-tuning not yet implemented")` — tracked as **Issue #1** (40-80h)

## What is not yet implemented

- Actual LoRA training
- Adapter saving to `~/.finch/adapters/`
- Adapter loading at ONNX inference time

## Planned training pipeline (Issue #1)

**Training (external tool):**
- macOS: [MLX](https://github.com/ml-explore/mlx-lm) — community standard for LoRA on Apple Silicon
- Linux/CUDA: PyTorch + PEFT (`peft`, `transformers`)

**Inference (loading the adapter):**
- `onnxruntime-genai` supports `.onnx_adapter` files via its `Adapters` API
- MLX/PEFT adapters must be converted via the Olive toolchain first

## Key files

- `src/models/lora.rs` — `LoRAAdapter`, `LoRAConfig`, `WeightedExample`, `ExampleBuffer` (all placeholder)
- `src/training/batch_trainer.rs` — Returns fake loss; not wired to real training
