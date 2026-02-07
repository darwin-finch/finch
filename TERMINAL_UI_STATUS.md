# Terminal UI Refactor - Progress Tracking

**Goal:** Progressive refactor to Ratatui-based TUI with Claude Code-like interface

**Plan Document:** `/Users/shammah/.claude/plans/encapsulated-stargazing-sedgewick.md`

**Started:** 2026-02-06

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

**Status:** 🔴 Not Started

### Tasks:

- [ ] Update `Cargo.toml`
  - [ ] Add `ratatui = "0.26"`
  - [ ] Add `ansi-to-tui = "3.1"`
  - [ ] Verify crossterm compatibility (already at 0.27)
  - [ ] Run `cargo check` to verify dependencies resolve

- [ ] Create `src/cli/tui/mod.rs`
  - [ ] Define `TuiRenderer` struct
  - [ ] Implement terminal setup (raw mode, alternate screen)
  - [ ] Define layout with `Layout::vertical()`:
    - Chunk 0: Output area (Constraint::Min(10))
    - Chunk 1: Input line (Constraint::Length(1))
    - Chunk 2: Status area (Constraint::Length(3))
  - [ ] Add `render()` method
  - [ ] Add `shutdown()` method (restore terminal)
  - [ ] Add feature flag: `use_tui` (default false)

- [ ] Create `src/cli/tui/output_widget.rs`
  - [ ] Implement `OutputWidget` struct
  - [ ] Implement `Widget` trait for Ratatui
  - [ ] Read messages from `OutputManager`
  - [ ] Convert ANSI codes using `ansi-to-tui`
  - [ ] Handle line wrapping
  - [ ] Add offset tracking for future scrolling

- [ ] Create `src/cli/tui/status_widget.rs`
  - [ ] Implement `StatusWidget` struct
  - [ ] Implement `Widget` trait for Ratatui
  - [ ] Read status lines from `StatusBar`
  - [ ] Support dynamic number of lines (1-5)
  - [ ] Color coding: training (gray), download (cyan), operations (yellow)
  - [ ] Truncate lines to terminal width

- [ ] Update `src/cli/repl.rs`
  - [ ] Add `tui_renderer: Option<TuiRenderer>` field
  - [ ] Add `use_tui: bool` parameter to config
  - [ ] Initialize TUI if `use_tui` enabled
  - [ ] Add `render()` method that calls `tui_renderer.render()`
  - [ ] Keep dual output (stdout + TUI buffer) for testing

- [ ] Update `src/cli/mod.rs`
  - [ ] Export `tui` module

- [ ] Testing Phase 2
  - [ ] Enable `use_tui` flag in config
  - [ ] Verify layout renders correctly
  - [ ] Check output area displays messages
  - [ ] Confirm status area shows multiple lines
  - [ ] Test terminal resizing (Ctrl+Z, fg)
  - [ ] Test fallback to println! when TUI disabled
  - [ ] Compare TUI output vs println output (should match)

- [ ] Commit Phase 2
  - [ ] Review changes
  - [ ] Run `cargo fmt`
  - [ ] Run `cargo clippy`
  - [ ] Update CLAUDE.md with TUI architecture notes
  - [ ] Commit with message: "Phase 2: Add Ratatui rendering (side-by-side)"

---

## Phase 3: Input Integration with Ratatui

**Status:** 🔴 Not Started

### Tasks:

- [ ] Create `src/cli/tui/input_handler.rs`
  - [ ] Implement `TuiInputHandler` wrapper around rustyline
  - [ ] Add `suspend()` and `resume()` methods for TUI coordination
  - [ ] Pattern: suspend TUI → show rustyline → resume TUI
  - [ ] Preserve history functionality
  - [ ] Handle Ctrl+C, Ctrl+D gracefully

- [ ] Update `src/cli/tui/mod.rs`
  - [ ] Add `suspend()` method (leave raw mode, restore cursor)
  - [ ] Add `resume()` method (re-enter raw mode, redraw)
  - [ ] Ensure idempotent (safe to call multiple times)

- [ ] Update `src/cli/repl.rs`
  - [ ] Refactor main loop to use `tokio::select!` pattern:
    ```rust
    loop {
        tokio::select! {
            input = input_handler.read_async() => { /* process */ }
            chunk = api_rx.recv() => { /* update buffer */ }
            _ = render_interval.tick() => { tui.render() }
        }
    }
    ```
  - [ ] Remove `println!()` calls (TUI-only now when enabled)
  - [ ] Update output methods to only write to buffer
  - [ ] Add periodic render (every 100ms)

- [ ] Update `src/cli/menu.rs`
  - [ ] Add TUI suspend/resume around `inquire::Select::new()`
  - [ ] Add TUI suspend/resume around `inquire::MultiSelect::new()`
  - [ ] Add TUI suspend/resume around `inquire::Text::new()`
  - [ ] Test menu appearance (should work normally)

- [ ] Testing Phase 3
  - [ ] Verify input works with TUI rendering
  - [ ] Test typing while streaming response
  - [ ] Test history navigation (Up/Down arrows)
  - [ ] Test tool confirmation menus (inquire)
  - [ ] Test Ctrl+C graceful shutdown
  - [ ] Test Ctrl+D exit
  - [ ] Verify no flickering during updates
  - [ ] Test with long streaming responses

- [ ] Commit Phase 3
  - [ ] Review changes
  - [ ] Run `cargo fmt`
  - [ ] Run `cargo clippy`
  - [ ] Test extensively (main integration point)
  - [ ] Commit with message: "Phase 3: Integrate input with Ratatui"

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
