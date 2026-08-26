# Shammah User Guide

**Version:** 0.7.4
**Last Updated:** 2026-02-24

## Table of Contents

1. [What is Shammah?](#what-is-finch)
2. [Quick Start](#quick-start)
3. [Installation](#installation)
4. [First-Time Setup](#first-time-setup)
5. [Basic Usage](#basic-usage)
6. [Tool Confirmations](#tool-confirmations)
7. [Advanced Features](#advanced-features)
8. [Daemon Mode](#daemon-mode)
9. [Configuration](#configuration)
10. [Troubleshooting](#troubleshooting)

---

## What is Shammah?

Shammah is a **local-first AI coding assistant** that combines the power of:
- **Pre-trained local models** (Qwen via ONNX Runtime) - works offline, preserves privacy
- **Teacher APIs** (Claude, GPT-4, Gemini, Grok) - high-quality fallback when needed
- **Tool execution** - can read files, run commands, search code
- **Explicit feedback** - privately retain ratings for future supported learning

### Key Benefits

✅ **Works offline** - Local model runs on your machine
✅ **Privacy-first** - Code stays on your device
✅ **Instant startup** - <100ms to REPL (progressive loading)
✅ **Feedback-aware** - Retains only feedback you explicitly submit
✅ **Multi-provider** - Configure multiple teacher APIs
✅ **Tool-enabled** - Can execute commands with your approval

---

## Quick Start

```bash
# Start interactive session
finch

# Ask a question
> How do I use lifetimes in Rust?

# Let it use tools (with your approval)
> Can you read my Cargo.toml and suggest improvements?
```

That's it! Shammah will guide you through setup on first run.

---

## Installation

### From Source

```bash
# Clone repository
git clone https://github.com/darwin-finch/finch.git
cd finch

# Build (requires Rust 1.70+)
cargo build --release

# Install
cargo install --path .

# Verify
finch --version
```

### System Requirements

- **RAM:** 8GB minimum (16GB+ recommended)
- **Disk:** 5-10GB for models
- **OS:** macOS (Apple Silicon recommended), Linux, Windows
- **Rust:** 1.70 or newer

---

## First-Time Setup

When you run `finch` for the first time, the setup wizard will guide you through:

### 1. Welcome Screen

```
┌─────────────────────────────────────────────┐
│ Welcome to Shammah Setup                    │
│                                             │
│ This wizard will help you configure:       │
│  • Teacher API (Claude/GPT-4/Gemini/Grok)  │
│  • Local model (optional, offline mode)    │
│  • Tool permissions                         │
└─────────────────────────────────────────────┘
```

### 2. Teacher API Configuration

**Recommended:** Start with Claude API for best results.

```
Teacher API Key:
> sk-ant-...

Which teachers would you like to configure?
  [x] Claude (Anthropic)  ← Selected
  [ ] GPT-4 (OpenAI)
  [ ] Gemini (Google)
  [ ] Grok (xAI)

Press 'a' to add more providers later.
```

You can add multiple providers and Shammah will use them as fallbacks.

### 3. Local Model Setup (Optional)

```
Would you like to enable local model?
  [x] Yes - Download model for offline use
  [ ] No - Use teacher APIs only

Model size (based on your RAM):
  [ ] Small (1.5B) - 8GB RAM
  [x] Medium (3B) - 16GB RAM  ← Recommended for 16GB systems
  [ ] Large (7B) - 32GB RAM
  [ ] XLarge (14B) - 64GB+ RAM
```

**First run:** Model downloads in background (5-30 minutes depending on size).
**Subsequent runs:** Instant startup, model loads from cache.

### 4. Completion

Setup is saved to `~/.finch/config.toml`. You can edit it manually or re-run setup:

```bash
finch setup
```

---

## Basic Usage

### Interactive REPL

```bash
finch
```

This starts an interactive session. You can:
- Ask coding questions
- Request code reviews
- Get explanations
- Debug issues

**Example:**

```
❯ What's the difference between String and &str in Rust?

String is an owned, heap-allocated string that can grow and shrink.
&str is a borrowed string slice, typically a view into a String or
string literal. Use &str for function parameters when you don't need
ownership, and String when you need to own or modify the data.

Would you like to see examples?
```

### Single Query Mode

```bash
finch query "Explain async/await in Rust"
```

Runs a single query and exits - useful for scripts.

### Keyboard Shortcuts

**In the REPL:**

- **Enter:** Submit query
- **Shift+Enter:** New line (multi-line input)
- **Ctrl+C:** Cancel in-progress query
- **Ctrl+G:** Privately record a good response
- **Ctrl+B:** Privately record a bad response
- **Up/Down:** Navigate command history

**In dialogs (tool confirmations, plan approval, etc.):**

- **↑/↓:** Navigate between options
- **Space:** Toggle selection (MultiSelect dialogs)
- **Enter:** Submit / confirm highlighted option
- **Esc:** Cancel dialog
- **o / O:** Jump to the "Other (custom response)" row and activate text entry
- **Any printable char** (when cursor is on the Other row): immediately start typing your custom response — no Enter needed to activate the field
- **Shift+Enter / Option+Enter:** Insert a newline in the custom text field (multi-line custom responses)
- **Backspace / ← →:** Edit the custom text field as normal

---

## Tool Confirmations

When Shammah needs to execute tools (read files, run commands), it will ask for your approval:

```
┌──────────────────────────────────────────┐
│ Tool 'bash' requires approval            │
│                                          │
│ Input:                                   │
│ {                                        │
│   "command": "cargo test"                │
│ }                                        │
│                                          │
│ 1. Allow Once                            │
│ 2. Allow Exact (Session)                 │
│ 3. Allow Pattern (Session)               │
│ 4. Allow Exact (Persistent)              │
│ 5. Allow Pattern (Persistent)            │
│ 6. Deny                                  │
│                                          │
│ ↑/↓: Navigate  Enter: Confirm  Esc: Deny│
└──────────────────────────────────────────┘
```

### Approval Options

1. **Allow Once** - Execute this time only
2. **Allow Exact (Session)** - Allow this exact command until restart
3. **Allow Pattern (Session)** - Allow similar commands until restart
4. **Allow Exact (Persistent)** - Always allow this exact command
5. **Allow Pattern (Persistent)** - Always allow similar commands
6. **Deny** - Don't execute

**Patterns** let you approve categories of commands:
- `cargo test` in any directory
- Any `git` command in `/home/user/projects`
- Reading any file in `/home/user/safe-dir`

### Structured Patterns

You can create patterns that match command components separately:

```
cmd:"cargo test"  args:"*"  dir:"/home/*/projects"
```

This allows:
- `cargo test` in any user's projects directory
- Any arguments (e.g., `--release`, `--bin foo`)

Managed via `finch tools` command.

---

## Advanced Features

### Command History

Press **Up/Down** arrows to navigate through previous queries.

History is saved to `~/.finch/history.txt` (last 1000 commands).

### Feedback System

Record explicit feedback about a response:

- **Ctrl+G** - Good response
- **Ctrl+B** - Bad response

Feedback retains historical weights:
- Good: 1x weight
- Bad: 10x weight

Data is saved privately to `~/.finch/feedback.jsonl`. It is not collected from
ordinary requests and does not trigger training, Python, queue processing, or
adapter loading.

### Status Bar

The status bar shows:
```
Model: qwen-3b | Tokens: 234→156 | Latency: 1.2s | Speed: 130 tok/s | Memory: 4.2GB / 16GB
```

- **Model:** Current model being used
- **Tokens:** Input→Output token counts
- **Latency:** Response time
- **Speed:** Tokens per second
- **Memory:** Process/System RAM usage

### Multi-Provider Setup

Add additional teacher providers after initial setup:

```bash
finch setup
# Navigate to teacher configuration
# Press 'a' to add a new provider
```

Providers are tried in order, with automatic fallback on errors.

---

## Daemon Mode

For advanced users and integrations.

### Auto-Spawning Daemon

The daemon starts automatically when you use `finch`. It:
- Runs in background
- Serves OpenAI-compatible HTTP API
- Manages sessions and model loading
- Handles tool execution pass-through

### Manual Daemon Management

```bash
# Check daemon status
finch daemon-status

# Stop daemon
finch daemon-stop

# Start daemon manually
finch daemon-start
```

### HTTP API

The daemon serves an OpenAI-compatible API on `http://127.0.0.1:8000`:

```bash
curl -X POST http://127.0.0.1:8000/v1/messages \
  -H "Content-Type: application/json" \
  -d '{
    "model": "qwen-3b",
    "messages": [{"role": "user", "content": "Hello!"}]
  }'
```

Use this to integrate Shammah with other tools (VSCode extensions, etc.).

---

## Configuration

Config file: `~/.finch/config.toml`

### Example Configuration

```toml
# Teacher API configuration
[teachers]
[[teachers.list]]
provider = "claude"
api_key = "sk-ant-..."
model = "claude-sonnet-4-5"

[[teachers.list]]
provider = "openai"
api_key = "sk-..."
model = "gpt-4"

# Local model configuration
[backend]
model_family = "Qwen"
model_size = "Medium"
execution_provider = "CoreML"  # or "CPU"

# Daemon configuration
[daemon]
bind_address = "127.0.0.1:8000"
auto_spawn = true

# Automatic LoRA training is disabled; there are no active auto_train settings.

# Tool permissions
[tools]
default_rule = "ask"  # "allow", "ask", or "deny"
max_tool_turns = 5
```

### Storage Locations

- **Config:** `~/.finch/config.toml`
- **Models:** `~/.cache/huggingface/hub/`
- **Adapters:** `~/.finch/adapters/`
- **Feedback:** `~/.finch/feedback.jsonl`
- **History:** `~/.finch/history.txt`
- **Tool Patterns:** `~/.finch/tool_patterns.json`
- **Daemon PID:** `~/.finch/daemon.pid`

---

## Troubleshooting

### Model Download Issues

**Problem:** Model download stuck or slow

```bash
# Check HuggingFace Hub cache
ls ~/.cache/huggingface/hub/

# Re-download model
rm -rf ~/.cache/huggingface/hub/models--*Qwen*
finch  # Will re-download on startup
```

**Problem:** Not enough disk space

Local models require:
- 1.5B: ~3GB
- 3B: ~6GB
- 7B: ~14GB
- 14B: ~28GB

Free up space or use teacher-only mode (no local model).

### Daemon Issues

**Problem:** Daemon won't start

```bash
# Check if port is in use
lsof -i :8000

# Force stop any existing daemon
finch daemon-stop
rm ~/.finch/daemon.pid

# Start fresh
finch
```

**Problem:** Connection refused errors

```bash
# Check daemon status
finch daemon-status

# Restart daemon
finch daemon-stop
finch daemon-start
```

### Memory Issues

**Problem:** System running out of RAM

1. Use a smaller model size in config
2. Close other memory-intensive applications
3. Check memory usage: `finch memory`
4. Consider teacher-only mode (no local model)

### Performance Issues

**Problem:** Slow responses

- **Local model:** Normal on first query (model loading), faster on subsequent queries
- **Teacher API:** Check network connection
- **Check status bar** for actual response times

**Problem:** High CPU usage

This is normal during:
- Model loading (first query)
- Model inference (generating responses)

Feedback does not start background training. Unexpected Python or sustained
training activity should be treated as a bug.

### Tool Execution Issues

**Problem:** Tools not executing

1. Check tool permissions in config
2. Ensure you're approving tools when prompted
3. Check `~/.finch/tool_patterns.json` for conflicting patterns

**Problem:** Confirmation dialogs not showing

Make sure terminal height is at least 15 lines:
```bash
# Check terminal size
echo $LINES $COLUMNS

# Resize terminal if needed
```

### Getting Help

```bash
# Show help
finch --help

# Show tool status
finch tools

# Show memory usage
finch memory

# View logs
tail -f ~/.finch/logs/finch.log  # if enabled
```

**GitHub Issues:** https://github.com/darwin-finch/finch/issues

---

## Tips & Best Practices

### 1. Start with Teacher APIs

Use Claude or GPT-4 first while local model downloads. You'll get high-quality responses immediately.

### 2. Approve Tool Patterns

Instead of approving every `cargo test` individually, approve the pattern once:
```
cmd:"cargo test" args:"*" dir:"/home/user/*"
```

### 3. Use Feedback Wisely

- **Ctrl+G (Good):** When response is exactly what you wanted
- **Ctrl+B (Bad):** When response has wrong approach or strategy

This privately records your preference for a future supported learning path.
It does not train or alter the current model.

### 4. Multi-Provider Fallback

Configure multiple providers for resilience:
1. Claude (primary)
2. GPT-4 (fallback)
3. Gemini (backup)

If one is down or rate-limited, Shammah automatically tries the next.

### 5. Monitor Memory

Keep an eye on the status bar. If memory usage is high:
- Consider smaller model
- Close other applications
- Restart daemon to free memory

### 6. Keyboard Shortcuts

Learn the shortcuts:
- **Shift+Enter** for multi-line input
- **Up/Down** for history
- **Ctrl+C** to cancel long queries
- **Ctrl+G/B** for feedback

### 7. Review Tool Patterns

Periodically review your approved patterns:
```bash
finch tools
```

Remove overly permissive patterns for security.

---

## What's Next?

- **Explore tools:** Let Shammah read your code, run tests, search files
- **Provide feedback:** Privately record your preferences without triggering training
- **Configure multiple providers:** Set up fallback APIs
- **Track LoRA feasibility:** Training and adapter loading remain blocked on Issues #1, #7, and #74

**Happy coding!** 🚀

For more details, see:
- **CLAUDE.md** - AI assistant context and architecture
- **ARCHITECTURE.md** - Technical implementation details
- **STATUS.md** - Current project status and roadmap
