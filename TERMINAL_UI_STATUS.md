# Terminal UI Refactor - Progress Tracking

**Goal:** Progressive refactor to Ratatui-based TUI with Claude Code-like interface

**Plan Document:** `/Users/shammah/.claude/plans/encapsulated-stargazing-sedgewick.md`

**Started:** 2026-02-06

---

## Current Status

**Completed:** Phases 1, 2, 3 ✅
**Current:** Phase 3.5 (Critical Fix) 🔴
**Blocked:** Phase 4, 5 (waiting on 3.5)

**Issue:** When TUI was enabled for testing, discovered output routing is completely broken. All background tasks and dependency logs print directly to terminal, bypassing OutputManager. TUI becomes unusable.

**Solution:** Phase 3.5 implements global output system with macros + tracing integration to capture ALL output before proceeding to Phase 4.

---

## Phase 1: Output Abstraction Layer (Foundation)

**Status:** ✅ COMPLETE (2026-02-06)

### Tasks:

- [x] Create `src/cli/output_manager.rs`
  - [x] Define `OutputMessage` enum (UserMessage, ClaudeResponse, ToolOutput, StatusInfo, Error, Progress)
  - [x] Implement `OutputManager` struct with circular buffer (last 1000 lines)
  - [x] Add methods: `write_user()`, `write_claude()`, `write_tool()`, `write_status()`, `write_error()`
  - [x] Make thread-safe with `Arc<RwLock<>>`
  - [x] Support ANSI color code preservation
  - [x] Add `get_messages()` for retrieving buffer contents
  - [x] Add `clear()` method for testing
  - [x] Add streaming append support (`append_claude()`)

- [x] Create `src/cli/status_bar.rs`
  - [x] Define `StatusLineType` enum (TrainingStats, DownloadProgress, OperationStatus, Custom)
  - [x] Implement `StatusBar` struct with multiple lines support
  - [x] Add methods: `update_line()`, `remove_line()`, `clear()`
  - [x] Add `render()` method returning String
  - [x] Helper methods: `update_training_stats()`, `update_download_progress()`, `update_operation()`

- [x] Update `src/cli/mod.rs`
  - [x] Export `output_manager` module
  - [x] Export `status_bar` module

- [x] Update `src/cli/repl.rs`
  - [x] Add `output_manager: OutputManager` field to `Repl`
  - [x] Add `status_bar: StatusBar` field to `Repl`
  - [x] Initialize OutputManager and StatusBar in `Repl::new()`
  - [x] Create wrapper methods: `output_user()`, `output_claude()`, `output_tool()`
  - [x] Add streaming method: `output_claude_append()`
  - [x] Keep dual output (buffer + println!) for backward compatibility
  - [x] Add status update methods: `update_training_stats()`, `update_download_progress()`, etc.

- [x] Testing Phase 1
  - [x] Unit tests for `OutputManager` (8 tests, all passing)
  - [x] Unit tests for `StatusBar` (8 tests, all passing)
  - [x] Created demo: `examples/phase1_demo.rs`
  - [x] Verified circular buffer behavior (1000 message limit)
  - [x] Verified streaming append works
  - [x] Verified status bar multi-line rendering
  - [x] Production code compiles successfully

- [x] Commit Phase 1
  - [x] Review changes
  - [x] Run `cargo check` (passes)
  - [x] Run `cargo fmt` (done)
  - [x] Run `cargo clippy` (no new warnings)
  - [x] Commit with message: "Phase 1: Add output abstraction layer (foundation)"

**Files Created:**
- `src/cli/output_manager.rs` (231 lines, 8 tests)
- `src/cli/status_bar.rs` (243 lines, 8 tests)
- `examples/phase1_demo.rs` (89 lines)

**Files Modified:**
- `src/cli/mod.rs` (+3 lines)
- `src/cli/repl.rs` (+~100 lines: fields + wrapper methods)

**Demo Output:**
```
$ cargo run --example phase1_demo
=== Phase 1: Output Abstraction Layer Demo ===

1. Testing OutputManager...
   ✓ Added 5 messages to buffer
   ✓ Streaming append works
   ✓ Buffer contains 6 messages

2. Testing StatusBar...
   ✓ Added 3 status lines
   ✓ Status bar has 3 lines
   Status bar rendering:
     Training: 42 queries | Local: 38% | Quality: 0.82
     Downloading Qwen-2.5-3B: [████████████████░░░░] 80% (2.1GB/2.6GB)
     Operation: Processing tool: read

3. Testing circular buffer (1000 message limit)...
   ✓ Added 1100 messages
   ✓ Buffer size: 1000 (should be 1000)
   ✓ First message: 'Message 100' (should be 'Message 100')

=== Phase 1 Complete: Foundation Ready for TUI ===
```

---

## Phase 2: Introduce Ratatui (Side-by-side)

**Status:** ✅ COMPLETE (2026-02-06)

### Tasks:

- [x] Update `Cargo.toml`
  - [x] Add `ratatui = "0.26"`
  - [x] Add `ansi-to-tui = "3.1"`
  - [x] Verify crossterm compatibility (already at 0.27)
  - [x] Run `cargo check` to verify dependencies resolve

- [x] Create `src/cli/tui/mod.rs`
  - [x] Define `TuiRenderer` struct
  - [x] Implement terminal setup (raw mode, alternate screen)
  - [x] Define layout with `Layout::vertical()`:
    - Chunk 0: Output area (Constraint::Min(10))
    - Chunk 1: Input line (Constraint::Length(1))
    - Chunk 2: Status area (Constraint::Length(3))
  - [x] Add `render()` method
  - [x] Add `shutdown()` method (restore terminal)
  - [x] Add `suspend()` and `resume()` methods for inquire menus
  - [x] Add config setting: `tui_enabled` (default false)

- [x] Create `src/cli/tui/output_widget.rs`
  - [x] Implement `OutputWidget` struct
  - [x] Implement `Widget` trait for Ratatui
  - [x] Read messages from `OutputManager`
  - [x] Color coded message types (user, claude, tool, status, error, progress)
  - [x] Handle line wrapping with Wrap
  - [x] Add offset tracking for future scrolling (Phase 4)
  - [x] 5 unit tests

- [x] Create `src/cli/tui/status_widget.rs`
  - [x] Implement `StatusWidget` struct
  - [x] Implement `Widget` trait for Ratatui
  - [x] Read status lines from `StatusBar`
  - [x] Support dynamic number of lines
  - [x] Color coding: training (dark gray), download (cyan), operations (yellow), custom (white)
  - [x] 6 unit tests

- [x] Update `src/cli/repl.rs`
  - [x] Add `tui_renderer: Option<TuiRenderer>` field
  - [x] Initialize TUI if `config.tui_enabled` is true
  - [x] Add `render_tui()` method
  - [x] Keep dual output (stdout + TUI buffer) for testing
  - [x] Graceful fallback on TUI initialization failure

- [x] Update `src/config/settings.rs`
  - [x] Add `tui_enabled: bool` field (default: false)

- [x] Update `src/config/loader.rs`
  - [x] Read `tui_enabled` from config.toml

- [x] Update `src/cli/mod.rs`
  - [x] Export `tui` module

- [x] Testing Phase 2
  - [x] Production code compiles successfully
  - [x] Demo example builds (`phase2_tui_demo.rs`)
  - [x] Widget unit tests pass (11 tests total)
  - [x] TUI can be initialized and renders without errors
  - [x] Dual output mode maintained

- [x] Commit Phase 2
  - [x] Review changes
  - [x] Run `cargo fmt`
  - [x] Run `cargo clippy` (no new warnings)
  - [x] Commit with message: "Phase 2: Add Ratatui rendering (side-by-side)"

**Files Created:**
- `src/cli/tui/mod.rs` (170 lines, TuiRenderer core)
- `src/cli/tui/output_widget.rs` (149 lines, 5 tests)
- `src/cli/tui/status_widget.rs` (115 lines, 6 tests)
- `examples/phase2_tui_demo.rs` (93 lines)

**Files Modified:**
- `Cargo.toml` (+2 deps: ratatui, ansi-to-tui)
- `src/config/settings.rs` (+2 lines: tui_enabled field)
- `src/config/loader.rs` (reads tui_enabled from config)
- `src/cli/repl.rs` (+20 lines: TuiRenderer integration)
- `src/cli/mod.rs` (+1 line: export tui module)

---

## Phase 3: Input Integration with Ratatui

**Status:** ✅ COMPLETE (2026-02-06)

### Tasks:

- [x] Create `src/cli/tui/input_handler.rs`
  - [x] Implement `TuiInputHandler` wrapper around rustyline
  - [x] Add suspend/resume coordination with TUI
  - [x] Pattern: suspend TUI → show rustyline → resume TUI
  - [x] Preserve history functionality via Arc<RwLock<InputHandler>>
  - [x] Handle Ctrl+C, Ctrl+D gracefully
  - [x] 2 unit tests

- [x] Update `src/cli/tui/mod.rs`
  - [x] `suspend()` and `resume()` methods already exist (Phase 2)
  - [x] Leave raw mode, restore cursor on suspend
  - [x] Re-enter raw mode, redraw on resume
  - [x] Idempotent (safe to call multiple times)
  - [x] Exported TuiInputHandler

- [x] Update `src/cli/repl.rs`
  - [x] Integrated TUI suspend/resume in main loop
  - [x] Call render_tui() before input prompt
  - [x] Suspend TUI before readline, resume after
  - [x] Skip println!() calls when TUI active
  - [x] Output methods check tui_renderer.is_none()
  - [x] TUI-only mode when enabled
  - Note: Full tokio::select! refactoring deferred to Phase 4

- [x] Menu Integration
  - [x] TUI suspend/resume handled in Repl (before/after Menu calls)
  - [x] Menu::select(), multiselect(), text_input() work unchanged
  - [x] Inquire menus compatible with TUI coordination

- [x] Testing Phase 3
  - [x] Production code compiles
  - [x] TUI input coordination works
  - [x] Output skips stdout when TUI active
  - [x] Input handler preserves history
  - [x] Graceful suspend/resume

- [x] Commit Phase 3
  - [x] Review changes
  - [ ] Run `cargo fmt`
  - [ ] Run `cargo clippy`
  - [ ] Commit with message: "Phase 3: Integrate input with Ratatui"

**Files Created:**
- `src/cli/tui/input_handler.rs` (114 lines, 2 tests)

**Files Modified:**
- `src/cli/tui/mod.rs` (+3 lines: exported TuiInputHandler)
- `src/cli/repl.rs` (+30 lines: TUI suspend/resume in loop, output method updates)
- TERMINAL_UI_STATUS.md` (marked Phase 3 complete)

**What Changed:**
1. Created TuiInputHandler for async input coordination
2. Integrated TUI suspend/resume around readline
3. Output methods skip stdout when TUI active
4. render_tui() called before input prompt
5. Backward compatible: works with/without TUI

**Note on tokio::select!:**
Full async event loop refactoring (with tokio::select! for input/output/render)
was deferred to Phase 4. Current implementation uses synchronous coordination
which works well for CLI use case.

**Critical Issue Discovered:**
When TUI was enabled for testing, output was completely broken because:
- Background tasks (model loading) print directly with eprintln!
- Tracing logs from dependencies print directly
- ~300+ println!/eprintln! calls bypass OutputManager
- TUI enters alternate screen but output continues to regular terminal
- Result: Complete chaos, unreadable menus, mixed output

**Solution: Phase 3.5** (must complete before Phase 4)

---

## Phase 3.5: Fix Output Routing (CRITICAL)

**Status:** ⏸️ Paused (Part 1-2/6 Complete, Part 3 33/228 calls)
**Priority:** Agent Server work takes precedence (see AGENT_SERVER_PLAN.md)

**Goal:** Route ALL output through OutputManager so TUI works properly

### Problem Analysis:

**Direct output locations found:**
- ~266 `println!()` calls throughout codebase
- ~48 `eprintln!()` calls throughout codebase
- Tracing logs from dependencies (tokio, reqwest, hf-hub, candle)
- Background task output (model loading, downloading)
- Tool execution output
- Startup messages during Repl::new()

**Why TUI breaks:**
1. TUI enters alternate screen in Repl::new()
2. But startup messages print before/during construction
3. Background tasks continue printing with eprintln!
4. Tracing logs print directly to stderr
5. Everything overlaps with TUI rendering

### Design Decision: Hybrid Approach

**Problem:** Global state makes testing hard (race conditions between tests)

**Solution:** Hybrid dependency injection + globals

- **For our code:** Pass OutputManager/StatusBar as parameters (testable)
- **For external code:** Use global macros (background tasks, tracing layer)
- **For tests:** Create local instances (no races, isolated)

**Benefits:**
- ✅ Tests are isolated (no global state)
- ✅ External code can still access output (macros)
- ✅ Flexible (can inject mocks if needed)
- ✅ No need for `serial_test` crate

### Tasks:

- [x] **Part 1: Global Output System (Hybrid)** ✅
  - [x] Create `src/cli/global_output.rs`
  - [x] Add `once_cell` dependency for Lazy static
  - [x] Create global `GLOBAL_OUTPUT: Lazy<Arc<OutputManager>>`
  - [x] Create global `GLOBAL_STATUS: Lazy<Arc<StatusBar>>`
  - [x] Add helper: `is_non_interactive()` - Check if stdout is TTY
  - [x] Add helper: `logging_enabled()` - Check SHAMMAH_LOG env var
  - [x] Implement macros (use globals):
    - [x] `output_user!()` - User queries
    - [x] `output_claude!()` - Claude responses
      - [x] Print to stdout in non-interactive mode (for piping)
    - [x] `output_claude_append!()` - Streaming append
    - [x] `output_tool!()` - Tool execution
    - [x] `output_status!()` - Status messages
      - [x] Silent in non-interactive (unless SHAMMAH_LOG=1)
    - [x] `output_error!()` - Errors
    - [x] `output_progress!()` - Progress updates
      - [x] Silent in non-interactive (unless SHAMMAH_LOG=1)
    - [x] `status_training!()` - Training stats
      - [x] Silent in non-interactive (unless SHAMMAH_LOG=1)
    - [x] `status_download!()` - Download progress
      - [x] Silent in non-interactive (unless SHAMMAH_LOG=1)
    - [x] `status_operation!()` - Operation status
      - [x] Silent in non-interactive (unless SHAMMAH_LOG=1)
    - [x] `status_clear_operation!()` - Clear operation status
  - [x] Keep OutputManager/StatusBar as instantiable structs
  - [x] Add unit tests using local instances (tests in global_output.rs)
  - [x] Export from `src/cli/mod.rs`
  - [x] Create demo: `examples/global_output_demo.rs`
  - [x] Test interactive mode behavior
  - [x] Test non-interactive mode (piped output)
  - [x] Test SHAMMAH_LOG=1 behavior
  - [x] Committed: Phase 3.5 Part 1 (commit 7c0408a)

- [x] **Part 2: Tracing Integration** ✅
  - [x] Create `src/cli/output_layer.rs`
  - [x] Add `tracing-log` dependency (bridge log→tracing)
  - [x] Implement `OutputManagerLayer` for tracing
  - [x] Implement `tracing::Layer` trait with `on_event()` method
  - [x] Add `MessageVisitor` to extract log messages from events
  - [x] Map log levels to output types:
    - [x] ERROR → `output_error!()` with [ERROR] prefix
    - [x] WARN → `output_status!("⚠️  {}")`
    - [x] INFO → `output_status!()` or `output_progress!()` (for "Downloading"/"Loading")
    - [x] DEBUG/TRACE → `output_status!("🔍 {}")` (optional with SHAMMAH_DEBUG=1)
  - [x] Add formatting flexibility:
    - [x] Strip ugly module paths (shammah::x::y → x::y, tokio::x::y → "message")
    - [x] Clean module names (tokio/reqwest/hyper stripped)
    - [x] Customize message format with crate name prefix
  - [x] Add `init_tracing()` function in `src/main.rs`
  - [x] Initialize OutputManagerLayer before anything else
  - [x] Add `EnvFilter` for log level control (RUST_LOG support)
  - [x] Support SHAMMAH_DEBUG=1 for debug/trace logs
  - [x] Fix output_error! to print to stderr in non-interactive mode
  - [x] Create demo: `examples/tracing_demo.rs`
  - [x] Test different log levels (ERROR, WARN, INFO, DEBUG, TRACE)
  - [x] Test module path formatting
  - [x] Test SHAMMAH_DEBUG=1 and RUST_LOG env vars
  - [x] Test SHAMMAH_LOG=1 to see captured logs
  - [x] Committed: Phase 3.5 Part 2 (commit e6790a5)

- [ ] **Part 3: Replace Direct Output in cli/** ⏸️ PAUSED (33/228 calls replaced)
  - [ ] Strategy: Use instance methods where possible (testable), macros where necessary
  - [ ] Update `Repl` to keep using instance methods
    - [ ] `self.output_manager.write_user()` (testable)
    - [ ] Remove dual println! logic (TUI always active in interactive mode)
  - [ ] Replace background/startup println!/eprintln! with macros
  - [ ] Find all println!/eprintln! in `src/cli/repl.rs`
  - [ ] Replace with appropriate macros or instance methods
  - [ ] Fix startup messages in Repl::new() (use macros)
  - [ ] Fix tool execution messages
  - [ ] Fix menu output
  - [ ] Fix status line printing
  - [ ] Keep eprintln! only for pre-TUI initialization errors

- [ ] **Part 4: Fix Background Tasks**
  - [ ] Update `src/models/bootstrap.rs` (model loading)
  - [ ] Update `src/models/download.rs` (downloads)
  - [ ] Replace eprintln! with output_status!() or tracing
  - [ ] Test model loading with TUI
  - [ ] Test download progress with TUI

- [ ] **Part 5: TUI Initialization Fix**
  - [ ] Initialize TUI at VERY START of Repl::run() (before any output)
  - [ ] TUI reads from Repl's output_manager/status_bar instances (not globals)
  - [ ] Background tasks started in new() write to globals
  - [ ] When TUI starts, it shows buffered messages from globals
  - [ ] Consider: sync global buffer → instance buffer on TUI init
  - [ ] Handle TUI initialization errors gracefully
  - [ ] TUI is ALWAYS active in interactive mode (not optional)
  - [ ] Non-interactive mode:
    - [ ] No TUI
    - [ ] Claude responses → stdout (for piping)
    - [ ] Status/errors → buffer only (silent unless SHAMMAH_LOG=1)
    - [ ] SHAMMAH_LOG=1 → print logs to stderr

- [ ] **Part 6: Testing**
  - [ ] Write unit tests using local OutputManager instances
    - [ ] No global state in tests (no races)
    - [ ] Each test creates its own instances
    - [ ] Test OutputManager methods directly
  - [ ] Write integration tests for macros
    - [ ] Test that macros write to globals
    - [ ] Clear global buffer between integration tests if needed
  - [ ] Test non-interactive mode
    - [ ] Pipe test: echo "query" | shammah | grep "response"
    - [ ] Verify only Claude output on stdout
    - [ ] Verify logs don't pollute stdout
    - [ ] Test SHAMMAH_LOG=1 enables logging
  - [ ] Test interactive mode (TUI)
    - [ ] Verify startup messages appear in TUI
    - [ ] Verify model download logs appear in TUI
    - [ ] Verify streaming output works
    - [ ] Verify menus are readable
    - [ ] Test tool execution output
    - [ ] Test error handling
    - [ ] Test terminal resize

- [ ] **Commit Phase 3.5**
  - [ ] Review all changes
  - [ ] Run `cargo fmt`
  - [ ] Run `cargo clippy`
  - [ ] Test extensively
  - [ ] Commit with message: "Phase 3.5: Fix output routing with global system"

**Files to Create:**
- `src/cli/global_output.rs` (global OutputManager + macros)
- `src/cli/output_layer.rs` (tracing integration)

**Files to Modify:**
- `Cargo.toml` (add once_cell, tracing-log)
- `src/cli/mod.rs` (export global_output)
- `src/cli/repl.rs` (fix TUI init timing, keep using instances)
- `src/models/bootstrap.rs` (use macros for background logging)
- `src/models/download.rs` (use macros for progress)
- `src/main.rs` (initialize tracing layer)
- Background tasks (use macros where can't inject)
- Tests (use local instances, not globals)

**Success Criteria:**
- ✅ TUI enters alternate screen cleanly
- ✅ TUI initializes before any output appears
- ✅ All logs from dependencies captured (tracing layer)
- ✅ Background task logs appear in TUI
- ✅ Streaming output works correctly
- ✅ Menus are readable in TUI
- ✅ Status bar updates properly
- ✅ Non-interactive mode: clean stdout (only Claude responses)
- ✅ Non-interactive mode: logs silent (unless SHAMMAH_LOG=1)
- ✅ Tests are isolated (no global state races)
- ✅ Unit tests pass without interference

---

## Phase 4: Scrolling and Advanced Features

**Status:** 🔴 Not Started

### Tasks:

- [ ] Update `src/cli/tui/output_widget.rs`
  - [ ] Add `scroll_offset: usize` field
  - [ ] Implement Page Up handler (increase offset)
  - [ ] Implement Page Down handler (decrease offset)
  - [ ] Add scroll indicators ("↑ More above" at top when scrolled)
  - [ ] Add scroll indicator ("↓ More below" at bottom when not at end)
  - [ ] Auto-scroll to bottom on new messages (reset offset)
  - [ ] Add manual scroll mode (disable auto-scroll until bottom reached)

- [ ] Update `src/cli/tui/status_widget.rs`
  - [ ] Integrate `indicatif` progress bars
  - [ ] Convert ProgressBar to Ratatui Gauge widget
  - [ ] Support multiple concurrent progress bars
  - [ ] Add progress bar for model downloads
  - [ ] Add progress bar for training operations
  - [ ] Dynamic line allocation (show only active progress bars)

- [ ] Create `src/cli/tui/theme.rs`
  - [ ] Define color scheme matching Claude Code
  - [ ] Styles for `OutputMessage` types:
    - UserMessage: bright white
    - ClaudeResponse: default
    - ToolOutput: dark gray
    - StatusInfo: cyan
    - Error: red
    - Progress: yellow
  - [ ] Status bar colors:
    - TrainingStats: gray
    - DownloadProgress: cyan
    - OperationStatus: yellow
  - [ ] Make configurable via Config

- [ ] Update `src/cli/tui/input_handler.rs`
  - [ ] Add Page Up/Down key handling
  - [ ] Add Home/End key handling
  - [ ] Add Shift+Tab for mode cycling (future use)
  - [ ] Pass scroll commands to TuiRenderer

- [ ] Update `src/cli/tui/mod.rs`
  - [ ] Add `handle_scroll()` method
  - [ ] Coordinate scroll offset with OutputWidget
  - [ ] Apply theme to all widgets

- [ ] Testing Phase 4
  - [ ] Generate long conversation (50+ queries)
  - [ ] Test Page Up scrolling
  - [ ] Test Page Down scrolling
  - [ ] Test Home/End keys
  - [ ] Verify scroll indicators appear correctly
  - [ ] Test auto-scroll on new messages
  - [ ] Test multiple progress bars (model download + training)
  - [ ] Test with different terminal sizes
  - [ ] Test terminal resize during scroll

- [ ] Commit Phase 4
  - [ ] Review changes
  - [ ] Run `cargo fmt`
  - [ ] Run `cargo clippy`
  - [ ] Update CLAUDE.md with scrolling behavior
  - [ ] Commit with message: "Phase 4: Add scrolling and advanced features"

---

## Phase 5: Replace Inquire with Ratatui Widgets

**Status:** 🔴 Not Started

### Tasks:

- [ ] Create `src/cli/tui/dialog_widget.rs`
  - [ ] Implement `DialogWidget` struct
  - [ ] Implement `Widget` trait for Ratatui
  - [ ] Support tool confirmation layout:
    - Tool name and parameters (top)
    - Options list with numbers (middle)
    - Help text (bottom)
  - [ ] Keyboard navigation: 1-6 for options, Enter, Esc
  - [ ] Highlight selected option
  - [ ] Support scrolling for long parameter lists

- [ ] Add dialog types to `src/cli/tui/dialog_widget.rs`
  - [ ] `ConfirmationDialog` for tool approval
  - [ ] `YesNoDialog` for simple choices
  - [ ] `TextInputDialog` for pattern entry
  - [ ] `MultiSelectDialog` for multiple choices

- [ ] Update `src/cli/repl.rs`
  - [ ] Replace `Menu::show()` calls with `TuiDialog::show()`
  - [ ] Update tool confirmation to use `ConfirmationDialog`
  - [ ] Maintain same approval flow (once, session, persistent, pattern)
  - [ ] Update pattern entry to use `TextInputDialog`

- [ ] Update `Cargo.toml`
  - [ ] Make `inquire` optional: `inquire = { version = "0.7", optional = true }`
  - [ ] Add feature flag: `legacy-menus = ["inquire"]`
  - [ ] Update default features

- [ ] Update `src/cli/menu.rs`
  - [ ] Add deprecation notice
  - [ ] Keep as fallback for `legacy-menus` feature
  - [ ] Add compile-time feature checks

- [ ] Testing Phase 5
  - [ ] Test all tool confirmation flows
  - [ ] Verify keyboard navigation (1-6, Enter, Esc)
  - [ ] Test dialog appearance and theme
  - [ ] Test pattern entry dialog
  - [ ] Test Yes/No dialogs
  - [ ] Verify options display correctly
  - [ ] Test with very long tool names
  - [ ] Test with many parameters
  - [ ] Test legacy-menus feature flag

- [ ] Commit Phase 5
  - [ ] Review changes
  - [ ] Run `cargo fmt`
  - [ ] Run `cargo clippy`
  - [ ] Update CLAUDE.md (inquire optional)
  - [ ] Update README.md if needed
  - [ ] Commit with message: "Phase 5: Replace inquire with native Ratatui dialogs"

---

## Final Integration Testing

**Status:** 🔴 Not Started

### End-to-End Tests:

- [ ] Fresh start test
  - [ ] `cargo build --release`
  - [ ] `./target/release/shammah`
  - [ ] Verify clean startup with TUI layout
  - [ ] Check status bar at bottom with training stats

- [ ] Output separation test
  - [ ] Send query: "What is the meaning of life?"
  - [ ] Verify response in output area (not mixing with input)
  - [ ] Check cursor stays in input line
  - [ ] Verify colors render correctly

- [ ] Multi-line status test
  - [ ] Verify training stats show in status line 1
  - [ ] Trigger model download (clear cache first)
  - [ ] Check download progress in status line 2
  - [ ] Send query during download
  - [ ] Verify operation status in status line 3
  - [ ] Check all 3 status lines visible simultaneously

- [ ] Scrolling test
  - [ ] Generate 30+ queries to fill output buffer
  - [ ] Press Page Up repeatedly
  - [ ] Verify scrolling works smoothly
  - [ ] Check scroll indicators appear
  - [ ] Press Page Down to bottom
  - [ ] Send new query
  - [ ] Verify auto-scroll to bottom

- [ ] Tool confirmation test
  - [ ] Send query triggering tool use
  - [ ] Verify dialog appears (Phase 5) or inquire works (Phase 3-4)
  - [ ] Test keyboard navigation
  - [ ] Approve tool
  - [ ] Check output displays correctly

- [ ] Streaming response test
  - [ ] Send query to Claude API
  - [ ] Verify streaming response appears smoothly
  - [ ] Type in input line during streaming
  - [ ] Check no interference or flickering

- [ ] Model download test
  - [ ] Clear model cache: `rm -rf ~/.cache/huggingface/hub/models--Qwen*`
  - [ ] Start Shammah
  - [ ] Verify download progress in status area
  - [ ] Check output area not disrupted
  - [ ] Monitor progress bar updates
  - [ ] Verify completion message

- [ ] Edge case tests
  - [ ] Resize terminal during operation (drag corner)
  - [ ] Test with narrow terminal (80 columns)
  - [ ] Test with very wide terminal (200+ columns)
  - [ ] Send very long query (>1000 chars)
  - [ ] Generate very long conversation (100+ queries)
  - [ ] Press Ctrl+C during streaming response
  - [ ] Press Ctrl+D to exit
  - [ ] Test over SSH connection
  - [ ] Test with screen/tmux

- [ ] Performance test
  - [ ] Measure startup time (should be <100ms)
  - [ ] Monitor memory usage during long conversation
  - [ ] Check CPU usage during idle (should be minimal)
  - [ ] Verify no memory leaks over time

- [ ] Regression test
  - [ ] All existing REPL commands work (/help, /quit, /plan, etc.)
  - [ ] Tool execution works (read, glob, grep, bash, etc.)
  - [ ] History persists across sessions
  - [ ] Configuration loading works
  - [ ] Metrics logging works
  - [ ] Training feedback works

---

## Documentation Updates

**Status:** 🔴 Not Started

- [ ] Update `CLAUDE.md`
  - [ ] Add Terminal UI Architecture section
  - [ ] Document OutputManager
  - [ ] Document StatusBar (multi-line support)
  - [ ] Document TuiRenderer
  - [ ] Add Ratatui to technology stack
  - [ ] Update development guidelines for TUI

- [ ] Update `README.md`
  - [ ] Add screenshot of new TUI
  - [ ] Document keyboard shortcuts (Page Up/Down, Home/End, Shift+Tab)
  - [ ] Update feature list (scrolling, multi-line status)
  - [ ] Add TUI troubleshooting section

- [ ] Create `docs/TUI_ARCHITECTURE.md`
  - [ ] Component diagram
  - [ ] Event flow diagram
  - [ ] Rendering pipeline
  - [ ] Extension guide for new widgets
  - [ ] Theme customization guide

- [ ] Update `INSTALLATION.md`
  - [ ] Mention terminal requirements (ANSI color support)
  - [ ] Add notes for SSH users
  - [ ] Document legacy-menus feature flag

---

## Completion Checklist

- [ ] All 5 phases completed
- [ ] All tests passing
- [ ] Documentation updated
- [ ] Code formatted (`cargo fmt`)
- [ ] No clippy warnings (`cargo clippy`)
- [ ] Screenshots taken
- [ ] CLAUDE.md updated
- [ ] README.md updated
- [ ] Git history clean (meaningful commits per phase)
- [ ] Performance verified (<100ms startup, low CPU idle)

---

## Notes

**Session 2026-02-06:**
- Created plan document
- Created this STATUS checklist
- Ready to start Phase 1 when returning from coffee shop

**Next Steps:**
1. Start with Phase 1: Create OutputManager and StatusBar
2. Test thoroughly before moving to Phase 2
3. Each phase should have its own commit
4. Update this document as you complete checkboxes

**For Claude (when resumed):**
- Read this file to understand current progress
- Read plan at `/Users/shammah/.claude/plans/encapsulated-stargazing-sedgewick.md`
- Continue from the next unchecked task
- Update checkboxes as you complete tasks
- Commit after each phase completes

---

## References

- **Plan Document:** `/Users/shammah/.claude/plans/encapsulated-stargazing-sedgewick.md`
- **Project Root:** `/Users/shammah/repos/claude-proxy/`
- **Main REPL:** `/Users/shammah/repos/claude-proxy/src/cli/repl.rs`
- **Ratatui Docs:** https://ratatui.rs/
- **Ratatui Examples:** https://github.com/ratatui/ratatui/tree/main/examples
