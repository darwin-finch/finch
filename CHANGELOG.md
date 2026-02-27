# Changelog

All notable changes to Shammah will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.7.19] - 2026-02-27

### Added
- **Brain can propose actions with user approval**: the background brain now has
  a `run_command` tool. When it discovers something actionable while you're
  typing (failing test, missing dependency, stale lockfile), it pops a
  `"Brain wants to run: \`{command}\` / Reason: {reason}"` Yes/No dialog.
  Approved commands run via `sh -c` (30 s timeout); output is folded into the
  brain's context summary. Max brain turns bumped 6 → 8 to accommodate
  action round-trips.

### Fixed
- **Orphaned `tool_result` blocks after context truncation**: when the sliding
  window or token-budget truncation dropped messages from the head of the
  conversation, it could leave a user message containing only `tool_result`
  blocks whose corresponding `tool_use` had been cut. All providers reject this
  with `"unexpected tool_use_id found in tool_result blocks"`, causing every
  subsequent query to fail. Both `apply_sliding_window()` and
  `ProviderRequest::truncate_to_context_limit()` now strip orphaned
  tool-result-only turns (and the assistant reply that follows) from the window
  head after truncation. Regression test added.

### Changed
- **Dead code removed**: deleted Phase 4 Candle-based stubs (`RouterModel`,
  `ValidatorModel`, `ModelEnsemble`) from `src/models/mod.rs` and purged the
  orphaned files (`ensemble.rs`, `router.rs`, `validator.rs`,
  `hybrid_router.rs`, `model_router.rs`) that were unreachable in the module
  tree. Cleaned up downstream references in `batch_trainer.rs` and
  `checkpoint.rs`.
- **Clippy warnings reduced 22 → 12**: added `Default` impls for
  `CandleLoader`, `ThresholdRouter`, `ThresholdValidator`, and `McpClient`;
  replaced `QueryPattern::from_str` inherent method with a proper
  `std::str::FromStr` impl; renamed `from_gemini_response` /
  `from_openai_response` → `parse_response` (wrong self-convention); renamed
  `TextTokenizer::default` → `stub`; added `TokenCallback` / `ForwardOutput`
  type aliases in `onnx.rs`.

## [0.7.17] - 2026-02-26

### Changed
- **Brain now queries memory before exploring the codebase**: when the user
  starts typing, the brain pre-fetches the top-3 relevant memories and injects
  them into its task message so it already knows past decisions, conventions,
  and bug fixes before it reads a single file. Memory disabled → behaviour
  unchanged (`None` passed; zero overhead).

## [0.7.16] - 2026-02-26

### Fixed
- **Memory schema migration**: production DBs created before v0.7.15 had the
  wrong `tree_nodes` primary key (`id AUTOINCREMENT` instead of `node_id`).
  `CREATE TABLE IF NOT EXISTS` silently skipped the broken table, causing every
  MemTree insert to fail — 154 SQL conversations recorded, 0 nodes ever indexed.
  `MemorySystem::new()` now detects the old schema via `pragma_table_info`,
  drops the always-empty stale table, and lets schema.sql recreate it correctly.
  Includes regression test.

## [0.7.15] - 2026-02-26

### Added
- **WorkUnit** (`src/cli/messages/work_unit.rs`): unified message type covering
  the full lifecycle of one AI generation turn — streaming throb animation,
  tool-call sub-rows with live output, and a collapsed completion view.
  Replaces the StreamingResponseMessage + OperationMessage combination.
  45 unit tests covering construction, status transitions, token accumulation,
  row lifecycle, format output, timing, ANSI rendering, thread safety, and
  concurrent updates.
- **Memory quality layer** (`src/memory/quality.rs`): three-tier improvement to
  what gets indexed and how it surfaces:
  - *Filter* — short acks ("ok", "got it"), greetings, and messages under 20
    chars are discarded from the MemTree semantic index (still written to SQL
    history). Noise-free index means retrieval is always signal.
  - *Classify + boost* — memories tagged Critical (decisions, bug insights,
    explicit rules — ×1.4 retrieval boost), High (file paths, code patterns,
    preferences — ×1.2), or Normal (×1.0). `create_memory` tool content is
    always Critical. Score = cosine_similarity × importance_boost.
  - *Extract* — long assistant responses have code fences stripped; prose core
    capped at 300 chars at a sentence boundary.
  - Schema: `importance INTEGER NOT NULL DEFAULT 1` added to `tree_nodes`;
    auto-migration on open for existing databases.
  - 19 new tests covering noise filtering, importance classification, extraction,
    retrieval boost ordering, and Discard-tier exclusion.

### Fixed
- **SQLite FK violations in memory tests** (`src/memory/mod.rs`): `libsqlite3-sys`
  bundles SQLite compiled with `SQLITE_DEFAULT_FOREIGN_KEYS=1`, so FK enforcement
  is ON by default. The root node (id=0) was never persisted, causing every leaf
  insert with `parent_id=0` to fail. Fixed by replacing `save_node_to_db(leaf_id)`
  with `save_all_nodes_to_db()` — writes all nodes sorted by node_id (root first)
  in a single transaction.
- **Stale parent embeddings across restarts**: the old code only persisted the
  newly inserted leaf. `update_parent_aggregation()` modifies all ancestors on
  every insert, but those changes were never written to DB. `save_all_nodes_to_db()`
  persists all of them.
- **WorkUnit body lines not rendered in Complete state**: `format_row_collapsed`
  was intentionally stripping body lines (diffs, bash output, etc.) even after
  completion. Body lines are permanent content, not ephemeral streaming noise —
  they now render correctly in both InProgress and Complete states.

## [0.7.14] - 2026-02-26

### Fixed
- **CRITICAL** — Stale `pending_brain_question_tx` could intercept tool-approval
  dialog results: when the user submits or restarts typing, the pending brain
  question oneshot sender is now explicitly dropped and any open brain question
  dialog is closed before the query runs.
- **CRITICAL** — `handle_brain_question` overwrote the previous pending oneshot
  sender without draining it; the old sender is now dropped before storing the
  new one.
- **MAJOR** — Post-cancel write race in `BrainSession`: added an `AtomicBool`
  cancelled guard set in `cancel()` before firing the `CancellationToken`.
  A stale session whose `run_brain_loop` future finished at the same instant as
  cancellation can no longer overwrite `brain_context` belonging to a newer session.
- **MAJOR** — Brain debounce was a rate-limiter (fire every 300ms while typing)
  causing constant brain restarts during active typing. Changed to a true
  "fire after 300ms of silence" debounce: `TypingStarted` fires exactly once per
  typing burst, when the user pauses.
- **MINOR** — Empty or whitespace-only brain context is no longer injected into
  the query (it would add noise without value).
- Brain task panics are now logged via a `JoinHandle` monitor instead of being
  silently lost.

## [0.7.13] - 2026-02-26

### Fixed
- Brain context was never injected: `cancel_active_brain(false)` on submit now
  preserves the context for `handle_user_input` to read, while `cancel_active_brain(true)`
  on typing restart still discards stale context.

## [0.7.12] - 2026-02-26

### Added
- **Typing spawns a brain** (`src/brain/`): when the user types ≥10 characters, a
  cancellable background agentic loop ("brain") starts immediately — reading and
  searching the codebase with `read`, `glob`, and `grep` tools. By the time the
  user hits Enter, the brain's summary is silently injected as a hidden context
  block into the query, giving the main model a head start.
- **`BrainSession`** (`src/brain/mod.rs`): lightweight agentic loop (max 6 turns)
  backed by a `CancellationToken`; cancelled automatically on submit or when new
  typing starts.
- **`AskUserBrainTool`** (`src/brain/ask_user.rs`): brain tool that sends a
  `ReplEvent::BrainQuestion` event; the event loop shows a TUI dialog and returns
  the answer via a oneshot channel (30s timeout → `"[no answer]"`).
- **`InputEvent` enum** (`src/cli/tui/async_input.rs`): `Submitted(String)` and
  `TypingStarted(String)` — replaces the raw `String` channel so the event loop
  can distinguish submits from mid-composition keystrokes.
- Debounced `TypingStarted` signal (300ms) — fired at most once per 300ms while
  the user is actively editing the input buffer.
- `ReplEvent::BrainQuestion` variant — routes brain clarifying questions through
  the existing TUI dialog infrastructure (`Select` for option lists, `TextInput`
  for free-form answers).

## [0.7.2] - 2026-02-24

### Fixed
- **Dialog "Other" row is now navigable**: arrow keys (↑/↓) and j/k can reach the
  "Other (custom response)" option; pressing Enter on it activates custom text input.
  Previously the option was only reachable via the undiscoverable 'o' hotkey.
- "Other" row renders as a numbered option ("N+1. Other (custom response)") and
  highlights with selection style when focused, consistent with the other options.
- MultiSelect dialogs receive the same navigation fix symmetrically.
- Defensive guard prevents `Selected(N)` from being emitted for out-of-bounds indices
  (e.g. empty options list + Enter).
- **`auto_compact_enabled` now defaults to `false`**: new sessions no longer show the
  "Context left until auto-compact: N%" status line; MemTree + ConversationCompactor
  are the primary continuity mechanism. Existing configs with `auto_compact_enabled = true`
  are unaffected.
- **Removed duplicate "Crafting…" line from status bar**: the status bar no longer
  shows the operation verb during queries — the WorkUnit in the live area already shows
  verb + elapsed time + token counts. The `throb_idx` dead variable and associated
  status-bar-only computation were removed from the streaming path.

## [0.7.1] - 2026-02-24

### Added
- **AskUserQuestion option markdown previews**: add a `markdown` field to any `QuestionOption`
  to show a live code/ASCII/diff preview box when that option is focused in the dialog.
- **AskUserQuestion `annotations` response field**: the selected option's `markdown` is echoed
  back in `annotations` (keyed by question text) so the LLM knows exactly which code preview
  the user approved—not just the label string. Matches the Claude Code AskUserQuestion spec.
- Multi-select support confirmed fully shipped (closes #20).

## [0.7.0] - 2026-02-24

### Added
- **Neural ONNX embeddings for memory** (`src/memory/neural_embedding.rs`): `all-MiniLM-L6-v2`
  sentence transformer (Apache 2.0, ~23MB quantized) runs in-process via ONNX Runtime to
  produce 384-dim L2-normalized embeddings. Semantic similarity replaces word-overlap hashing,
  so memory retrieval finds relevant past context even when phrasing differs. Downloads
  automatically from HuggingFace on first use (respects `HF_TOKEN` / `~/.cache/huggingface/token`);
  falls back to TF-IDF if the model is not yet cached.
- **`MemoryConfig.use_neural_embeddings`** (default `true`) and
  **`MemoryConfig.embedding_cache_dir`**: opt-out of neural embeddings or override the
  cache directory.
- **`MemorySystem::new_async()`**: async constructor that triggers model download before
  constructing, so the first session gets neural embeddings instead of the TF-IDF fallback.
- **`MemTree::new_with_dim(dim)`**: parameterised root-embedding dimension so the tree
  works correctly with both TF-IDF (2048) and neural (384) engines.
- **Subagent `spawn_task` tool** (`src/tools/implementations/spawn.rs`): lets the model
  delegate subtasks to isolated, headless agentic loops. Each subagent has its own
  conversation history, a focused system prompt, and a restricted tool set:
  - `general` — Read, Glob, Grep, Bash, WebFetch
  - `explore` — Read, Glob, Grep (read-only)
  - `researcher` — Read, Glob, Grep, WebFetch
  - `coder` — Read, Glob, Grep, Bash
  - `bash` — Bash only
  Subagents cannot call `spawn_task` recursively. Multiple `spawn_task` calls in one
  model response can be executed in parallel by the executor.
- **`TodoWrite` / `TodoRead` tools + live task list in TUI** (issue #32): the model can
  maintain a persistent in-session task list. `TodoWrite` atomically replaces the list;
  `TodoRead` returns all items as JSON. Active items (in-progress then pending, high priority
  first) are rendered live in the TUI between the spinner and the CWD separator.
  Both tools are auto-approved (pure in-memory, no filesystem side effects).
- **Configurable context-strip depth** (`config.features.memory_context_lines`, default 4,
  range 1–8): depth-sliced MemTree centroid summaries populate the status strip at multiple
  time-window granularities. Adjustable in the setup wizard Settings screen with `◀ N ▶`.
- **Input token count in spinner** (`↑ N.Nk`): the Anthropic SSE `message_start` event
  carries the exact prompt token count; it is now captured and shown in the status bar
  throughout streaming (e.g. `✳ Thinking… (3s · ↑ 1.2k · thinking)`). Degrades gracefully
  for providers that don't emit usage events.
- **Rotating spinner verb**: the 27-word verb list (Analyzing, Brainstorming, Building, …)
  is cycled round-robin per WorkUnit via a global `AtomicUsize` counter — no extra dependency.

### Fixed
- **`AskUserQuestion` "Other" option invisible** (#18): the `◌ Other (custom response)` row
  is now rendered at the bottom of Select/MultiSelect option lists when `allow_custom=true`.
  Help text updated to show `o: Other`. Esc while typing a custom response now exits custom
  input mode instead of cancelling the whole dialog; Ctrl+C still cancels unconditionally.
- **Multi-question dialog shows only first question** (#19): `render_content()` now iterates
  all tabs and renders each question's full text and options simultaneously. Numbered section
  headers (bold = active, dimmed = others) replace the old tab strip; `────` separators
  appear between consecutive questions. Help text updated: "Switch tabs" → "Switch question".
- **`PresentPlan` and `AskUserQuestion` blocked in planning mode**: both tools were missing
  from the allowed-tool list in `is_tool_allowed_in_mode`, causing a hard error when the
  model tried to present its plan or ask a clarifying question.
- **Option+Enter inserts newline** (was broken on macOS): standard VT100 raw mode never sets
  SHIFT for Enter; Option+Enter arrives as `KeyModifiers::ALT`. Modifier check changed from
  `contains(SHIFT)` to `intersects(SHIFT | ALT)` in both the async and blocking input paths.
- **Status strip always visible**: conversation topic/focus lines are no longer erased when
  the MemTree summary returns `None` — values persist between queries. Strip also updates
  during agentic (tool-calling) turns, not just after final responses.

## [0.6.0] - 2026-02-22

### Added
- **`finch license` command**: activate, view, and remove a commercial license key.
  - `finch license status` — show current license (Noncommercial or Commercial)
  - `finch license activate --key <FINCH-...>` — activate a purchased key (offline Ed25519 validation)
  - `finch license remove` — revert to noncommercial license
  - REPL slash commands: `/license`, `/license status`, `/license activate <key>`, `/license remove`
- **Startup license notice**: non-commercial users see a weekly reminder with a link to
  `https://polar.sh/darwin-finch`. Suppressed for 7 days after each display; not shown
  for commercial licensees.

### Changed
- **License changed from MIT/Apache-2.0 to PolyForm Noncommercial 1.0.0** (source-available).
  Personal, educational, and research use remain free. Commercial use requires a $10/yr key
  from `https://polar.sh/darwin-finch`. `Cargo.toml` license field updated to
  `LicenseRef-PolyForm-Noncommercial-1.0`.

### Fixed
- **`/provider <name>` switching now works in TUI mode.** Previously, the command updated
  `teacher_session` in `repl.rs` but `event_loop.rs` called `claude_gen` directly and never
  read the updated session — so the switch was silently ignored. The active cloud generator is
  now wrapped in `Arc<RwLock<Arc<dyn Generator>>>` (`cloud_gen`) and swapped atomically when
  `/provider` is called. `ModelShow`, `ModelList`, and `ModelSwitch` commands are handled
  explicitly instead of falling through to the "not yet implemented" catch-all.
- **Acronym fix: IMCPD → IMPCPD throughout codebase.** The methodology is *Iterative
  Multi-**P**erspective Code Plan Debugging* — two Ps. All identifiers, file names, and prose
  references corrected: `ImcpdConfig` → `ImpcpdConfig`, `imcpd_methodology.md` →
  `impcpd_methodology.md`, test file renamed, user-visible strings updated.

### Added
- **`AgentServer` multi-provider pool** (`src/server/mod.rs`): holds
  `Vec<Arc<dyn LlmProvider>>` built from the full `[[providers]]` config. Daemon handler
  (`openai_handlers.rs`) selects the provider via `x-finch-provider` request header, falling
  back to the first configured cloud provider (or the legacy `ClaudeClient` when the pool is
  empty). 5 new unit tests in `server::tests`.

### Changed
- **Universal alignment prompt** (`src/providers/alignment.rs`): `UNIVERSAL_ALIGNMENT_PROMPT`
  constant and `with_alignment(system)` helper enforce consistent JSON output, numbered-format
  adherence, and schema fidelity across all LLM providers — the key enabler for safely swapping
  to the cheapest available provider without breaking IMPCPD or other structured-output workflows
- **Live LLM test suite** (`tests/live.rs`, `tests/live/`): opt-in integration tests
  (gated by `FINCH_LIVE_TESTS=1`, all `#[ignore]` by default) that verify real API contracts:
  - `tests/live/providers.rs` — per-provider smoke tests (one per provider, 6 total)
  - `tests/live/parity.rs` — cross-provider behavioral parity (non-empty response, bare JSON
    with alignment, max_tokens respected)
  - `tests/live/impcpd.rs` — IMPCPD JSON schema contract (critique parses to `Vec<CritiqueItem>`,
    plan generates numbered steps, critique parity across all providers)
  - Keys resolved from env vars first (CI), then `~/.finch/config.toml` (local dev)
  - Run: `FINCH_LIVE_TESTS=1 cargo test -- --include-ignored live_`

### Changed
- **IMPCPD plan loop** (`src/planning/loop_runner.rs`): alignment prompt now prepended to both
  `generate_plan` and `critique_plan` prompts, reducing JSON-format failures across non-Claude
  providers

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
  - Weighted feedback collection infrastructure (Ctrl+G/B, JSONL queue) for future LoRA fine-tuning
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

[0.7.0]: https://github.com/darwin-finch/finch/compare/v0.6.0...v0.7.0
[0.6.0]: https://github.com/darwin-finch/finch/compare/v0.5.2...v0.6.0
[0.5.2]: https://github.com/darwin-finch/finch/compare/v0.5.1...v0.5.2
[0.5.1]: https://github.com/darwin-finch/finch/compare/v0.5.0...v0.5.1
[0.5.0]: https://github.com/darwin-finch/finch/compare/v0.2.2...v0.5.0
[0.2.2]: https://github.com/darwin-finch/finch/compare/v0.2.1...v0.2.2
[0.2.1]: https://github.com/darwin-finch/finch/compare/v0.2.0...v0.2.1
[0.2.0]: https://github.com/darwin-finch/finch/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/darwin-finch/finch/releases/tag/v0.1.0
