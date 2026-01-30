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
- ✅ Added Candle dependencies
- ✅ Created models module structure
- ✅ Started Router model implementation

**Next Steps:**

1. **Fix Router Implementation**
   - Correct Candle API usage
   - Test forward pass
   - Test backward pass (online learning)
   - Model persistence (save/load)

2. **Implement Generator Model**
   - Transformer decoder architecture
   - Text generation (autoregressive)
   - Online learning from Claude responses
   - Model persistence

3. **Implement Validator Model**
   - Similar to Router (binary classifier)
   - Takes query + response as input
   - Online learning from quality assessments
   - Model persistence

4. **Tokenization**
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

1. **Candle API:** Need to learn correct API usage (compilation errors)
2. **Testing:** Need example data to test models
3. **Integration:** Need to wire up models to existing Phase 1 code

## Next Immediate Steps

1. Fix Candle API usage in Router model
2. Write unit tests for Router
3. Test forward pass with dummy data
4. Test online learning (backward pass + weight update)
5. Model save/load functionality

---

**Status:** Phase 2 implementation in progress
**Last Updated:** 2026-01-30
