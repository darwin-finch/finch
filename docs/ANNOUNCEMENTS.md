# Shammah Announcement Posts

Ready-to-post announcements for sharing Shammah with the world.

---

## Hacker News - Show HN

**Title:** Show HN: Shammah – Local-first AI coding assistant that learns your style

**Post:**

```
I built Shammah, a local-first AI coding assistant that actually works offline.

Unlike cloud AI assistants, Shammah:
- Runs entirely on your machine (privacy-first)
- Works offline after initial setup
- Learns your coding patterns through weighted LoRA fine-tuning
- Costs $0 per query after setup
- Starts instantly (<100ms)

The key innovation is weighted feedback: you can mark critical mistakes with 10x weight so the model learns to avoid anti-patterns strongly, while good examples get normal weight.

It uses pre-trained models (Qwen/Llama/Mistral via ONNX) so it works well from day 1, then adapts to your specific needs over time. No months of training data collection required.

Also includes full tool execution (read files, run tests, search codebase) and runs as an OpenAI-compatible API server for multi-client use.

One-liner install:
curl -sSL https://raw.githubusercontent.com/darwin-finch/finch/main/install.sh | bash

GitHub: https://github.com/darwin-finch/finch
Release: https://github.com/darwin-finch/finch/releases/tag/v0.2.0

Built in Rust, works on macOS (Intel + Apple Silicon) and Linux.

Would love feedback from the HN community!
```

---

## Twitter/X Post

**Thread:**

```
🚀 Launching Shammah v0.2.0 - A local-first AI coding assistant that actually works offline

Unlike cloud AI:
✅ Runs on YOUR machine
✅ Works offline after setup
✅ Learns YOUR coding style
✅ $0 per query
✅ Privacy-first

🧵 1/5

The secret sauce: Weighted LoRA fine-tuning

Mark critical mistakes with 10x weight → model strongly learns to avoid them
Mark good examples with 1x weight → model remembers patterns

Your AI assistant that actually learns from YOUR feedback.

2/5

Tech stack:
• Pre-trained models (Qwen/Llama/Mistral) via ONNX
• Works great day 1, improves over time
• Metal acceleration on Apple Silicon
• Full tool execution (read/write files, run tests)
• OpenAI-compatible API server

3/5

Installation is trivial:

curl -sSL https://raw.githubusercontent.com/darwin-finch/finch/main/install.sh | bash

30 seconds from zero to working AI assistant.

Built in Rust. Runs on macOS + Linux.

4/5

📦 Download: github.com/darwin-finch/finch/releases/tag/v0.2.0
📖 Docs: github.com/darwin-finch/finch
🛡️ License: MIT OR Apache-2.0

For developers who value privacy and local-first tools.

Try it and let me know what you think!

5/5
```

**Short version (single tweet):**

```
🚀 Shammah v0.2.0: Local-first AI coding assistant that learns your style

✅ Works offline
✅ Learns from weighted feedback
✅ $0 per query
✅ Privacy-first

One-liner install:
curl -sSL https://raw.githubusercontent.com/darwin-finch/finch/main/install.sh | bash

github.com/darwin-finch/finch
```

---

## Reddit - /r/LocalLLaMA

**Title:** [Release] Shammah v0.2.0 - Local-first AI coding assistant with weighted LoRA fine-tuning

**Post:**

```markdown
Hey r/LocalLLaMA!

I just released Shammah v0.2.0 - a local-first AI coding assistant that I think you'll find interesting.

## What is it?

Shammah runs pre-trained models (Qwen, Llama, Mistral, Phi) locally via ONNX Runtime, then continuously improves through weighted LoRA fine-tuning based on your feedback.

## Why it's different

**Weighted feedback system:**
- 🔴 High-weight (10x): "Never use .unwrap() in production" → model strongly avoids this
- 🟡 Medium-weight (3x): "Prefer iterator chains over loops" → model learns your style
- 🟢 Normal-weight (1x): "This is good, remember this" → reinforces patterns

This means you can prioritize critical feedback and the model actually learns YOUR anti-patterns, not just generic ones.

## Key Features

- **Instant startup**: REPL ready in <100ms, model loads in background
- **Adaptive sizing**: Auto-selects model based on RAM (1.5B/3B/7B/14B)
- **Hardware acceleration**: Metal (Apple Silicon), CUDA, or CPU
- **Full tool execution**: Read files, run commands, search codebase
- **OpenAI-compatible API**: Run as daemon, drop-in replacement
- **Privacy-first**: Everything runs locally, code never leaves your machine

## Installation

One-liner:
```bash
curl -sSL https://raw.githubusercontent.com/darwin-finch/finch/main/install.sh | bash
```

Or download binaries:
- [macOS ARM64](https://github.com/darwin-finch/finch/releases/latest/download/finch-macos-aarch64.tar.gz)
- [macOS x86_64](https://github.com/darwin-finch/finch/releases/latest/download/finch-macos-x86_64.tar.gz)
- [Linux x86_64](https://github.com/darwin-finch/finch/releases/latest/download/finch-linux-x86_64.tar.gz)

## Quick Start

```bash
./finch setup  # Interactive wizard
./finch        # Start REPL

> How do I implement a binary search tree in Rust?
# Works immediately, model downloads in background
# Future queries use fast local model
```

## Tech Details

- Built in Rust
- ONNX Runtime with CoreML/Metal/CUDA execution providers
- KV cache for efficient generation
- LoRA adapters via PyTorch + PEFT
- HTTP daemon mode with session management

## Links

- **GitHub**: https://github.com/darwin-finch/finch
- **Release**: https://github.com/darwin-finch/finch/releases/tag/v0.2.0
- **Docs**: See README

Would love to hear your thoughts! What models are you most interested in seeing supported?

---

**System Requirements:**
- 8GB+ RAM (16GB+ recommended)
- 2-15GB disk space for models
- macOS, Linux, or Windows
- HuggingFace token (free) for model downloads
```

---

## Reddit - /r/rust

**Title:** [Project] Shammah - Local-first AI coding assistant built in Rust

**Post:**

```markdown
Hey Rustaceans!

I built Shammah, a local-first AI coding assistant in Rust that I wanted to share.

## Why Rust?

- **Memory safety**: Handles large models without leaks
- **Performance**: ONNX Runtime integration with zero-copy tensors
- **Async**: Tokio for daemon mode with concurrent sessions
- **Cross-platform**: Works on macOS, Linux, Windows
- **Apple Silicon**: Native Metal acceleration via CoreML

## Architecture

```
src/
├── models/          # ONNX Runtime integration, LoRA
├── generators/      # Text generation with KV cache
├── tools/           # Tool execution system
├── server/          # HTTP daemon (Axum)
├── cli/             # TUI (Ratatui)
└── providers/       # Multi-provider support
```

**Key tech:**
- `ort` - ONNX Runtime bindings
- `hf-hub` - HuggingFace model downloads
- `tokio` - Async runtime
- `axum` - HTTP server
- `ratatui` - Terminal UI
- `serde` - Config + API serialization

## What it does

- Runs pre-trained models locally (Qwen, Llama, Mistral via ONNX)
- Learns your coding style through weighted LoRA fine-tuning
- Full tool execution (read files, run commands, search)
- OpenAI-compatible API server
- Professional TUI with scrollback, multi-line input, live status

## Installation

```bash
curl -sSL https://raw.githubusercontent.com/darwin-finch/finch/main/install.sh | bash
```

Or build from source:
```bash
git clone https://github.com/darwin-finch/finch
cd finch
cargo build --release
```

## GitHub

https://github.com/darwin-finch/finch

Built with:
- Rust 1.70+
- ~50k lines of code
- CI/CD via GitHub Actions
- Multi-platform releases

Feedback welcome!
```

---

## Dev.to Article (Draft Outline)

**Title:** Building a Privacy-First AI Coding Assistant in Rust

**Outline:**

1. **Introduction**
   - Problem: Cloud AI assistants lack privacy, cost money, can't learn
   - Solution: Local-first with continuous learning

2. **Architecture Overview**
   - ONNX Runtime integration
   - Progressive bootstrap pattern
   - LoRA fine-tuning pipeline

3. **Key Technical Challenges**
   - KV cache for efficient generation
   - Async model loading without blocking
   - TUI rendering with scrollback
   - Multi-platform binary releases

4. **Weighted Learning System**
   - Why weighted feedback matters
   - Implementation details
   - Training pipeline

5. **Tool Execution**
   - Security model
   - Pattern-based approvals
   - Client-side execution in daemon mode

6. **Results**
   - Performance metrics
   - Real-world usage
   - What worked, what didn't

7. **Open Source**
   - GitHub link
   - How to contribute
   - Future roadmap

---

## Discord/Slack Message

```
🚀 Just released Shammah v0.2.0!

A local-first AI coding assistant that:
• Works completely offline
• Learns YOUR coding style
• Costs $0 per query
• Privacy-first (code stays on your machine)

One-liner install:
curl -sSL https://raw.githubusercontent.com/darwin-finch/finch/main/install.sh | bash

Check it out: https://github.com/darwin-finch/finch

Built in Rust, runs on macOS + Linux 🦀
```

---

## Usage Instructions

1. **Copy the appropriate post** for each platform
2. **Customize** if needed (add your personal voice)
3. **Post at optimal times**:
   - HN: Weekday mornings (9-11am ET)
   - Reddit: Weekday afternoons (2-5pm ET)
   - Twitter: Multiple times, spread throughout day
4. **Engage** with comments and questions
5. **Be authentic** - you built something cool, be proud!

---

**Ready to share Shammah with the world!** 🌍
