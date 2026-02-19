# Building Shammah

## Prerequisites

- Rust 1.70+ (install from https://rustup.rs)
- macOS (Apple Silicon M1/M2/M3/M4 recommended)

## Quick Start

1. **Build the project:**
   ```bash
   cargo build --release
   ```

2. **Run tests:**
   ```bash
   cargo test
   ```

3. **Run the example:**
   ```bash
   cargo run --example simple_query
   ```

4. **Run the REPL:**
   ```bash
   cargo run
   # or after building:
   ./target/release/finch
   ```

## Configuration

Shammah reads your Claude API key from:
1. `~/.claude/settings.json` (Claude Code config)
2. `$ANTHROPIC_API_KEY` environment variable

Set your API key:
```bash
export ANTHROPIC_API_KEY="sk-ant-..."
```

Or configure Claude Code:
```bash
# Create ~/.claude/settings.json with:
{
  "env": {
    "ANTHROPIC_API_KEY": "sk-ant-..."
  }
}
```

## Development

**Format code:**
```bash
cargo fmt
```

**Run linter:**
```bash
cargo clippy
```

**Watch mode (requires cargo-watch):**
```bash
cargo install cargo-watch
cargo watch -x run
```

## Project Structure

```
finch/
├── src/
│   ├── main.rs           # Entry point
│   ├── lib.rs            # Library exports
│   ├── config/           # Configuration loading
│   ├── claude/           # Claude API client
│   ├── patterns/         # Pattern matching (TF-IDF)
│   ├── crisis/           # Crisis detection
│   ├── router/           # Routing logic
│   ├── metrics/          # Metrics logging
│   └── cli/              # REPL interface
├── data/
│   ├── patterns.json     # Constitutional patterns
│   └── crisis_keywords.json  # Crisis keywords
├── tests/                # Integration tests
└── examples/             # Usage examples
```

## Troubleshooting

**Error: "Claude API key not found"**
- Set `ANTHROPIC_API_KEY` environment variable
- Or configure `~/.claude/settings.json`

**Error: "Failed to read patterns file"**
- Ensure you're running from the project root directory
- Check that `data/patterns.json` exists

**Build errors:**
- Update Rust: `rustup update`
- Clean and rebuild: `cargo clean && cargo build`

## Phase 1 MVP Goals

- ✓ Load API key from Claude Code config
- ✓ 20-30% local processing via pattern matching
- ✓ 70-80% forwarding to Claude API
- ✓ 100% crisis detection (no false negatives)
- ✓ Metrics logging for Phase 2 training

## Next Steps

After Phase 1 is working:
- Phase 2: Uncertainty estimation (weeks 5-8)
- Phase 3: Tool integration (weeks 9-12)
- Phase 4: Apple Neural Engine optimization (weeks 13-16)
