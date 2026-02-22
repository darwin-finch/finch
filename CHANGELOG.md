# Changelog

All notable changes to Shammah will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.5.2] - 2026-02-22

### Added
- **Unified `[[providers]]` config format** (`src/config/provider.rs`): new `ProviderEntry` tagged
  enum (claude/openai/grok/gemini/mistral/groq/local) replaces legacy `TeacherEntry` system;
  backwards-compatible loader, save always writes new format
- **Candle backend as default feature**: `default = ["onnx", "candle"]`; Qwen 2.5 KV cache enabled
  so generation time no longer grows with conversation length; setup wizard filters to Qwen-only
  when Candle is selected
- **`Patch` tool** (`src/tools/implementations/patch.rs`): native unified-diff applier for
  targeted file edits — parses `@@ -start,count +start,count @@` hunks, verifies context lines,
  applies back-to-front; registered in REPL, agent, and daemon tool registries
- **Full cursor movement in `AskUserQuestion` custom text input**: Left/Right, Home/End, Delete,
  and char insertion at cursor position (Unicode-safe); block cursor rendered at correct offset
  in both `Dialog` and `TabState`
- **Tabbed multi-question dialog**: `show_llm_question` now calls `show_tabbed_dialog` when
  2+ questions are present, so all questions are visible simultaneously with ←/→ tab navigation
- **`DEFAULT_HTTP_ADDR`/`DEFAULT_WORKER_ADDR`/`DEFAULT_HTTP_PORT` constants** in
  `src/config/constants.rs`; `ServerConfig::default()` uses them instead of string literals

### Fixed
- Scheduling stubs (`queue.rs`, `scheduler.rs`) now return explicit errors instead of silent
  `Ok(fake_value)`; callers can detect that work was not performed (closes #8)
- `batch_trainer::train_batch_internal` returns `bail!` instead of hardcoded loss values;
  prevents callers from believing training succeeded when it did not (closes #11)
- Removed `.unwrap()` panic risks in `event_loop.rs`, `dialog_widget.rs`, `memtree.rs`,
  `repl.rs`, `tui/mod.rs`, `factory.rs`; replaced with `.expect("reason")` or `ok_or_else()`
  error propagation (closes #9)
- Char-index cursor rendering in `render_text_input` no longer panics on multi-byte Unicode

### Changed
- Block cursor `█` used consistently in setup wizard persona editor and all dialog custom-input
  fields; insert cursor `│` removed (terminal convention)
- `CLAUDE.md` documents mandatory regression test requirement for every bug fix

## [0.5.1] - 2026-02-22

### Added
- **Context auto-loading**: Finch now automatically discovers and injects `CLAUDE.md` / `FINCH.md`
  project instructions into the system prompt on startup (matching Claude Code behavior)
  - `~/.claude/CLAUDE.md` — user-level Claude Code defaults
  - `~/.finch/FINCH.md` — user-level Finch defaults
  - `CLAUDE.md` / `FINCH.md` walking from filesystem root down to cwd (outermost first)
  - `FINCH.md` supported as a vendor-neutral, tool-agnostic alternative to `CLAUDE.md`
  - New module: `src/context/claude_md.rs` with 6 unit tests

### Changed
- `build_system_prompt(cwd, claude_md)` — added `claude_md: Option<&str>` parameter

## [0.5.0] - 2026-02-22

### Added
- **Unified `[[providers]]` config format**: New `ProviderEntry` tagged enum replaces the legacy
  `[fallback]` / `TeacherEntry` system for configuring cloud providers and local models
  - Supports Claude, OpenAI, Grok, Gemini, Mistral, Groq, and Local as first-class entries
  - Backwards-compatible: old `[[teachers]]` format auto-migrates on load
  - Save always writes the new `[[providers]]` format
- **Darwin finch ASCII bird banner** at REPL startup
- **WorkUnit**: unified animated message UI for one AI generation turn
- **Streaming tool calls**: fixed multi-turn tool execution over SSE
- **Feedback commands** wired into event loop (`/feedback good|bad`)
- **`/metrics` and `/training` REPL commands**
- **~100 new unit tests** across 7 modules
- **Runtime model/teacher switching** (`/model`, `/teacher`) with memory preservation
- **Command autocomplete**, `/compact`, paste sanitization, memtree view mode
- **Edit and Write tools** for in-context file editing
- **Distributed worker network** foundation (`finch worker`, `finch node-info`)
- **Autonomous agent loop** (`finch agent`) with persona, task backlog, and self-reflection
- **Lotus Network device registration** (`finch network register/join/status`)

### Changed
- Replaced ratatui inline viewport with direct crossterm renderer (smoother streaming)
- Raised context window, message limit, and tool iteration cap
- Stronger system prompt with sharper tool descriptions
- Renamed project from Shammah → Darwin Finch

### Fixed
- Grok streaming tool calls
- Correct model name used in status bar (was hardcoded "Qwen2.5-3B")
- Latency tracking and token count estimation for local Qwen generator
- Insert-before architecture restored for scrollback streaming UX
- macOS/Linux build gaps: CoreML refs guarded with `cfg(target_os = "macos")`
- Linux CI runner bumped to ubuntu-24.04 (glibc 2.39) for ONNX Runtime compatibility

> **Note:** Versions 0.3.x and 0.4.x were internal development iterations that were never
> tagged or released. The public release jumped from v0.2.2 directly to v0.5.0.

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

[0.5.1]: https://github.com/darwin-finch/finch/compare/v0.5.0...HEAD
[0.5.0]: https://github.com/darwin-finch/finch/compare/v0.2.2...v0.5.0
[0.2.2]: https://github.com/darwin-finch/finch/compare/v0.2.1...v0.2.2
[0.2.1]: https://github.com/darwin-finch/finch/compare/v0.2.0...v0.2.1
[0.2.0]: https://github.com/darwin-finch/finch/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/darwin-finch/finch/releases/tag/v0.1.0
