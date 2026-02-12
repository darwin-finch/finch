# Shammah Development Roadmap

**Last Updated:** 2026-02-11
**Current Version:** 0.3.0

This document provides detailed plans for future Shammah development, organized by priority and phase.

---

## Phase 1: Polish & UX (High Priority)

### 1.0 Tool Confirmation System Fix

**Problem:** Tool confirmation prompts not appearing/working properly
**Security Impact:** HIGH - Tools may execute without user approval
**User Impact:** Critical - Affects trust and control over system actions

**Investigation Steps:**
1. Test tool execution in REPL mode
   - Try: `> Can you read my Cargo.toml?`
   - Expected: Confirmation prompt appears
   - Actual: ?
2. Check if permission system is bypassed
3. Verify prompt rendering in TUI
4. Test daemon mode vs REPL mode

**Implementation Details:**
- Files:
  - `src/tools/permissions.rs` - PermissionManager
  - `src/tools/executor.rs` - ToolExecutor
  - `src/cli/tui/mod.rs` - TUI rendering
- Behavior:
  1. When tool requested, check if pattern approved
  2. If not approved, show prompt with options:
     - Approve once
     - Approve for session
     - Approve pattern (persistent)
     - Deny
  3. Wait for user input
  4. Execute or deny based on response

**Debugging Questions:**
- Are prompts being generated?
- Are they being sent to TUI?
- Is TUI rendering them?
- Is input being captured?
- Are patterns being saved/loaded correctly?

**Test Cases:**
1. First tool use (no patterns) → Should prompt
2. Approved pattern → Should execute without prompt
3. Session pattern → Should work in session only
4. Persistent pattern → Should work across restarts

**Complexity:** Medium-High (requires debugging existing system)
**Estimated Effort:** 4-8 hours
**Priority:** CRITICAL - Security and user trust

---

### 1.1 Textarea Improvements

#### 1.1.1 Shift-Return Multi-Line Input Support

**Problem:** Users cannot expand input to multiple lines naturally
**Expected Behavior:** Shift+Return creates new line, Return submits
**Impact:** High - Basic UX expectation from modern REPLs

**Implementation Details:**
- File: `src/cli/tui/input_widget.rs`
- Current: tui-textarea widget used for input
- Need: Configure textarea to handle Shift+Return differently from Return
- API: Check tui-textarea documentation for multi-line handling
- Edge cases: Handle submission with trailing newlines

**Complexity:** Low-Medium
**Estimated Effort:** 2-4 hours
**Dependencies:** None

---

#### 1.1.2 Command History Navigation

**Problem:** Users cannot recall previous queries with up/down arrows
**Expected Behavior:** Up arrow shows previous query, down shows next
**Impact:** High - Essential REPL feature

**Implementation Details:**
- File: `src/cli/tui/input_widget.rs`
- Current: No history tracking
- Need:
  1. Add `Vec<String>` to store command history
  2. Add `history_index: usize` to track position
  3. Handle Up/Down key events
  4. Restore current input when navigating away and back
  5. Handle multi-line history entries
  6. Persist history to disk (`~/.shammah/history.txt`)
  7. Load history on startup (last N entries, e.g., 1000)

**Edge Cases:**
- Multi-line entries (preserve newlines)
- Empty entries (don't add to history)
- Duplicate entries (optional: deduplicate)
- History limit (e.g., 1000 entries max)

**Complexity:** Medium
**Estimated Effort:** 4-8 hours
**Dependencies:** None

**Reference Implementation:** Check `reedline` crate for history handling patterns

---

### 1.1.3 Control-C Query Termination

**Problem:** No way to cancel in-progress queries
**Expected Behavior:** Control-C stops current query and returns to prompt
**Impact:** High - Essential for long-running queries

**Implementation Details:**
- Files:
  - `src/cli/tui/mod.rs` - Capture Control-C signal
  - `src/daemon/client.rs` - Send cancellation to daemon
  - `src/daemon/session.rs` - Cancel ongoing generation
  - `src/providers/*.rs` - Cancel API requests

**Behavior:**
1. User presses Control-C during query
2. TUI captures SIGINT signal
3. If query in progress:
   - Send cancel request to daemon
   - Daemon cancels API request (HTTP abort)
   - Return partial response (if any)
   - Display "Query cancelled by user"
   - Return to prompt
4. If no query in progress:
   - Ignore (already at prompt)

**API Cancellation:**
- For streaming requests: Close SSE connection
- For non-streaming: Abort HTTP request
- Teacher APIs (Claude/GPT-4): Use cancellation endpoints if available
- Local model: Interrupt generation loop

**Edge Cases:**
- Query just completed → Show full response
- Multi-turn tool execution → Cancel current turn only
- Background tasks → Don't cancel (e.g., model loading)

**Signal Handling:**
```rust
use tokio::signal;

async fn handle_sigint() {
    signal::ctrl_c().await.unwrap();
    // Send cancellation
}
```

**Complexity:** Medium
**Estimated Effort:** 3-6 hours
**Dependencies:** None
**Priority:** HIGH - Basic UX expectation

---

### 1.2 Status Bar Enhancements

**Problem:** Status bar exists but shows no live information
**Expected Behavior:** Show tokens, latency, model, provider in real-time
**Impact:** Medium - Helps users understand performance

**Implementation Details:**
- File: `src/cli/tui/status_widget.rs`
- Current: Status bar renders but receives no data
- Need:
  1. Add `StatusBarData` struct with fields:
     - `current_model: Option<String>`
     - `current_provider: Option<String>` (local/Claude/GPT-4/etc.)
     - `last_query_tokens: Option<usize>`
     - `last_query_latency_ms: Option<u64>`
     - `generation_speed_tps: Option<f64>` (tokens per second)
  2. Update `OutputManager` to track these metrics
  3. Pass metrics to `TuiRenderer` via `Arc<RwLock<StatusBarData>>`
  4. Update status bar widget to display formatted metrics

**Display Format:**
```
Model: qwen-2.5-3b (local) | Tokens: 247 | Speed: 42.3 tok/s | Latency: 1.2s
```

**Complexity:** Low-Medium
**Estimated Effort:** 2-4 hours
**Dependencies:** None

---

### 1.3 Daemon Management Subcommands

#### 1.3.1 `shammah daemon stop`

**Problem:** No way to gracefully stop daemon from CLI
**Current Workaround:** Users run `kill $(cat ~/.shammah/daemon.pid)`
**Impact:** Medium - Quality of life improvement

**Implementation Details:**
- File: `src/cli/commands.rs`
- Behavior:
  1. Read PID from `~/.shammah/daemon.pid`
  2. Send SIGTERM to daemon process
  3. Wait up to 5 seconds for graceful shutdown
  4. If still running, send SIGKILL
  5. Remove PID file
  6. Print success/error message

**Error Handling:**
- PID file doesn't exist → "Daemon not running"
- PID file exists but process dead → Remove stale PID file
- Permission denied → "Cannot stop daemon (permission denied)"

**Complexity:** Low
**Estimated Effort:** 1-2 hours
**Dependencies:** None

---

#### 1.3.2 `shammah daemon start`

**Problem:** No explicit start command (only auto-spawn)
**Use Case:** Users want to pre-start daemon before making queries
**Impact:** Low - Nice to have, auto-spawn works fine

**Implementation Details:**
- File: `src/cli/commands.rs`
- Behavior:
  1. Check if daemon already running (read PID file)
  2. If running, print "Daemon already running (PID: X)"
  3. If not, spawn daemon explicitly
  4. Wait for health check to succeed
  5. Print "Daemon started (PID: X)"

**Complexity:** Low
**Estimated Effort:** 1-2 hours
**Dependencies:** None

---

#### 1.3.3 `shammah daemon status`

**Problem:** No way to check daemon state from CLI
**Expected Output:** Running/stopped, PID, uptime, active sessions
**Impact:** Medium - Useful for debugging

**Implementation Details:**
- File: `src/cli/commands.rs`
- Behavior:
  1. Check if daemon running (PID file + process exists)
  2. If running:
     - Fetch `/health` endpoint
     - Parse uptime, active sessions, model status
     - Display formatted output
  3. If not running:
     - Print "Daemon not running"

**Example Output:**
```
Daemon Status: Running (PID: 12345)
Uptime: 2h 34m
Active Sessions: 3
Model: qwen-2.5-3b (ready)
Bind Address: 127.0.0.1:8000
```

**Complexity:** Medium
**Estimated Effort:** 2-4 hours
**Dependencies:** Daemon must implement `/health` endpoint (may already exist)

---

## Phase 2: Setup & Configuration (Medium Priority)

### 2.1 Multi-Provider Setup Wizard

**Problem:** Setup wizard only configures Claude
**Impact:** Medium - Users manually edit config for other providers

**Implementation Details:**
- File: `src/cli/setup.rs`
- Current: Interactive prompts for Anthropic API key only
- Need:
  1. After Claude setup, ask "Configure additional providers? (y/n)"
  2. Show menu:
     ```
     Available providers:
     1. GPT-4 (OpenAI)
     2. Gemini (Google)
     3. Grok (xAI)
     4. Skip
     ```
  3. For each selected provider:
     - Prompt for API key
     - Optionally test API key (make simple request)
     - Save to config
  4. Allow configuring multiple providers in one session

**Config Format (already supported):**
```toml
[providers.claude]
api_key = "sk-..."

[providers.openai]
api_key = "sk-..."

[providers.gemini]
api_key = "..."
```

**Complexity:** Medium
**Estimated Effort:** 3-6 hours
**Dependencies:** None (provider adapters already exist)

---

### 2.2 Multi-Model Setup Wizard

**Problem:** Wizard doesn't let users choose models
**Current Behavior:** Auto-selects based on RAM
**Impact:** Low-Medium - Power users want control

**Implementation Details:**
- File: `src/cli/setup.rs`
- Current: Model selection happens automatically
- Need:
  1. After provider setup, ask "Configure local model? (y/n)"
  2. Detect system RAM, show recommended model
  3. Show menu:
     ```
     System RAM: 16GB
     Recommended: Qwen-2.5-3B (3GB RAM, balanced)

     Available models:
     1. Qwen-2.5-1.5B (1.5GB RAM, fast) [smaller than recommended]
     2. Qwen-2.5-3B (3GB RAM, balanced) [RECOMMENDED]
     3. Qwen-2.5-7B (7GB RAM, powerful)
     4. Qwen-2.5-14B (14GB RAM, maximum) [exceeds RAM]
     5. Skip (use teacher only)
     ```
  4. Allow user to override recommendation
  5. Warn if selection exceeds available RAM
  6. Offer to download model immediately or defer

**Complexity:** Medium
**Estimated Effort:** 4-6 hours
**Dependencies:** None

---

### 2.3 Mistral Model Support

**Problem:** Mistral models not tested with existing LlamaAdapter
**Hypothesis:** Should work (Mistral is Llama-like architecture)
**Impact:** Medium - Expands model options

**Implementation Plan:**
1. Research Mistral ONNX models on HuggingFace
   - onnx-community/Mistral-7B-Instruct-v0.3-ONNX
2. Create integration test:
   - File: `tests/mistral_integration_test.rs`
   - Load Mistral model with LlamaAdapter
   - Test generation, tool use
3. If works: Document in README
4. If fails: Create `MistralAdapter` with fixes

**Complexity:** Low-Medium (if LlamaAdapter works) to Medium-High (if new adapter needed)
**Estimated Effort:** 2-8 hours
**Dependencies:** None

---

### 2.4 Additional Model Adapters

**Candidate Models:**
- Phi-3 (Microsoft) - Small, efficient
- DeepSeek-Coder - Code-specialized
- CodeLlama - Meta's code model
- StarCoder - BigCode's model

**Implementation Per Model:**
1. Research ONNX availability on HuggingFace
2. Identify tokenizer format (SentencePiece, BPE, etc.)
3. Create adapter in `src/models/adapters/`
4. Handle model-specific prompt format
5. Test generation and tool use
6. Document in README

**Complexity:** Medium per model
**Estimated Effort:** 4-8 hours per model
**Dependencies:** None

---

## Phase 3: Advanced Features (Lower Priority)

### 3.1 Plan Mode Redesign

**Problem:** Current plan mode is "nearly useless" (user feedback)
**Goal:** Match Claude Code's plan mode quality

**Research Phase:**
1. Study Claude Code's plan mode behavior
2. Identify key features:
   - Multi-step planning with dependencies
   - Approval workflow (approve plan before execution)
   - Step-by-step execution tracking
   - Progress visibility
   - Ability to modify plan mid-execution
3. Document findings in `docs/PLAN_MODE_RESEARCH.md`

**Implementation Plan:**
1. **Plan Creation:**
   - User requests plan mode: `> /plan implement user authentication`
   - Model generates structured plan with steps
   - Steps include: description, dependencies, files, commands

2. **Plan Review:**
   - Display plan in readable format
   - User can approve, modify, or reject
   - Allow editing individual steps

3. **Plan Execution:**
   - Execute steps in dependency order
   - Show progress for each step
   - Allow pausing/resuming
   - Handle failures gracefully

4. **Plan Storage:**
   - Save plans to `~/.shammah/plans/`
   - Allow resuming saved plans
   - Track plan history

**Files to Modify:**
- `src/cli/plan_mode.rs` (major refactor)
- `src/models/plan.rs` (new: plan data structures)
- `src/execution/plan_executor.rs` (new: step execution)

**Complexity:** High
**Estimated Effort:** 20-40 hours
**Dependencies:** Research phase must complete first

---

### 3.2 LoRA Training Integration

#### 3.2.1 Python Dependencies Setup

**Problem:** Python training script exists but dependencies not installed
**Impact:** Medium - Weighted training not functional

**Implementation:**
1. Create virtual environment:
   ```bash
   python -m venv ~/.shammah/venv
   source ~/.shammah/venv/bin/activate
   pip install -r scripts/requirements.txt
   ```
2. Add venv setup to first-run wizard (optional step)
3. Add `shammah train setup` subcommand to install deps
4. Document in README

**Dependencies in requirements.txt:**
- torch
- transformers
- peft (LoRA implementation)
- datasets
- accelerate

**Complexity:** Low
**Estimated Effort:** 1-2 hours
**Dependencies:** None

---

#### 3.2.2 Adapter Loading in Rust Runtime

**Problem:** Training works but adapters not loaded in ONNX runtime
**Challenge:** ONNX Runtime doesn't natively support LoRA adapters

**Options:**

**Option A: Export merged model to ONNX**
- After training, merge LoRA weights into base model
- Export merged model to ONNX
- Swap ONNX model at runtime
- Pro: Works with existing inference code
- Con: Slower switching, larger disk usage

**Option B: Implement LoRA in Rust**
- Load LoRA adapters as separate weights
- Apply LoRA during inference (matrix multiply)
- Pro: Fast adapter switching
- Con: Complex implementation

**Option C: Hybrid (Recommended)**
- Use Option A initially (simpler)
- Migrate to Option B later (optimization)

**Implementation (Option A):**
1. Add merge step to Python training script
2. Export merged model to ONNX
3. Save to `~/.shammah/adapters/name_merged.onnx`
4. Rust: Implement adapter switching command
5. Reload model when adapter changes

**Complexity:** Medium (Option A) to High (Option B)
**Estimated Effort:** 8-16 hours (Option A)
**Dependencies:** Python setup must complete first

---

## Phase 4: Documentation & Cleanup (Ongoing)

### 4.1 Update CLAUDE.md

**Tasks:**
- [ ] Replace Candle references with ONNX Runtime
- [ ] Update architecture diagrams
- [ ] Document ONNX loader implementation
- [ ] Update "Current Project Status" section
- [ ] Fix component descriptions (remove Candle-specific details)

**Files:** `CLAUDE.md`
**Complexity:** Low
**Estimated Effort:** 2-3 hours

---

### 4.2 Create User Guide

**New File:** `docs/USER_GUIDE.md`

**Sections:**
1. **Installation**
   - Prerequisites (Rust, HuggingFace token)
   - Building from source
   - First run experience

2. **Configuration**
   - Setup wizard walkthrough
   - Manual config editing
   - Environment variables

3. **Basic Usage**
   - REPL mode
   - Single query mode
   - Daemon mode

4. **Tool Execution**
   - Tool confirmation workflow
   - Managing patterns
   - Available tools reference

5. **Advanced Features**
   - Model selection
   - Provider fallback
   - LoRA training (when implemented)

6. **Troubleshooting**
   - Common errors and fixes
   - Performance tuning
   - Logging and debugging

**Complexity:** Medium
**Estimated Effort:** 6-10 hours

---

### 4.3 Update ARCHITECTURE.md

**Tasks:**
- [ ] Document daemon architecture
- [ ] Explain client/daemon split
- [ ] Describe tool pass-through mechanism
- [ ] Session management details
- [ ] Add sequence diagrams for:
  - Daemon auto-spawn
  - Tool execution flow
  - Multi-client handling

**Files:** `docs/ARCHITECTURE.md`
**Complexity:** Medium
**Estimated Effort:** 4-6 hours

---

## Timeline Estimates

### This Week (Feb 12-18)
- [x] Documentation cleanup (STATUS.md, archive)
- [ ] **Fix tool confirmation system (1.0) - CRITICAL**
- [ ] **Control-C query termination (1.1.3) - HIGH PRIORITY**
- [ ] Textarea shift-return support (1.1.1)
- [ ] Status bar live stats (1.2)
- [ ] Daemon management commands (1.3)

### This Month (Feb 2026)
- [ ] Command history navigation (1.1.2)
- [ ] Multi-provider setup wizard (2.1)
- [ ] Mistral model testing (2.3)
- [ ] Update CLAUDE.md (4.1)
- [ ] Create USER_GUIDE.md (4.2)

### Next Quarter (Mar-May 2026)
- [ ] Plan mode redesign (3.1)
- [ ] LoRA training integration (3.2)
- [ ] Multi-model setup wizard (2.2)
- [ ] Additional model adapters (2.4)
- [ ] Update ARCHITECTURE.md (4.3)

### Future (Q3 2026+)
- [ ] Quantization support
- [ ] Multi-GPU inference
- [ ] Custom tool framework
- [ ] CoreML export

---

## Contribution Guidelines

If you're working on any of these items:

1. **Create an issue** on GitHub before starting
2. **Reference the roadmap section** (e.g., "Implements 1.1.1 from ROADMAP.md")
3. **Follow the implementation details** in this document
4. **Add tests** for new functionality
5. **Update documentation** as you go
6. **Submit PR** when ready for review

---

## Feedback

Have suggestions for the roadmap? Open an issue with:
- **Feature request** description
- **Use case** (why is this useful?)
- **Priority** (high/medium/low)
- **Estimated complexity** (if known)

We'll review and potentially add to this roadmap.

---

**Last Updated:** 2026-02-11
**Maintained By:** Shammah contributors
