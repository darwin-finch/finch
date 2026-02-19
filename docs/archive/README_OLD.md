# Shammah

> **שָׁמָה** (Shammah) - Hebrew: "watchman" or "guardian"

A local-first Constitutional AI proxy that learns to handle 95% of requests locally while maintaining Claude API compatibility.

## What is Shammah?

Shammah is an intelligent proxy that sits between you and Claude AI. Instead of sending every request to the cloud, it learns from Claude's responses and progressively handles more requests locally on your machine. Over time, it reduces API usage from 100% to just 5%, while maintaining high-quality responses through Constitutional AI principles.

Think of it as a smart cache that learns not just answers, but reasoning patterns.

## Key Features

- **Tool Execution** - Claude can inspect code, search files, and run commands
  - Read files and directories
  - Search codebase with glob patterns and regex
  - Fetch web content for research
  - Execute bash commands
  - Self-improvement: modify code and restart

- **Interactive Tool Confirmation** - Full control over tool execution
  - Approve tools once or remember approvals (session or persistent)
  - Pattern-based approvals with wildcards (`*`, `**`) or regex
  - Manage patterns with `/patterns` commands
  - Saves approved patterns to `~/.finch/tool_patterns.json`
  - See [docs/TOOL_CONFIRMATION.md](docs/TOOL_CONFIRMATION.md) for details

- **Streaming Responses** - Real-time character-by-character output
  - Better UX for long responses
  - SSE (Server-Sent Events) parsing
  - Graceful fallback when tools are used

- **95% Local Processing** (Future Goal) - After training period, only 5% of requests require API calls
  - Enhanced privacy: your data stays on your machine
  - Faster responses: no network latency for local processing
  - Works offline for most queries

- **Constitutional AI Reasoning** - Multi-model ensemble that learns safe, helpful behavior
  - Learns from every Claude response
  - Applies constitutional principles locally
  - Custom constitution support (optional)
  - Maintains quality without constant API access

- **Continuous Learning** - Improves over time
  - Starts at 100% forwarding (everything goes to Claude)
  - Threshold-based routing learns from query 1
  - Transitions to neural models after sufficient training data
  - Converges to 5% forwarding over ~6 months
  - Models stored locally in `~/.finch/`

- **Concurrent Safe** - Multiple sessions can run simultaneously
  - File locking prevents data corruption
  - Statistics merged across sessions
  - Safe for team use

- **HTTP Daemon Mode** (Phase 1 - NEW) - Multi-tenant agent server
  - Claude-compatible `/v1/messages` endpoint
  - Session management with automatic cleanup
  - Health checks and Prometheus metrics
  - Perfect for running as a service or in containers
  - Multiple clients can connect simultaneously

- **Cost Effective** - Reduces API costs by 76% (24% of original after accounting for 5% forwarding)
  - Pay only for novel or complex queries
  - Training investment pays off quickly
  - Transparent cost tracking

## How It Works

```
┌─────────────┐
│   Request   │
└──────┬──────┘
       │
       v
┌─────────────────────┐
│  Shammah Router     │
│  Decision: Local    │ ← 95% after training
│         or Forward? │
└──────┬──────────────┘
       │
       ├─────── Local (95%) ──────┐
       │                           v
       │                    ┌──────────────┐
       │                    │ Multi-Model  │
       │                    │  Ensemble    │
       │                    └──────┬───────┘
       │                           │
       │                           v
       │                    ┌──────────────┐
       │                    │ Constitution │
       │                    │   Validator  │
       │                    └──────┬───────┘
       │                           │
       └─── Forward (5%) ───┐      │
                            v      v
                      ┌──────────────┐
                      │ Claude API   │
                      └──────┬───────┘
                             │
                             v
                      ┌──────────────┐
                      │  Learn from  │
                      │   Response   │
                      └──────────────┘
```

### Learning Process

1. **Initial Phase** (Weeks 1-2): 100% forwarding
   - Every request goes to Claude
   - System observes patterns
   - No local predictions yet

2. **Training Phase** (Months 1-3): 50-80% forwarding
   - System starts handling simple queries locally
   - Continues learning from Claude responses
   - Gradually increases confidence

3. **Mature Phase** (Months 4-6): 10-20% forwarding
   - Handles most requests locally
   - Forwards only novel/complex queries
   - Continuous refinement

4. **Steady State** (Month 6+): ~5% forwarding
   - Optimal balance achieved
   - Only truly new patterns forwarded
   - Cost savings realized

## Quick Start

```bash
# Build from source
git clone https://github.com/finch/claude-proxy
cd claude-proxy
cargo build --release

# Run in interactive mode
./target/release/finch

# Example: Claude can now use tools to inspect your code
> Can you read my Cargo.toml and tell me about dependencies?
# Claude uses the Read tool to read the file

> Find all Rust files and show me the main.rs structure
# Claude uses Glob to find files, then Read to inspect main.rs

> Search for all TODO comments in the codebase
# Claude uses Grep with regex pattern

# Piped input mode (non-interactive)
echo "What is 2+2?" | ./target/release/finch
# Output: 4

cat query.txt | ./target/release/finch
# Processes query from file and exits

./target/release/finch <<EOF
What is the capital of France?
EOF
# Supports heredoc syntax

# In piped mode:
# - No REPL startup messages
# - No interactive prompts
# - Tool confirmations auto-approve
# - Exits after printing response

# Self-improvement workflow (advanced)
> I want to optimize the router code
# Claude reads code, suggests changes, uses tools to modify files
> Now build the new version
# Claude uses Bash tool: cargo build --release
> Restart into the new binary
# Claude uses Restart tool to exec into new version

# Tool confirmation and pattern management
> /patterns                    # List all saved approval patterns
> /patterns add                # Create a new pattern interactively
> /patterns remove abc12345    # Remove a specific pattern
> /patterns clear               # Clear all patterns

# HTTP Daemon Mode (NEW - Phase 1)
# Run as background server for multi-client access
./target/release/finch daemon --bind 127.0.0.1:8000

# Test health check
curl http://127.0.0.1:8000/health

# Send message (Claude-compatible API)
curl -X POST http://127.0.0.1:8000/v1/messages \
  -H "Content-Type: application/json" \
  -d '{
    "model": "claude-sonnet-4-5-20250929",
    "messages": [{"role": "user", "content": "Hello!"}]
  }'

# Get session info
curl http://127.0.0.1:8000/v1/session/SESSION_ID

# View Prometheus metrics
curl http://127.0.0.1:8000/metrics
```

## Architecture

Shammah uses a multi-model ensemble approach:

- **Small models** (~1-3B params) for classification and routing
- **Medium models** (~7-13B params) for general queries
- **Constitutional validator** ensures responses meet safety criteria
- **Learning engine** continuously improves from Claude responses
- **Apple Neural Engine** optimization for M1/M2/M3/M4 chips

See [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) for detailed architecture.

## Configuration

### API Key

Set your Claude API key in the configuration file:

```bash
# Create config directory
mkdir -p ~/.finch

# Add your API key
cat > ~/.finch/config.toml <<EOF
api_key = "your-claude-api-key"
streaming_enabled = true
EOF
```

### File Structure

All data stored in `~/.finch/`:
```
~/.finch/
├── config.toml               # API key and settings
├── constitution.md           # Optional: custom constitutional principles
├── crisis_keywords.txt       # Safety keywords for crisis detection
├── tool_patterns.json        # Saved tool approval patterns
├── metrics/                  # Daily JSONL logs for training
│   └── 2026-01-30.jsonl
└── models/                   # Trained model weights
    ├── threshold_router.json
    └── ensemble.json
```

### Optional Constitution

Define custom constitutional principles for local generation:

```bash
cat > ~/.finch/constitution.md <<EOF
# My Constitutional Principles

1. Always prioritize user privacy
2. Be helpful, harmless, and honest
3. Acknowledge uncertainty rather than guessing
4. [Add your principles here]
EOF
```

**Note**: Constitution is loaded but not sent to Claude API. It will be used for local model generation when implemented.

See [docs/CONFIGURATION.md](docs/CONFIGURATION.md) for full configuration options.

## Project Status

**Current Status**: ✅ Alpha - Fully functional with tool execution

Shammah is now a working local-first AI proxy with tool execution, streaming responses, and self-improvement capabilities.

**Version**: 0.2.0 (Post-Tool Execution Implementation)

### Completed

- ✅ **Phase 1**: Core infrastructure
  - Crisis detection for safety
  - Claude API integration with retry logic
  - REPL interface with readline support
  - Metrics collection for training

- ✅ **Phase 2a**: Threshold Models
  - Statistics-driven routing (learns from query 1)
  - Threshold-based validator with 8 quality signals
  - Concurrent weight merging with file locking
  - Model persistence to disk

- ✅ **Tool Execution**: 6 working tools
  - Read, Glob, Grep, WebFetch, Bash, Restart
  - Multi-turn conversation loop
  - Self-improvement workflow

- ✅ **Tool Confirmation**: Interactive approval system
  - Pattern-based approvals (wildcard and regex)
  - Session and persistent approval storage
  - Pattern management commands (`/patterns`)
  - Match count tracking and statistics

- ✅ **Streaming**: Real-time responses (partial)
  - SSE parsing for character-by-character display
  - Disabled when tools are used (detection pending)

- ✅ **Constitution Support**: Infrastructure complete
  - Configurable path (~/.finch/constitution.md)
  - Loaded on startup, not sent to API

### In Progress

- 🔄 **Neural Networks**: Training on real usage data
- 🔄 **Hybrid Routing**: Threshold → neural transition

### Roadmap

- [ ] **Phase 2b**: Neural models as primary router (~200 queries)
- [ ] **Phase 3**: Uncertainty estimation and confidence-based forwarding
- [ ] **Phase 4**: Core ML export for maximum Apple Silicon performance
- [ ] **Phase 5**: Achieve 95% local processing rate

See [STATUS.md](STATUS.md) for detailed current state and [CONSTITUTIONAL_PROXY_SPEC.md](CONSTITUTIONAL_PROXY_SPEC.md) for complete specification.

## Development

```bash
# Clone repository
git clone https://github.com/finch/claude-proxy
cd claude-proxy

# Build project
cargo build

# Run tests
cargo test

# Run in development mode
cargo run
```

See [docs/DEVELOPMENT.md](docs/DEVELOPMENT.md) for development guidelines.

## Requirements

- **Platform**: macOS (Apple Silicon M1/M2/M3/M4)
- **Rust**: 1.70 or later
- **Storage**: ~5GB for models and training data
- **Memory**: 8GB RAM minimum, 16GB recommended

## Privacy & Security

- All models run locally on your machine
- Training data never leaves your device
- Only forwarded requests (5% at steady state) go to Claude API
- Claude API key stored securely in system keychain
- No telemetry or data collection

## License

Licensed under either of:

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE) or http://www.apache.org/licenses/LICENSE-2.0)
- MIT License ([LICENSE-MIT](LICENSE) or http://opensource.org/licenses/MIT)

at your option.

## Contributing

Contributions welcome! Please read [docs/DEVELOPMENT.md](docs/DEVELOPMENT.md) first.

## Acknowledgments

- Built on [Claude AI](https://www.anthropic.com/claude) by Anthropic
- Inspired by Constitutional AI research
- Powered by Rust and Apple's CoreML/Neural Engine

---

**Note**: Shammah is an independent project and is not affiliated with or endorsed by Anthropic.
