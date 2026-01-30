# Phase 2: Online Learning - Implementation Started

## Summary

Phase 2 implementation has begun. We're building the 3-model ensemble with online learning (models update after each Claude forward).

## Architecture Clarified

### The 3 Models (All Binary Decisions + Online Learning)

**1. Router Model**
- Input: Query text (tokenized)
- Output: **Binary (0 or 1)** - forward or try local
- Architecture: Small transformer encoder (~500M params)
- Updates: After each forward (learn from success/failure)

**2. Generator Model**
- Input: Query text (tokenized)
- Output: **Response text** (generated tokens)
- Architecture: Medium transformer decoder (~7B params)
- Updates: After each forward (distillation from Claude)

**3. Validator Model**
- Input: Query + Generated response (tokenized)
- Output: **Binary (0 or 1)** - bad or good
- Architecture: Small transformer encoder (~500M params)
- Updates: After each forward (learn from quality assessment)

## Implementation Plan

### ✅ Phase 1 Complete
- Basic proxy infrastructure
- Claude API client
- Metrics logging
- Template system (placeholder for Phase 2)

### 🚧 Phase 2 In Progress

**Done:**
- ✅ Decided on Candle (Rust ML framework)
- ✅ Added Candle dependencies (candle-core, candle-nn, candle-transformers, tokenizers)
- ✅ Created models module structure
- ✅ Implemented Router model (binary classifier, compiles successfully)
- ✅ Implemented Generator model (autoregressive decoder, compiles successfully)
- ✅ Implemented Validator model (binary classifier, compiles successfully)
- ✅ Fixed all Candle API compilation errors
- ✅ Created training framework design (docs/TRAINING_FRAMEWORK.md)
- ✅ All three models include:
  - Forward pass implementations
  - Binary decision/generation methods
  - Online learning update methods (placeholders)
  - Model persistence (save/load) interfaces
  - Unit tests

**Next Steps:**

1. **Implement Proper Autograd/Backward Pass**
   - Candle doesn't have built-in autograd like PyTorch
   - Need to implement gradient computation manually or use alternative approach
   - Consider using Candle's VarMap tracking for parameter updates
   - Implement SGD/Adam optimizer integration

2. **Tokenization**
   - Use `tokenizers` crate
   - BPE or WordPiece tokenizer
   - Vocab size: 50k tokens

5. **Online Learning Loop**
   - After each Claude forward:
     - Update Router (was decision correct?)
     - Update Generator (learn from Claude's response)
     - Update Validator (was quality assessment correct?)
   - Save updated models

6. **Integration**
   - Replace Phase 1 templates with real models
   - Wire up online learning
   - Model initialization (random weights)
   - Progressive improvement over time

## Key Decisions Made

### Pure Rust + Candle
- No Python dependencies
- Candle provides tensor ops, autograd, optimizers
- We implement custom model architectures
- Fast, native performance

### Binary Decisions (No Thresholds)
- Router outputs 0 or 1 directly (not confidence + threshold)
- Validator outputs 0 or 1 directly (not quality score)
- Simpler, models learn appropriate conservatism via training

### Random Initialization
- Models start with random/zero weights
- No pre-trained models to download
- Each user trains on their own data
- Fully personalized

### Online Learning (Not Batch Training)
- Update weights after EVERY forward to Claude
- No separate training phase
- Continuous improvement
- Day 1: random weights, forward 100%
- Day 30: decent models, forward 60%
- Day 180: good models, forward 5%

## File Structure

```
src/models/
├── mod.rs              # Module exports
├── common.rs           # Shared utilities, config, device selection
├── router.rs           # Router model (binary classifier)
├── generator.rs        # Generator model (text generation)
└── validator.rs        # Validator model (binary classifier)
```

## Technical Stack

- **Framework:** Candle (Rust ML)
- **Models:** Custom transformers (implemented by us)
- **Optimization:** SGD/Adam with online updates
- **Tokenization:** BPE via `tokenizers` crate
- **Device:** Metal (Apple Silicon) or CPU
- **Persistence:** Save/load weights to `~/.shammah/models/`

## Expected Timeline

- **Week 1:** Router model working + tests
- **Week 2:** Generator model working + tests
- **Week 3:** Validator model working + tests
- **Week 4:** Integration, online learning loop, testing
- **Week 5-6:** Bug fixes, optimization, documentation

## Current Blockers

1. **Gradient Computation:** Candle doesn't have PyTorch-style autograd - need to implement backward passes manually or find alternative approach
2. **Tokenization:** Need to integrate the `tokenizers` crate for BPE tokenization
3. **Testing:** Need real data to test models and verify training works
4. **Integration:** Need to wire up models to existing Phase 1 code

## Next Immediate Steps

1. Implement tokenization system using `tokenizers` crate
2. Implement proper gradient computation or find Candle-compatible training approach
3. Test forward passes with real tokenized data
4. Implement complete training loop as specified in docs/TRAINING_FRAMEWORK.md
5. Wire up models to Phase 1 router infrastructure

---

**Status:** Phase 2 models implemented and compiling successfully. Next: tokenization and training loop.
**Last Updated:** 2026-01-29
