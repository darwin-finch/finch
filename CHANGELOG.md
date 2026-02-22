# Changelog

All notable changes to Shammah will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased] - 0.5.0-dev

### Added
- **Context auto-loading**: Finch now automatically discovers and injects `CLAUDE.md` / `FINCH.md`
  project instructions into the system prompt on startup (matching Claude Code behavior)
  - `~/.claude/CLAUDE.md` — user-level Claude Code defaults
  - `~/.finch/FINCH.md` — user-level Finch defaults
  - `CLAUDE.md` / `FINCH.md` found walking upward from cwd to filesystem root (outermost first)
  - `FINCH.md` supported as a vendor-neutral, tool-agnostic naming convention
  - New module: `src/context/claude_md.rs` with 6 unit tests
  - New module: `src/context/mod.rs`
- **Unified `[[providers]]` config format**: New `ProviderEntry` tagged enum replaces the legacy
  `[fallback]` / `TeacherEntry` system for configuring cloud providers and local models
  - Supports Claude, OpenAI, Grok, Gemini, Mistral, Groq, and Local as first-class entries
  - Backwards-compatible: old `[[teachers]]` format auto-migrates on load
  - Save always writes the new `[[providers]]` format
- **Darwin finch ASCII bird banner** at REPL startup

### Changed
- `build_system_prompt(cwd, claude_md)` — added `claude_md: Option<&str>` parameter; injects
  collected context under `## Project Instructions` header
- Linux CI runner bumped to ubuntu-24.04 (glibc 2.39) for prebuilt ONNX Runtime compatibility

## [0.2.2] - 2026-02-18

### Fixed
- **Linux binary builds**: Added diagnostic output to debug compilation failures
  - Re-enabled Linux x86_64 builds with error capture
  - Added pre-build environment checks

## [0.2.1] - 2026-02-17

### Fixed
- **GitHub Actions release workflow**: Updated to use modern `gh release create` command
  - Previous v0.2.0 release workflow used deprecated actions that failed to upload binaries
  - New workflow uploads artifacts and creates release with all binaries in one step
  - Users can now download pre-built binaries for Linux, macOS Intel, and macOS ARM64

## [0.2.0] - 2026-02-16

### Added
- **Setup wizard improvements**: Pre-fills existing Claude API key with visual feedback
  - Shows truncated view for long keys (first 40 + last 10 chars)
  - Green text indicates pre-filled values
  - Clear instructions: "(Pre-filled - press Backspace to clear)"
  - Cursor indicator for better UX
- **GitHub Actions CI/CD**: Automated testing and releases
  - CI workflow runs on every push (formatting, clippy, builds)
  - Release workflow auto-builds binaries for all platforms on version tags
  - Multi-platform support: Linux x86_64, macOS x86_64, macOS ARM64
- **Config migration**: Automatic handling of deprecated execution targets
  - Gracefully filters out deprecated "metal" variant
  - Falls back to platform defaults if needed
  - Logs warnings for unknown targets

### Fixed
- **Setup wizard bug**: Config failed to load when `fallback_chain` contained deprecated "metal" variant
  - Root cause: Deserialization error caused wizard to start with empty values
  - Solution: Custom deserializer filters invalid entries
- **Debug logging loop**: Removed infinite `eprintln!` statements in shadow buffer
  - Previously caused binary to hang on `--version` command
  - Now runs cleanly without debug spam

### Changed
- **README improvements**: Made installation clearer and more compelling
  - Added "Why Shammah?" section up front
  - One-liner download commands for all platforms
  - Clearer quick start (30 seconds to working AI)
  - Better formatting with badges and navigation
  - Comparison tables vs alternatives

### Infrastructure
- GitHub Actions workflows for CI and releases
- Automated binary builds on tag push
- Multi-platform release artifacts

## [0.1.0] - 2026-02-10

Initial release of Shammah - Local-first AI coding assistant.

### Added
- **Core Features**:
  - Pre-trained local model support (Qwen, Llama, Mistral, Phi via ONNX)
  - Weighted LoRA fine-tuning for continuous improvement
  - Progressive bootstrap (instant startup with background loading)
  - Tool execution system (Read, Glob, Grep, WebFetch, Bash, Restart)
  - HTTP daemon mode with OpenAI-compatible API

- **Model Support**:
  - ONNX Runtime integration with CoreML/Metal acceleration
  - KV cache for efficient autoregressive generation
  - Adaptive model selection based on system RAM (1.5B/3B/7B/14B)
  - Multiple model families (Qwen, Llama, Mistral, Phi, DeepSeek)

- **Tool System**:
  - Interactive tool confirmation dialogs
  - Session and persistent approval patterns
  - Pattern-based matching (wildcards and regex)
  - Tool pass-through in daemon mode

- **TUI/UX**:
  - Professional terminal UI with scrollback
  - Multi-line input with Shift+Enter
  - Command history (1000 commands, persistent)
  - Live status bar with tokens/latency/speed
  - Query cancellation with Ctrl+C
  - Feedback system (Ctrl+G good, Ctrl+B bad)

- **Multi-Provider Support**:
  - Teacher APIs: Claude, GPT-4, Gemini, Grok, Mistral, Groq
  - Adaptive routing (local first, graceful fallback)
  - Setup wizard for configuration

- **Training System**:
  - Weighted example collection (10x/3x/1x)
  - JSONL export for training queue
  - Python training script (PyTorch + PEFT)
  - Non-blocking background training

### Infrastructure
- Rust-based implementation
- Cross-platform support (macOS, Linux, Windows)
- Configuration file (~/.finch/config.toml)
- Model caching (~/.cache/huggingface/hub/)
- Adapter storage (~/.finch/adapters/)

[0.2.0]: https://github.com/darwin-finch/finch/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/darwin-finch/finch/releases/tag/v0.1.0
