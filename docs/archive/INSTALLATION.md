# Installation Guide - Shammah Phase 1 MVP

## Prerequisites

- **Rust 1.70+** - Install from https://rustup.rs
- **macOS** - Apple Silicon (M1/M2/M3/M4) recommended
- **Claude API Key** - Get from https://console.anthropic.com/

## Installation Steps

### 1. Clone and Build

```bash
cd /Users/finch/repos/claude-proxy
cargo build --release
```

The binary will be at: `./target/release/finch`

### 2. Configure API Key

**Option A: Using Claude Code settings (recommended)**

Create or edit `~/.claude/settings.json`:

```json
{
  "env": {
    "ANTHROPIC_API_KEY": "sk-ant-api03-..."
  }
}
```

**Option B: Using environment variable**

```bash
export ANTHROPIC_API_KEY="sk-ant-api03-..."
```

Add this to your shell profile (`.zshrc` or `.bashrc`) to make it permanent.

### 3. Verify Installation

Run the tests:

```bash
cargo test
```

Expected output: All tests passing.

Run the example:

```bash
cargo run --example simple_query
```

Expected: Shows routing decisions for test queries.

## Usage

### Interactive REPL Mode

```bash
cargo run
# or
./target/release/finch
```

Example session:

```
Shammah v0.1.0 - Constitutional AI Proxy (Phase 1 MVP)
Using API key from: ~/.claude/settings.json ✓
Loaded 10 constitutional patterns ✓
Loaded crisis detection keywords ✓
Ready. Type /help for commands.

You: What is the golden rule?

[Analyzing...]
├─ Crisis check: PASS
├─ Pattern match: reciprocity (0.21)
└─ Routing: LOCAL (12ms)

This relates to reciprocity dynamics - how the way we treat
others creates expectations of how we'll be treated in return...

You: /metrics

Metrics (last 24 hours):
  Total requests: 1
  Local: 1 (100.0%)
  Forwarded: 0 (0.0%)
  ...

You: /quit

Goodbye!
```

### Available Commands

- `/help` - Show help message
- `/quit` or `/exit` - Exit the REPL
- `/metrics` - Display routing statistics
- `/patterns` - List all 10 constitutional patterns
- `/debug` - Toggle debug output

## Phase 1 MVP Features

✅ **Implemented:**

1. **Configuration Loading**
   - Reads API key from `~/.claude/settings.json` or environment
   - Falls back gracefully with helpful error messages

2. **Pattern Matching (TF-IDF)**
   - 10 constitutional patterns implemented
   - Similarity threshold: 0.2 (20% match required)
   - Matches queries to pre-written template responses

3. **Crisis Detection**
   - 100% recall on crisis keywords
   - Self-harm, violence, and abuse detection
   - Always forwards crisis queries to Claude API

4. **Routing Logic**
   - Crisis detection → Forward
   - Pattern match ≥0.2 → Local
   - No match → Forward
   - Logs all decisions for metrics

5. **Metrics Logging**
   - Stores in `~/.local/share/finch/metrics/YYYY-MM-DD.jsonl`
   - Privacy-preserving (query hashing)
   - Daily rotation, keeps 30 days
   - Tracks local vs forward rates

6. **Claude API Integration**
   - Full Messages API support
   - Retry with exponential backoff
   - Error handling and timeouts

7. **Interactive CLI**
   - REPL interface
   - Real-time routing display
   - Slash commands for control

## Expected Performance (Phase 1)

- **Forward Rate:** 70-80% (acceptable for MVP)
- **Local Rate:** 20-30% (pattern matches only)
- **Crisis Detection:** 100% recall
- **Response Time (local):** <50ms
- **Response Time (forward):** ~1-2s

## Data Files

### Patterns (`data/patterns.json`)

10 constitutional patterns with pre-written responses:

1. reciprocity - Golden rule, karma, reciprocity
2. enforcement-paradox - Control backfiring
3. judgment-rebound - Harsh judgment invites judgment back
4. deception-compounding - Lies require more lies
5. truthfulness-enabling - Honesty enables error correction
6. systemic-oppression - Structural barriers and harm
7. trauma-patterns - Safety violation effects
8. information-asymmetry - Knowledge gaps create risk
9. coordination-failure - Individual vs collective incentives
10. path-dependence - Historical choices constrain options

### Crisis Keywords (`data/crisis_keywords.json`)

Three categories:

- **Self-harm:** suicide, kill myself, self-harm, etc.
- **Violence:** kill someone, hurt people, mass shooting, etc.
- **Abuse:** being abused, domestic violence, sexual assault, etc.

## Troubleshooting

### "Claude API key not found"

**Solution:** Set the API key in `~/.claude/settings.json` or export `ANTHROPIC_API_KEY`

### "Failed to read patterns file"

**Solution:** Run from project root directory where `data/` exists

### Build errors

**Solution:**
```bash
rustup update
cargo clean
cargo build
```

### Tests failing

**Solution:** Ensure you're in the project root and data files exist:
```bash
ls data/patterns.json data/crisis_keywords.json
```

## Metrics Storage

Metrics are stored in: `~/.local/share/finch/metrics/`

Format: One JSONL file per day (e.g., `2026-01-30.jsonl`)

Each line is a JSON object:

```json
{
  "timestamp": "2026-01-30T05:00:00Z",
  "query_hash": "abc123...",
  "routing_decision": "local",
  "pattern_id": "reciprocity",
  "confidence": 0.21,
  "forward_reason": null,
  "response_time_ms": 12
}
```

## Next Steps

After Phase 1 is working well:

- **Phase 2:** Uncertainty estimation (reduce forward rate to 40-50%)
- **Phase 3:** Tool integration and web search
- **Phase 4:** Apple Neural Engine optimization

## Support

For issues or questions:

- Check documentation in `docs/`
- Review `CLAUDE.md` for project context
- Check `CONSTITUTIONAL_PROXY_SPEC.md` for full specification
