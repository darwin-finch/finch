# Shammah Self-Improvement Roadmap

This plan can be executed by Shammah working on itself. Each task is designed to be implementable through the existing tool system (Read, Glob, Grep, Bash).

## Phase 1: Critical Fixes (Release Blockers)

### Task 1.1: Fix Streaming Responses
**Priority:** HIGH
**Impact:** UX - users see responses character-by-character in real-time

**Problem:**
- Streaming is hardcoded to `false` in `src/cli/repl.rs:1672`
- Even queries without tools don't stream
- Originally disabled because tool_use detection in SSE stream wasn't implemented

**Solution:**
- Enable streaming for responses without tool uses
- Keep non-streaming path when tools are detected
- Strategy: Use streaming by default, fall back to buffered if tool_use detected in stream

**Implementation:**
1. Modify `src/cli/repl.rs:1672` to enable streaming
2. Update `display_streaming_response()` to detect tool_use events
3. If tool_use detected mid-stream, switch to buffered mode and re-request
4. Add flag to track if stream contained tools

**Files to modify:**
- `src/cli/repl.rs` (lines 1672, 620-650)
- `src/claude/client.rs` (SSE parsing logic)

**Test:**
- Simple query: "What is Rust?" should stream character-by-character
- Tool query: "Read the Cargo.toml file" should work (buffered is fine)

---

### Task 1.2: Integrate Local Generation into Main Loop
**Priority:** HIGH
**Impact:** Core value proposition - 95% local processing

**Problem:**
- LocalGenerator exists but isn't called from main query loop
- All queries still forward to Claude API
- No actual cost savings yet

**Solution:**
- Call `local_generator.try_generate()` before forwarding to Claude
- If confidence >= 0.7, return local response
- If confidence < 0.7, forward to Claude and learn from response
- Update metrics to track local vs forwarded requests

**Implementation:**
1. Add `local_generator: LocalGenerator` to `Repl` struct
2. In `process_query()`, try local generation first
3. Track local_success/local_failure in metrics
4. Learn from Claude responses via `response_generator.learn_from_claude()`

**Files to modify:**
- `src/cli/repl.rs` (lines 147, 1600-1700)
- `src/metrics/mod.rs` (add local generation metrics)

**Test:**
- Query: "hello" should generate locally (greeting pattern)
- Query: "What is quantum computing?" should forward (complex)
- After 5+ forwards, retry "What is quantum computing?" - might generate locally

---

### Task 1.3: Add Integration Tests
**Priority:** MEDIUM
**Impact:** Reliability - prevents regressions

**Problem:**
- Only unit tests exist
- No end-to-end tests of query → response flow
- No tests of tool execution loop
- No tests of local generation integration

**Solution:**
- Create `tests/integration/` directory
- Add tests for full query flow
- Add tests for tool execution
- Add tests for local generation learning

**Implementation:**
1. Create `tests/integration/mod.rs`
2. Test: Query without tools
3. Test: Query with tools (Read, Grep)
4. Test: Local generation (greeting)
5. Test: Learning from Claude response
6. Test: Threshold router statistics update

**Files to create:**
- `tests/integration/query_flow.rs`
- `tests/integration/tool_execution.rs`
- `tests/integration/local_generation.rs`

**Test:**
- Run `cargo test --test integration`
- All integration tests pass

---

## Phase 2: Missing Core Features

### Task 2.1: Add /help Command with Full Documentation
**Priority:** MEDIUM
**Impact:** Discoverability - users learn features

**Problem:**
- `/help` command is basic
- Doesn't document all features (tools, plan mode, patterns, metrics)
- New users don't know what Shammah can do

**Solution:**
- Comprehensive `/help` with sections
- `/help tools` - list and document all tools
- `/help commands` - list all slash commands
- `/help patterns` - explain pattern system
- `/help plan` - explain plan mode

**Implementation:**
1. Update `src/cli/commands.rs` - expand Help command
2. Add help sections: tools, commands, patterns, plan, metrics
3. Make help searchable: `/help tools` shows only tool help

**Files to modify:**
- `src/cli/commands.rs` (Help command handler)

**Test:**
- `/help` shows overview
- `/help tools` shows Read, Glob, Grep, WebFetch, Bash
- `/help plan` explains plan mode workflow

---

### Task 2.2: Add /status Command for System Status
**Priority:** LOW
**Impact:** Transparency - users see what's happening

**Problem:**
- No visibility into system state
- Can't see router/validator statistics
- Can't see local generation performance
- Can't see pattern learning progress

**Solution:**
- Add `/status` command showing:
  - Total queries processed
  - Local vs forwarded ratio
  - Pattern statistics (queries per pattern, success rates)
  - Router confidence trends
  - Validator quality trends
  - Model training progress

**Implementation:**
1. Add Status command to `src/cli/commands.rs`
2. Gather stats from threshold_router, local_generator, metrics_logger
3. Format as nice table with colors

**Files to modify:**
- `src/cli/commands.rs` (new Status command)
- `src/models/threshold_router.rs` (expose stats)
- `src/local/mod.rs` (expose stats)

**Test:**
- `/status` shows current statistics
- After 10 queries, stats update correctly

---

### Task 2.3: Improve Error Messages
**Priority:** LOW
**Impact:** UX - users understand failures

**Problem:**
- Generic error messages
- Stack traces shown to users
- No suggestions for fixing issues

**Solution:**
- User-friendly error messages
- Suggest fixes: "API key not found → set ANTHROPIC_API_KEY or add to config"
- Hide stack traces unless `--debug` flag

**Implementation:**
1. Add error formatting helper in `src/cli/repl.rs`
2. Match common errors (API key, network, rate limit)
3. Provide actionable suggestions
4. Add `--debug` flag to show full errors

**Files to modify:**
- `src/cli/repl.rs` (error display)
- `src/main.rs` (add --debug flag)

**Test:**
- Remove API key, run query → helpful error message
- Network error → "Check internet connection"
- Rate limit → "Wait 60s or upgrade API plan"

---

## Phase 3: Self-Improvement Infrastructure

### Task 3.1: Self-Modification Tool (Restart Tool v2)
**Priority:** LOW (after public release)
**Impact:** Meta - Shammah improves itself

**Problem:**
- Restart tool exists but removed due to security concerns
- No safe way for Shammah to modify its own code
- Can't implement features autonomously

**Solution:**
- Create safe self-modification workflow:
  1. Claude proposes code changes
  2. Show diff to user
  3. User approves/rejects
  4. If approved: apply changes, run tests, build, restart
  5. If tests fail: rollback

**Implementation:**
1. Create `src/tools/implementations/self_modify.rs`
2. Implement safe workflow:
   - Save current binary as backup
   - Apply proposed changes
   - Run `cargo test`
   - If pass: build release, restart
   - If fail: rollback changes, show error
3. Add user confirmation with diff preview

**Files to create:**
- `src/tools/implementations/self_modify.rs`

**Security:**
- ALWAYS show diff before applying
- ALWAYS require user approval
- ALWAYS run tests before restart
- ALWAYS keep backup binary
- Add `--no-self-modify` flag to disable

**Test:**
- Propose simple change (add comment)
- Show diff
- User approves
- Tests pass
- Restart with new code

---

### Task 3.2: Training Pipeline for Neural Networks
**Priority:** MEDIUM (after data collection)
**Impact:** Performance - reach 95% local processing

**Problem:**
- Neural networks exist but not trained
- No training pipeline
- No model evaluation metrics
- Using random weights

**Solution:**
- Implement training pipeline:
  1. Load metrics data from `~/.shammah/metrics/*.jsonl`
  2. Train router model (query → forward/local decision)
  3. Train generator model (query → response, via distillation)
  4. Train validator model (query + response → quality score)
  5. Evaluate on validation set
  6. Save best models

**Implementation:**
1. Create `src/models/training.rs`
2. Implement data loading from JSONL metrics
3. Implement training loop with early stopping
4. Implement evaluation metrics (accuracy, F1, BLEU)
5. Add `shammah train` command to CLI

**Files to create:**
- `src/models/training.rs`
- `src/cli/commands.rs` (Train command)

**Test:**
- Collect 100+ queries
- Run `shammah train`
- Models improve over epochs
- Validation accuracy increases

---

### Task 3.3: A/B Testing Framework
**Priority:** LOW
**Impact:** Optimization - measure improvements

**Problem:**
- No way to measure if changes improve performance
- Can't compare old vs new models
- Can't validate that self-improvements work

**Solution:**
- Add A/B testing framework:
  - Run queries through both old and new models
  - Track which performs better
  - User provides feedback (👍/👎)
  - Statistical significance testing

**Implementation:**
1. Create `src/testing/ab_test.rs`
2. Add `--ab-test` flag to enable
3. For each query, run both models
4. Show both responses, ask user to pick better one
5. Track win rates, confidence intervals

**Files to create:**
- `src/testing/ab_test.rs`
- `src/testing/mod.rs`

**Test:**
- Enable A/B test mode
- Query: "What is Rust?"
- See two responses (old model, new model)
- Pick better one
- Stats update

---

## Phase 4: Advanced Features (Post-Release)

### Task 4.1: Multi-Model Support (GPT-4, Gemini, etc.)
**Priority:** LOW
**Impact:** Flexibility - not locked to Claude

**Solution:**
- Abstract LLM client interface
- Implement adapters for GPT-4, Gemini, Llama
- User chooses provider in config

### Task 4.2: Constitutional AI Editor
**Priority:** LOW
**Impact:** Customization - users define principles

**Solution:**
- Interactive editor for `~/.shammah/constitution.md`
- Built-in command: `/constitution edit`
- Test constitutional adherence

### Task 4.3: Conversation Branching
**Priority:** LOW
**Impact:** Exploration - try different approaches

**Solution:**
- Save conversation checkpoints
- Branch from any point: `/branch`
- List branches: `/branches`
- Switch branches: `/switch <branch>`

### Task 4.4: Tool Permission Profiles
**Priority:** LOW
**Impact:** Security - fine-grained control

**Solution:**
- Predefined profiles: `safe`, `development`, `admin`
- User chooses: `shammah --profile safe`
- Safe profile: only Read, Glob, Grep
- Admin profile: all tools including Bash

---

## Implementation Strategy: Self-Improvement Loop

To have Shammah work on itself:

1. **Start with Task 1.1 (Fix Streaming)**
   ```
   /plan "Fix streaming responses: enable streaming for queries without tools, keep buffered mode for tool queries"
   ```

2. **Once streaming works, tackle Task 1.2 (Integrate Local Generation)**
   ```
   /plan "Integrate LocalGenerator into main query loop: try local generation first, forward if confidence < 0.7, learn from Claude responses"
   ```

3. **Add tests (Task 1.3)**
   ```
   /plan "Create integration tests for: query flow, tool execution, local generation learning"
   ```

4. **Iterate through Phase 2 features**
   - Each feature is a separate `/plan` session
   - Test each feature before moving to next
   - Commit after each working feature

5. **Build self-modification capability (Task 3.1)**
   - This enables fully autonomous improvement
   - Shammah can then tackle remaining tasks independently

---

## Success Metrics

After completing this plan, Shammah should:

- ✅ **Stream responses** for non-tool queries (better UX)
- ✅ **Generate 50%+ queries locally** after 100 queries (cost reduction)
- ✅ **Pass integration tests** (reliability)
- ✅ **Self-modify safely** (meta-improvement)
- ✅ **Train neural networks** from collected data (reach 95% local)
- ✅ **Provide great documentation** (discoverability)
- ✅ **Handle errors gracefully** (professional UX)

---

## Next Immediate Action

**RECOMMENDED: Start with Task 1.1 (Fix Streaming)**

Prompt for Shammah:
```
/plan "Fix streaming responses for queries without tools

Problem: Streaming is hardcoded to false in src/cli/repl.rs:1672, so even simple queries don't stream character-by-character.

Goal: Enable streaming for responses that don't use tools, keep buffered mode for tool queries.

Approach:
1. Change use_streaming logic to default to true
2. Keep tool detection in buffered path working
3. For streaming path, add early exit if tool_use event detected (or just keep using buffered mode for tool queries)

Success criteria:
- Query 'What is Rust?' streams character-by-character
- Query 'Read Cargo.toml' still works with tools (buffered mode is fine)
- No regressions in tool execution"
```

This will improve UX immediately and is a good first self-improvement task.
