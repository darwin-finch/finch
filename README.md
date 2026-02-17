# Shammah

<div align="center">

**שָׁמָה** (Shammah) - Hebrew: "watchman" or "guardian"

[![CI](https://github.com/schancel/shammah/workflows/CI/badge.svg)](https://github.com/schancel/shammah/actions)
[![Release](https://img.shields.io/github/v/release/schancel/shammah)](https://github.com/schancel/shammah/releases)
[![License](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue.svg)](LICENSE)

**A local-first AI coding assistant that actually works offline.**

[Quick Start](#quick-start) •
[Download](#installation) •
[Features](#key-features) •
[Documentation](#documentation)

</div>

---

## Why Shammah?

**Problem:** Cloud AI assistants require constant internet, cost money per query, and can't learn your specific coding patterns.

**Solution:** Shammah runs entirely on your machine with:
- ✅ **Works offline** after initial setup
- ✅ **Instant responses** from local models
- ✅ **Learns your style** through weighted LoRA fine-tuning
- ✅ **Privacy-first** - your code never leaves your machine
- ✅ **Free to run** - no per-query costs

Unlike training a model from scratch (months + expensive GPUs), Shammah uses pre-trained models that work great immediately and adapt to your needs over time.

## Quick Start

### Installation

**Option 1: One-Liner Install** (Easiest)

```bash
curl -sSL https://raw.githubusercontent.com/schancel/shammah/main/install.sh | bash
```

This will:
- Detect your platform automatically
- Download the latest release
- Install to `~/.local/bin/shammah`
- Verify the installation

**Option 2: Download Pre-Built Binary**

```bash
# macOS (Apple Silicon)
curl -L https://github.com/schancel/shammah/releases/latest/download/shammah-macos-aarch64.tar.gz | tar xz
./shammah --version

# macOS (Intel)
curl -L https://github.com/schancel/shammah/releases/latest/download/shammah-macos-x86_64.tar.gz | tar xz
./shammah --version

# Linux
curl -L https://github.com/schancel/shammah/releases/latest/download/shammah-linux-x86_64.tar.gz | tar xz
./shammah --version
```

**Option 3: Build from Source**

```bash
git clone https://github.com/schancel/shammah
cd shammah
cargo build --release
./target/release/shammah --version
```

### First Run (30 seconds to working AI)

```bash
# 1. Run setup wizard (interactive)
./shammah setup

# Enter:
# - Your Claude API key (from console.anthropic.com) - for fallback
# - Your HuggingFace token (from huggingface.co/settings/tokens) - for model downloads
# - Choose model size (auto-selected based on your RAM)

# 2. Start using it!
./shammah

# REPL appears instantly - you can start asking questions right away
> How do I implement a binary search tree in Rust?

# First time: Model downloads in background (1-14GB depending on RAM)
# You get Claude responses while model loads
# Once ready: Future queries use fast local model

> Explain Rust lifetimes
# Now using local Qwen model! ⚡
```

That's it! 🎉

## Key Features

### 🚀 Instant Quality - Pre-trained Local Models

Works from day 1 - no training period required.

- **Multiple model support** - Qwen, Llama, Mistral, Phi via ONNX
- **Adaptive sizing** - Auto-selects based on your RAM:
  - 8GB → 1.5B model (fast, 500ms responses)
  - 16GB → 3B model (balanced)
  - 32GB → 7B model (powerful)
  - 64GB+ → 14B model (maximum capability)
- **Instant startup** - REPL ready in <100ms
- **Hardware acceleration** - Uses Metal (Apple Silicon), CUDA, or CPU
- **Offline capable** - No internet needed after first download

### 📈 Continuous Improvement - Weighted LoRA Fine-Tuning

Model adapts to YOUR coding style and patterns.

**How it works:**
```bash
> /feedback high
This is a critical error - never use .unwrap() in production.
Always handle errors properly.

# This feedback has 10x impact on future responses
# Model learns to avoid this pattern strongly
```

**Three feedback levels:**
- 🔴 **High (10x)**: Critical errors, anti-patterns, security issues
- 🟡 **Medium (3x)**: Style preferences, better approaches
- 🟢 **Normal (1x)**: Good examples to remember

**Benefits:**
- Specializes to your frameworks and libraries
- Remembers your architectural preferences
- Learns from mistakes without degrading base quality
- Efficient - trains only 0.1-1% of parameters

### 🛠️ Full Tool Execution

AI can inspect and modify your code:

```bash
> Read my Cargo.toml and suggest dependency updates
🔧 Tool: Read (approved)
   File: Cargo.toml
   ✓ Success

> Find all TODO comments in Rust files
🔧 Tool: Glob (approved)
   Pattern: **/*.rs
   Found: 15 files
🔧 Tool: Grep (approved)
   Pattern: TODO.*
   23 matches found

> Run the test suite
🔧 Tool: Bash (requires confirmation)
   Command: cargo test
   Approve? [y/N/always]: y
   ✓ All tests passed
```

**Available tools:**
- Read - Inspect files
- Glob - Find files by pattern
- Grep - Search with regex
- WebFetch - Get documentation
- Bash - Run commands
- Restart - Self-improvement

**Safety built-in:**
- Approve once or save patterns
- Session or persistent approvals
- Wildcards and regex matching
- Manage with `/patterns` commands

### 📊 HTTP Daemon Mode - Multi-Client Server

Run as an OpenAI-compatible API server:

```bash
# Start daemon
./shammah daemon --bind 127.0.0.1:11435

# Use from any OpenAI-compatible client
curl http://127.0.0.1:11435/v1/chat/completions \
  -H "Content-Type: application/json" \
  -d '{
    "model": "claude-sonnet-4-20250514",
    "messages": [{"role": "user", "content": "Hello!"}]
  }'
```

**Features:**
- OpenAI-compatible API (drop-in replacement)
- Tool execution on client side (proper context/security)
- Session management with auto-cleanup
- Prometheus metrics for monitoring
- Production-ready (run as service)

## How It Works

```
User Query
    ↓
Is local model ready?
    ├─ NO  → Forward to Claude API (graceful fallback)
    └─ YES → Use local model + LoRA adapters
    ↓
Response to User
    ↓
User provides feedback (optional)
    ├─ 🔴 High-weight (10x) → Critical issues
    ├─ 🟡 Medium-weight (3x) → Improvements
    └─ 🟢 Normal-weight (1x) → Good examples
    ↓
Background LoRA training (non-blocking)
    ↓
Future responses incorporate learnings
```

## Basic Usage

### Interactive REPL

```bash
./shammah

> How do I use lifetimes in Rust?
> Read my src/main.rs and suggest improvements
> Run the tests to see if my changes work
> /feedback high - Never use unsafe without documenting why
> /train - Manually trigger LoRA training
> /model status - Check current model
> /patterns - Manage tool approvals
```

### Single Query

```bash
./shammah query "What's the best way to handle errors in Rust?"

# Or pipe input
echo "Explain closures" | ./shammah
```

### HTTP Daemon

```bash
# Start daemon
./shammah daemon-start

# Check status
./shammah daemon-status

# Stop daemon
./shammah daemon-stop
```

## Configuration

Config file: `~/.shammah/config.toml`

```toml
streaming_enabled = true
tui_enabled = true

[backend]
enabled = true
execution_target = "coreml"  # or "cpu", "cuda"
model_family = "Qwen2"
model_size = "Medium"  # or "Small", "Large", "XLarge"

[[teachers]]
provider = "claude"
api_key = "sk-ant-..."  # Your Claude API key
model = "claude-sonnet-4-20250514"
name = "Claude (Primary)"

[client]
use_daemon = true
daemon_address = "127.0.0.1:11435"
auto_spawn = true
```

## Learning Timeline

**Day 1:**
- ✅ High-quality responses (pre-trained Qwen)
- ✅ All coding queries work well
- 🔄 Start collecting feedback

**Week 1:**
- ✅ Learns your code style
- ✅ Adapts to preferred libraries
- 🔄 Building specialized adapter

**Month 1:**
- ✅ Specialized for your domain
- ✅ Remembers critical feedback
- ✅ Handles codebase patterns

**Month 3+:**
- ✅ Highly specialized to your work
- ✅ Multiple domain adapters
- ✅ Recognizes anti-patterns

## Performance

| Metric | Value |
|--------|-------|
| REPL startup | <100ms |
| Model loading (cached) | 2-3 seconds |
| First download | 1.5-14GB |
| Local response time | 500ms-2s |
| LoRA overhead | +50-100ms |
| RAM usage | 3-28GB (model dependent) |
| Disk space | Model + ~5MB per adapter |

## Why Shammah vs Alternatives?

### vs. Claude API Directly
- ✅ Works offline after setup
- ✅ Faster local responses
- ✅ Learns your patterns
- ✅ Privacy - code stays local
- ✅ No per-query costs

### vs. Training Custom Models
- ✅ Immediate quality (day 1)
- ✅ No training period
- ✅ Efficient LoRA learning
- ✅ Trains on your machine

### vs. Other Local AI
- ✅ Full tool execution
- ✅ Weighted feedback
- ✅ Instant startup
- ✅ Apple Silicon GPU acceleration

## Requirements

- **macOS** (Apple Silicon or Intel), **Linux**, or **Windows**
- **Rust** 1.70+ (for building from source)
- **8GB+ RAM** (16GB+ recommended)
- **2-15GB disk space** (for models)
- **Claude API key** (free tier works) - for fallback
- **HuggingFace account** (free) - for model downloads

## Troubleshooting

### Model won't download

```bash
# Check HuggingFace token
cat ~/.cache/huggingface/token

# Should show: hf_...
# If not, get token from https://huggingface.co/settings/tokens
```

### Out of memory

```bash
# Switch to smaller model
./shammah
> /model select 1.5B
```

### Slow responses

```bash
# Check if using GPU
> /model status
# Should show: Device: Metal ✓ (on Mac)

# If not, try:
> /model device metal
```

### Setup wizard issues

```bash
# Run setup again to reconfigure
./shammah setup

# Or manually edit config
vim ~/.shammah/config.toml
```

## Documentation

- [Architecture](docs/ARCHITECTURE.md) - System design
- [Contributing](CONTRIBUTING.md) - Development guide
- [Changelog](CHANGELOG.md) - Release history
- [Roadmap](docs/ROADMAP.md) - Future plans

## Community & Support

- **Issues**: https://github.com/schancel/shammah/issues
- **Discussions**: https://github.com/schancel/shammah/discussions
- **Discord**: [Coming soon]

## Contributing

We welcome contributions! Areas of interest:
- Additional model backends
- LoRA training optimizations
- Multi-GPU support
- Quantization for lower memory
- Additional tool implementations

See [CONTRIBUTING.md](CONTRIBUTING.md) for guidelines.

## License

MIT OR Apache-2.0

---

<div align="center">

**Shammah** - Your AI coding watchman that learns and improves with you. 🛡️

[Download](https://github.com/schancel/shammah/releases) •
[Docs](#documentation) •
[Report Bug](https://github.com/schancel/shammah/issues)

Made with ❤️ for developers who value privacy and local-first tools

</div>
