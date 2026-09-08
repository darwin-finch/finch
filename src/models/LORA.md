# Deferred LoRA Placeholders and Retained Feedback

**Status:** Deferred. Automatic training and adapter loading are not supported Finch features.

## What works today

- `Ctrl+G` (good) / `Ctrl+B` (bad) records explicit feedback
- Feedback is written privately to `~/.finch/feedback.jsonl`
- Three weight tiers: **high (10x)**, **medium (3x)**, **normal (1x)**
- `LoRAConfig` and `LoRAAdapter` structs exist as infrastructure placeholders
- `train()` returns `anyhow::bail!("LoRA fine-tuning not yet implemented")`
- Existing `training_queue.jsonl` and adapter files are preserved but not processed

## What is not yet implemented

- Actual LoRA training
- Adapter saving to `~/.finch/adapters/`
- Adapter loading at ONNX inference time

## Why implementation is deferred

- Finch cannot yet apply an adapter in its ONNX inference path:
  [#1 (LoRA adapter loading at ONNX runtime)](https://github.com/darwin-finch/finch/issues/1).
- The investigated training options require unsupported external ML toolchains and a
  memory-bounded merge-and-reload design:
  [#7 (LoRA training memory efficiency)](https://github.com/darwin-finch/finch/issues/7).
- The compatible combinations of base models, runtimes, and adapter formats have not been
  verified:
  [#74 (refresh the local-model catalog and publish a runtime compatibility matrix)](https://github.com/darwin-finch/finch/issues/74).
- Automatic background Python training was removed until a supported path exists:
  [#139 (disable automatic Python LoRA training unless a supported native path is explicitly enabled)](https://github.com/darwin-finch/finch/issues/139).

The following notes record the investigated dependencies; they are not setup instructions for a
supported Finch feature.

**Training (external tool):**
- macOS: [MLX](https://github.com/ml-explore/mlx-lm) — community standard for LoRA on Apple Silicon
- Linux/CUDA: PyTorch + PEFT (`peft`, `transformers`)

**Inference (loading the adapter):**
- `onnxruntime-genai` supports `.onnx_adapter` files via its `Adapters` API
- MLX/PEFT adapters must be converted via the Olive toolchain first

## Key files

- `src/models/lora.rs` — `LoRAAdapter`, `LoRAConfig`, `WeightedExample`, `ExampleBuffer` (all placeholder)
- `src/training/batch_trainer.rs` — Returns fake loss; not wired to real training
