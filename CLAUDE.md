# CLAUDE.md - AI Assistant Context

This document provides context for AI assistants (like Claude Code) working on the Shammah project.

## Project Context

**Project Name**: Shammah (שָׁמָה - "watchman/guardian")
**Purpose**: Local-first Constitutional AI proxy
**Core Innovation**: Learns to handle 95% of requests locally while maintaining Claude API compatibility

### The Problem

Users of Claude API face:
- High API costs for repetitive or simple queries
- Privacy concerns sending all data to cloud
- Network latency for every request
- Inability to work offline
- No learning/improvement over time

### The Solution

Shammah acts as an intelligent proxy that:
1. Initially forwards 100% of requests to Claude API
2. Learns from every Claude response (patterns, reasoning, style)
3. Gradually handles more requests locally using trained models
4. Reaches steady state of ~5% forwarding (only novel/complex queries)
5. Reduces costs by 76% while maintaining response quality

### Key Metrics

- **Target**: 95% local processing, 5% API forwarding (steady state)
- **Timeline**: ~6 months from 100% → 5% forwarding
- **Cost Reduction**: 76% (accounting for training costs)
- **Quality**: Maintain Claude-level responses through Constitutional AI

## Architecture Overview

### High-Level Design: 3-Model Ensemble

Shammah uses **three specialized neural networks** trained on your actual Claude usage:

```
User Request
    ↓
┌─────────────────────────────────────┐
│ [1] Router Model (Small, ~1-3B)     │  Pre-generation decision
│     "Can we handle this locally?"   │  Based on query features
│     Confidence score: 0.0 - 1.0     │  <50ms inference
└─────────────────────────────────────┘
    ↓
Confidence > threshold?
    │
    ├─ NO → Forward to Claude API (log for training)
    │
    └─ YES (try locally)
         ↓
    ┌─────────────────────────────────────┐
    │ [2] Generator Model (Medium, ~7-13B)│  Produces response
    │     Generates Claude-style response  │  Trained via distillation
    │     Mimics Claude's patterns         │  ~500ms-2s inference
    └─────────────────────────────────────┘
         ↓
    ┌─────────────────────────────────────┐
    │ [3] Validator Model (Small, ~1-3B)  │  Post-generation quality gate
    │     "Is this response good enough?"  │  Catches generator errors
    │     Detects hallucinations/mistakes  │  <100ms inference
    └─────────────────────────────────────┘
         ↓
    Response passes?
         ├─ YES → Return to user
         └─ NO → Forward to Claude (generator made mistake)
```

**Key Insight:** All three models are **trained from scratch** on YOUR Claude usage data. They learn your specific query patterns and Claude's response style for your domain.

### The Three Models Explained

**1. Router Model (Classifier)**
- **Purpose:** Quick pre-generation decision: "Should we try locally?"
- **Input:** Query text + context features
- **Output:** Confidence score (0.0 = must forward, 1.0 = can handle)
- **Training:** Learns which queries had low divergence (handled well locally)
- **Size:** 1-3B parameters for speed
- **Runs on:** Apple Neural Engine (ultra-fast)

**2. Generator Model (Response Producer)**
- **Purpose:** Generate actual response mimicking Claude
- **Input:** Query text + context
- **Output:** Full response text
- **Training:** Distillation from Claude's responses (learn query → response mapping)
- **Size:** 7-13B parameters for quality
- **Runs on:** Apple GPU (or Neural Engine with quantization)

**3. Validator Model (Quality Gate)**
- **Purpose:** Detect generator errors before returning to user
- **Input:** Query + generated response
- **Output:** Quality score + error flags (hallucination, off-topic, incoherent)
- **Training:** Learns to detect divergence from Claude's quality
- **Size:** 1-3B parameters for speed
- **Runs on:** Apple Neural Engine

### Why Three Models?

**Efficiency:** Router is tiny and fast - can reject queries in <50ms without running expensive generator

**Accuracy:** Generator specializes in response quality, validator catches its mistakes

**Safety:** Two decision points (router + validator) prevent bad local responses from reaching users

### Core Components

1. **Router** (`src/router/`)
   - Phase 1: Pattern matching (placeholder)
   - Phase 2+: Neural network classifier
   - Tracks routing decisions and accuracy

2. **Generator** (`src/models/generator/`)
   - Phase 1: Template responses (placeholder)
   - Phase 2+: Custom LLM trained on Claude responses
   - Learns your specific usage patterns

3. **Validator** (`src/models/validator/`)
   - Phase 1: Crisis detection (partial implementation)
   - Phase 2+: Quality assessment model
   - Detects errors before returning to user

4. **Claude Client** (`src/claude/`)
   - HTTP client for Claude API
   - Logs all (query, response) pairs for training
   - Handles streaming and retries

5. **Learning Engine** (`src/learning/`)
   - Phase 2+: Processes logged data into training sets
   - Trains all three models
   - Retrains models periodically
   - Tracks performance metrics

5. **Configuration** (`src/config/`)
   - Reads `~/.claude/settings.json` (Claude Code integration)
   - Manages `~/.claude-proxy/` storage
   - Environment variables and CLI args

### Technology Stack

- **Language**: Rust (memory safety, performance, Apple Silicon optimization)
- **ML Framework**:
  - Phase 2: PyTorch/Candle for training custom models
  - Phase 4: CoreML for inference (Apple Neural Engine)
  - ONNX for cross-platform model format
- **Models** (all trained from scratch on your data):
  - Router: ~1-3B parameters (binary classifier)
  - Generator: ~7-13B parameters (text generation)
  - Validator: ~1-3B parameters (quality assessment)
- **Training**: Distillation from Claude's responses
- **API**: Compatible with Claude API format
- **Storage**: `~/.shammah/` for all data
- **Async**: Tokio runtime
- **HTTP**: Reqwest client
- **CLI**: Clap for argument parsing

## Key Design Decisions

### 1. Claude Code Compatibility

**Decision**: Use `~/.claude/settings.json` for configuration
**Rationale**: Seamless integration with Claude Code CLI tool
**Implication**: Must respect Claude Code's config format and behavior

### 2. Storage Location

**Decision**: Store everything in `~/.shammah/`
**Rationale**:
- Simple, single directory for all Shammah data
- Traditional Unix convention (dot-directory in home)
- Clear separation from Claude Code
- User can easily find/delete data

**Structure**:
```
~/.shammah/
├── config.toml              # API key and settings
├── metrics/                 # Daily JSONL logs for training
│   ├── 2026-01-29.jsonl
│   ├── 2026-01-30.jsonl
│   └── ...
└── models/                  # Phase 2+: trained models
    ├── router.onnx
    ├── generator.onnx
    └── validator.onnx
```

### 3. Command Name

**Decision**: Use `shammah` as the binary name
**Rationale**:
- Distinct from `claude` command
- Memorable and meaningful (Hebrew "watchman")
- Easy to type

### 4. Three Operating Modes

**Interactive REPL**:
```bash
shammah
> How do I use lifetimes in Rust?
```

**Daemon Mode** (background service):
```bash
shammah daemon
# Runs HTTP server on localhost:8000
# Claude Code connects via proxy settings
```

**Single Query**:
```bash
shammah query "What is the time complexity of quicksort?"
```

### 5. Learning Strategy

**Decision**: Continuous learning from all forwarded requests
**Rationale**: Every API call is a training opportunity

**Process**:
1. Forward request to Claude
2. Receive response
3. Log (request, response, metadata) to training set
4. Periodically retrain models
5. Update router confidence thresholds

### 6. Training Strategy: Distillation from Claude

**Decision**: Train all models from scratch using Claude as the teacher

**How It Works:**
1. Forward queries to Claude, log (query, response) pairs
2. Collect 1000+ examples of your actual usage
3. Train Router: "Which queries had low divergence when handled locally?"
4. Train Generator: "Given query X, what would Claude say?" (distillation)
5. Train Validator: "Is this response as good as Claude's?"
6. Deploy models and continue learning from mistakes

**Why Distillation:**
- Models learn YOUR specific query patterns
- Generator inherits Claude's quality and safety properties
- No need for pre-trained models
- Personalized to your domain/usage

**Data Requirements:**
- Phase 1: Collect 1000+ query/response pairs
- Phase 2: Train initial models
- Ongoing: Continuous learning from forwards

### 7. Constitutional AI (Quality & Safety)

**Decision**: Validator ensures local responses meet constitutional principles

**Principles**:
- **Helpful**: Response must address the query
- **Harmless**: No harmful, illegal, or unethical content
- **Honest**: Acknowledge uncertainty, don't make things up
- **Consistent**: Style matches Claude's tone

**Implementation:**
- Validator model learns these from Claude's examples
- Two decision points (Router + Validator) prevent bad responses
- If either model is uncertain → forward to Claude

## Development Guidelines

### Code Style

- **Formatting**: Always use `cargo fmt` before committing
- **Linting**: Run `cargo clippy` and address warnings
- **Documentation**: Doc comments for all public items
- **Error Messages**: User-friendly, actionable error messages

### Error Handling

- Use `anyhow::Result` for application code
- Use `thiserror` for library-style errors with custom types
- Always provide context with `.context()` or `.with_context()`
- Never use `.unwrap()` or `.expect()` in production code paths

Example:
```rust
use anyhow::{Context, Result};

fn load_config() -> Result<Config> {
    let path = config_path()
        .context("Failed to determine config path")?;

    let contents = fs::read_to_string(&path)
        .with_context(|| format!("Failed to read config from {}", path.display()))?;

    toml::from_str(&contents)
        .context("Failed to parse config TOML")
}
```

### Testing

- **Unit tests**: Test individual functions in module files
- **Integration tests**: Test cross-module behavior in `tests/`
- **Property tests**: Use `proptest` for complex logic
- **Mocking**: Use `mockito` for external API calls

Minimum coverage: 80% for new code

### Logging

Use `tracing` for structured logging:

```rust
use tracing::{debug, info, warn, error, instrument};

#[instrument]
async fn forward_request(req: Request) -> Result<Response> {
    info!("Forwarding request to Claude API");
    debug!(?req, "Request details");

    let response = claude_client.send(req).await
        .context("Failed to forward request")?;

    info!(status = %response.status, "Received response");
    Ok(response)
}
```

### Git Workflow for AI Assistants

When working on this project, create commits after completing each logical unit of work. **Each commit should include all related changes: code, tests, documentation, and design updates.**

#### When to Commit
- After implementing a complete feature
- After fixing a bug
- After adding/updating documentation
- After refactoring that maintains functionality
- Do NOT commit: work-in-progress, broken code, or experimental changes

#### What to Include in Each Commit
A complete commit includes:
1. **Code changes** (src/, examples/, tests/)
2. **Test updates** if behavior changed
3. **Documentation updates**:
   - README.md if user-facing changes
   - CLAUDE.md if AI assistant context changed
   - docs/ARCHITECTURE.md if design changed
   - Code comments/docstrings for new public APIs
4. **Design document updates** if implementation differs from spec
5. **Examples** if new functionality added

#### Commit Message Format
Use conventional commits with high-level descriptions:
- **feat**: new functionality (e.g., "feat: add threshold-based router for immediate learning")
- **fix**: bug fixes (e.g., "fix: handle empty tokenizer vocabulary with character fallback")
- **docs**: documentation only (e.g., "docs: update architecture with Phase 2 design")
- **test**: adding/updating tests (e.g., "test: add training verification tests")
- **refactor**: code changes without behavior change (e.g., "refactor: extract routing logic into hybrid module")

#### Message Content
- **Subject**: what was done (imperative mood, <70 chars)
- **Body** (optional): why it was done, what problem it solves, what docs updated
- Keep it high-level - avoid implementation details

#### Example Complete Commit
```bash
# After implementing threshold router:
git add \
  src/models/threshold_router.rs \
  src/models/threshold_validator.rs \
  src/models/mod.rs \
  examples/threshold_demo.rs \
  docs/ARCHITECTURE.md \
  CLAUDE.md

git commit -m "feat: add threshold-based router and validator

Implements statistics-driven routing that learns from query 1, providing
immediate value during cold start period before neural networks trained.

Changes:
- New threshold_router.rs: query categorization + success tracking
- New threshold_validator.rs: heuristic quality validation
- Updated ARCHITECTURE.md: added Phase 2a (threshold approach)
- Updated CLAUDE.md: documented hybrid strategy
- Added threshold_demo.rs: demonstrates immediate learning"
```

#### Verification Before Commit
- Run `cargo test` - all tests pass
- Run `cargo fmt` - code formatted
- Run `cargo clippy` - no warnings
- Verify documentation reflects current implementation
- Check that examples still work

### No Code in CLAUDE.md

**IMPORTANT**: This file (CLAUDE.md) should contain context, architecture, and guidelines, but NOT implementation code. Code belongs in `src/`, `tests/`, or `examples/`.

This document is for helping AI assistants understand the project - it's a "map" not the "territory."

## Current Project Phase

**Phase**: Phase 2a - Threshold Models ✅
**Status**: Threshold-based router and validator implemented, learning from query 1
**Next Steps**:
- Deploy threshold models in production to collect data
- Train neural networks once sufficient data collected (500+ queries)
- Transition to hybrid approach (threshold + neural)

### What's Done (Phase 1)

- ✅ **Router (placeholder):** Pattern matching with TF-IDF demonstrates routing concept
- ✅ **Generator (placeholder):** Template responses for 10 patterns demonstrate local processing
- ✅ **Validator (partial):** Crisis detection demonstrates safety checking
- ✅ **Claude API Client:** Full integration with retry logic
- ✅ **Metrics Logger:** Collects (query, response, routing) data for Phase 2 training
- ✅ **CLI/REPL:** Interactive interface with commands
- ✅ **Tests:** 14/14 passing (9 unit + 5 integration)

**Current Performance:**
- 20-30% "local" rate (templates, not real models yet)
- 100% crisis detection
- Ready to collect real training data

### What's Done (Phase 2a - Threshold Models)

- ✅ **ThresholdRouter:** Statistics-based routing using query categorization
  - Tracks success rates per category (Greeting, Definition, HowTo, etc.)
  - Adaptive confidence thresholds (starts 0.95, adjusts based on performance)
  - Learns from query 1 (no cold start period)
  - Fully interpretable decisions
- ✅ **ThresholdValidator:** Rule-based quality validation using heuristics
  - 8 quality signals (TooShort, Repetitive, AnswersQuestion, etc.)
  - Learns signal correlations over time
  - Conservative at start (forces Claude learning for first 10 queries)
- ✅ **HybridRouter:** Smooth transition from threshold to neural
  - Phase 1 (queries 1-50): Pure threshold-based
  - Phase 2 (queries 51-200): Hybrid with gradually increasing neural weight
  - Phase 3 (queries 201+): Primarily neural with threshold safety checks
- ✅ **Neural Network Models:** Basic implementations (Router, Generator, Validator)
  - Candle-based training with SGD optimizer
  - Online learning after each query
  - Tests verify loss reduction and pattern learning
- ✅ **Examples:** threshold_demo.rs, hybrid_demo.rs demonstrate immediate learning

**Key Innovation:**
The threshold models provide **immediate value** from query 1, unlike neural networks that need 200+ queries for cold start. This hybrid approach combines interpretability and instant learning (threshold) with adaptive power (neural).

### Apple Silicon Optimization ✅

- ✅ **Metal Backend Support:** Automatic GPU acceleration on M1/M2/M3/M4 Macs
  - `DevicePreference::Auto` - tries Metal, falls back to CPU
  - `DevicePreference::Metal` - forces Metal (errors if unavailable)
  - `DevicePreference::Cpu` - forces CPU (for debugging)
- ✅ **Device Detection:** Automatic detection with informative logging
- ✅ **Performance Monitoring:** `metal_benchmark` example shows 10-100x speedup
- ✅ **Intelligent Fallback:** Gracefully handles non-Apple Silicon Macs

**Expected Performance:**
- Small models (128 hidden dim): 2-5x speedup on Metal
- Large models (768+ hidden dim): 10-100x speedup on Metal
- Best performance on M1 Pro/Max, M2 Pro/Max, M3/M4

### What's NOT Done Yet (Phase 2b+)

- ❌ No production deployment yet
- ❌ Neural networks not trained on real data (random weights)
- ❌ Generator model needs actual LLM (currently placeholder)
- ❌ No uncertainty estimation
- ❌ No Core ML export (.mlmodel format for maximum Apple Silicon optimization)

## Working with This Project

### For AI Assistants (Claude Code, etc.)

When working on this project:

1. **Read the spec first**: `CONSTITUTIONAL_PROXY_SPEC.md` is the authoritative design document
2. **Check current phase**: Don't implement Phase 3 features when we're in Phase 1
3. **Follow Rust conventions**: Prefer standard patterns over clever tricks
4. **Think about Apple Silicon**: This targets M1/M2/M3/M4 Macs specifically
5. **Test as you go**: Write tests alongside implementation code
6. **Document trade-offs**: When you make a design choice, explain why in code comments

### Common Tasks

**Adding a new module**:
1. Create file in `src/` (e.g., `src/metrics.rs`)
2. Add `pub mod metrics;` to `src/lib.rs`
3. Implement with proper error handling and tests
4. Add documentation to relevant `docs/` files

**Implementing a feature**:
1. Read the spec section for that feature
2. Design the API (functions, structs, traits)
3. Write tests first (TDD)
4. Implement the feature
5. Run `cargo test`, `cargo clippy`, `cargo fmt`
6. Update documentation

**Debugging an issue**:
1. Check logs (structured with `tracing`)
2. Add more instrumentation if needed
3. Write a failing test that reproduces the issue
4. Fix the issue
5. Verify test passes

## Reference Documents

- **CONSTITUTIONAL_PROXY_SPEC.md**: Complete technical specification (authoritative)
- **README.md**: User-facing documentation
- **docs/ARCHITECTURE.md**: Detailed architecture breakdown
- **docs/CONFIGURATION.md**: Configuration options and setup
- **docs/DEVELOPMENT.md**: Development workflow and guidelines

## Questions?

If you're unsure about something:

1. Check `CONSTITUTIONAL_PROXY_SPEC.md` first
2. Check this file (CLAUDE.md) for context
3. Look at existing code for patterns
4. Ask the user if still unclear

## Key Principles

1. **Local-first**: Prioritize privacy and performance
2. **Gradual improvement**: Learn continuously, don't expect perfection
3. **Constitutional AI**: Maintain safety and quality at all times
4. **User experience**: Fast, reliable, transparent
5. **Rust best practices**: Safe, idiomatic, performant code

---

This document should evolve as the project grows. Keep it updated with new design decisions and context that helps AI assistants work effectively on Shammah.
