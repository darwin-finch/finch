# CLAUDE.md - AI Assistant Context

This document provides context for AI assistants (like Claude Code) working on the Shammah project.

## Project Context

**Project Name**: Shammah (שָׁמָה - "watchman/guardian")
**Purpose**: Local-first AI coding assistant with continuous improvement
**Core Innovation**: Pre-trained local models + weighted LoRA fine-tuning for domain adaptation
**Supported Models**: Qwen, Llama, Mistral, Phi (via ONNX)
**Teacher Backends**: Claude (Anthropic), GPT-4 (OpenAI), Gemini (Google), Grok (xAI)

### The Problem

Traditional AI coding assistants require:
- Constant internet connection
- High API costs for every query
- No learning from your specific patterns
- Months of training before becoming useful
- Privacy concerns (code sent to cloud)

### The Solution

Shammah provides **immediate quality** with **continuous improvement**:
1. Uses pre-trained local models (works well from day 1)
2. Loads instantly with progressive bootstrap (<100ms startup)
3. Learns from weighted feedback via LoRA fine-tuning
4. Adapts to your coding style, frameworks, and patterns
5. Works offline after initial model download
6. Preserves privacy (code stays on your machine)
7. Falls back to teacher models (Claude/GPT-4/etc.) when needed

### Key Metrics

- **Startup Time**: <100ms (instant REPL)
- **First-Run Experience**: 0ms blocked (background download)
- **Quality Day 1**: High (pre-trained models)
- **Quality Month 1**: Specialized (LoRA adapted to your domain)
- **System Support**: 8GB to 64GB+ RAM (adaptive model selection)

## Architecture Overview

### Design: Pre-trained Local Models + LoRA Adaptation

Shammah uses **pre-trained local models** with **weighted LoRA fine-tuning** instead of training from scratch:

```
User Request
    ↓
┌─────────────────────────────────────┐
│ Router with Model Check              │
│ - Crisis detection (safety)          │
│ - Local model ready? Use local      │
│ - Model loading? Forward to teacher │
└──────────┬──────────────────────────┘
           │
    Model Ready?
           │
    ├─ NO  → Forward to Teacher API (Claude/GPT-4/Gemini/Grok)
    └─ YES → Continue
           │
           v
    ┌──────────────────────────────────┐
    │ Pre-trained Local Model          │
    │ (Qwen/Llama/Mistral/Phi)        │
    │ (1.5B / 3B / 7B / 14B)          │
    │ + LoRA Adapters                  │
    │   (your learned patterns)        │
    └──────────┬───────────────────────┘
           │
           v
    ┌──────────────────────────────────┐
    │ Response to User                 │
    └──────────┬───────────────────────┘
           │
           v
    User Feedback?
           │
    ├─ 🔴 High-weight (10x)
    ├─ 🟡 Medium-weight (3x)
    └─ 🟢 Normal-weight (1x)
           │
           v
    ┌──────────────────────────────────┐
    │ Background LoRA Fine-Tuning      │
    │ - Collects weighted examples     │
    │ - Trains in batches (non-blocking)│
    │ - Saves adapters incrementally   │
    └──────────────────────────────────┘
```

### Why This Approach?

**Pre-trained Qwen vs. Training from Scratch:**
- ✅ **Immediate quality** - Works well from day 1
- ✅ **No cold start** - No months of data collection
- ✅ **Proven performance** - Qwen models are battle-tested
- ✅ **Broad knowledge** - Trained on diverse coding data

**LoRA vs. Full Fine-Tuning:**
- ✅ **Efficient** - Trains only 0.1-1% of parameters
- ✅ **Fast** - Minutes instead of hours
- ✅ **Low memory** - Works on consumer hardware
- ✅ **Multiple adapters** - Switch between domains easily
- ✅ **No degradation** - Base model quality preserved

**Weighted Examples vs. Uniform Training:**
- ✅ **Prioritize critical feedback** - Strategy errors get 10x weight
- ✅ **Faster adaptation** - Learn from mistakes more quickly
- ✅ **User control** - You decide what's important
- ✅ **Efficient learning** - Focus on what matters

### Core Components

#### 1. **Progressive Bootstrap** (`src/models/bootstrap.rs`)

**Purpose:** Instant startup with background model loading

**GeneratorState:**
- `Initializing` - Selecting model based on RAM
- `Downloading` - Downloading from HuggingFace Hub (first run)
- `Loading` - Loading weights into memory
- `Ready` - Model ready for use
- `Failed` - Load failed with error
- `NotAvailable` - Offline mode

**Bootstrap Flow:**
```rust
1. REPL appears instantly (<100ms)
2. Background task spawned
3. Check cache (HF Hub: ~/.cache/huggingface/)
4. Download if needed (with progress)
5. Load model weights
6. Update state to Ready
7. Future queries use local
```

**Key Files:**
- `src/models/bootstrap.rs` - BootstrapLoader, GeneratorState
- `src/models/download.rs` - ModelDownloader with HF Hub integration
- `src/models/model_selector.rs` - RAM-based model selection

#### 2. **ONNX Model Integration** (`src/models/loaders/onnx.rs`)

**Purpose:** Load pre-trained models in ONNX format with KV cache support

**Model Selection:**
- 8GB Mac → Qwen-2.5-1.5B (1.5GB RAM, fast)
- 16GB Mac → Qwen-2.5-3B (3GB RAM, balanced)
- 32GB Mac → Qwen-2.5-7B (7GB RAM, powerful)
- 64GB+ Mac → Qwen-2.5-14B (14GB RAM, maximum)

**Features:**
- Uses ONNX Runtime with CoreML execution provider
- Full KV cache support (56+ dynamic inputs for 28 layers)
- Autoregressive generation with cache reuse
- Metal acceleration on Apple Silicon via CoreML
- Graceful CPU fallback
- Automatic tokenizer loading (tokenizer.json)

**Key Files:**
- `src/models/loaders/onnx.rs` - OnnxLoader, LoadedOnnxModel, KV cache
- `src/generators/qwen.rs` - QwenGenerator with multi-turn tool execution
- `src/models/adapters/qwen.rs` - QwenAdapter with output cleaning
- `src/models/loaders/onnx_config.rs` - Configuration types

#### 3. **LoRA Fine-Tuning** (`src/models/lora.rs`)

**Purpose:** Efficient domain-specific adaptation with weighted examples

**LoRAConfig:**
```rust
pub struct LoRAConfig {
    rank: usize,              // Low-rank dimension (4-64)
    alpha: f64,               // Scaling factor (1.0-32.0)
    dropout: f64,             // Regularization (0.0-0.3)
    target_modules: Vec<String>, // Layers to adapt

    // Weighted training
    high_weight: f64,         // Critical issues (10x)
    medium_weight: f64,       // Improvements (3x)
    normal_weight: f64,       // Good examples (1x)
}
```

**Weighted Training:**
- **High-weight (10x)**: Critical strategy errors
  - Example: "Never use .unwrap() in production"
  - Example: "This algorithm is O(n²), use O(n log n)"
  - Impact: Model strongly learns to avoid this

- **Medium-weight (3x)**: Style preferences
  - Example: "Prefer iterator chains over manual loops"
  - Example: "Use library X instead of library Y"
  - Impact: Model learns your preferred approach

- **Normal-weight (1x)**: Good examples
  - Example: "This is exactly right"
  - Example: "Remember this pattern"
  - Impact: Model learns normally

**Training Flow:**
```rust
1. User provides feedback with weight
2. Example stored in training buffer
3. Buffer reaches threshold (e.g., 10 examples)
4. Background training triggered (non-blocking)
5. LoRA adapter trained for N epochs
6. Adapter saved to ~/.finch/adapters/
7. Adapter loaded for future queries
8. Process repeats continuously
```

**Key Files:**
- `src/models/lora.rs` - LoRAAdapter, LoRAConfig, weighted training
- `src/models/generator_new.rs` - fine_tune(), save_lora(), load_lora()

#### 4. **Router with Graceful Degradation** (`src/router/decision.rs`)

**Purpose:** Decide when to use local vs. Claude, handle model loading

**ForwardReasons:**
- `Crisis` - Safety issue detected
- `ModelNotReady` - Model still loading (progressive bootstrap)
- `NoMatch` - No local pattern match
- `LowConfidence` - Threshold router uncertain

**New Method:**
```rust
fn route_with_generator_check(
    query: &str,
    generator_is_ready: bool,
) -> RouteDecision
```

**Behavior:**
- Checks if generator loaded before considering local
- Forwards to Claude gracefully during bootstrap
- Enables seamless transition: Claude → local
- No blocking or errors during model load

**Key Files:**
- `src/router/decision.rs` - Router, RouteDecision, route_with_generator_check()
- `src/router/hybrid_router.rs` - Hybrid threshold + neural routing

#### 5. **TUI Renderer System** (`src/cli/tui/`)

**Purpose:** Professional terminal UI with scrollback, streaming, and efficient updates

**Architecture:**

The TUI uses a dual-layer rendering system:
1. **Terminal Scrollback** (permanent, scrollable with Shift+PgUp)
   - Written via `insert_before()` for new messages
   - Pushes content above the inline viewport
   - Preserves full history (scrollable by user)

2. **Inline Viewport** (6 lines at bottom, double-buffered)
   - Separator line (visual boundary)
   - Input area (4 lines, tui-textarea)
   - Status bar (1 line, model/token info)

**Key Innovation: Immediate Scrollback with Efficient Updates**

Traditional approach (wrong):
```
New message → Wait for "Complete" status → Write to scrollback
Problem: Streaming messages never appear in scrollback
```

Shammah's approach (correct):
```
New message → Write to scrollback immediately via insert_before()
Message updates → Diff-based blitting to visible area only
```

**Flow Diagram:**

```
User Query / Response Update
    ↓
OutputManager has messages
    ↓
┌─────────────────────────────────────┐
│ flush_output_safe()                  │
└─────────────────────────────────────┘
    ↓
Check: msg in scrollback?
    │
    ├─ NO (NEW MESSAGE)
    │   ↓
    │   Add to ScrollbackBuffer
    │   ↓
    │   insert_before() writes to terminal scrollback
    │   (pushes content above viewport)
    │   (permanent, scrollable with Shift+PgUp)
    │   ↓
    │   Wraps long lines at terminal width
    │   Preserves ANSI color codes
    │
    └─ YES (UPDATE MESSAGE)
        ↓
        Message already in scrollback
        Updates via Arc<RwLock<>>
        (shadow buffer sees changes automatically)
    │
    └───┬───┘
        ↓
┌─────────────────────────────────────┐
│ blit_visible_area()                  │
│ (diff-based updates to visible area) │
└─────────────────────────────────────┘
    ↓
Render messages to shadow_buffer
(2D char array with proper wrapping)
    ↓
diff_buffers(current, prev_frame)
(find changed cells)
    ↓
Group changes by row
    ↓
Clear and rewrite changed rows only
(BeginSynchronizedUpdate for tear-free)
    ↓
Update prev_frame_buffer
```

**Shadow Buffer System:**

The shadow buffer is a 2D character array that handles:
- Proper text wrapping at terminal width
- ANSI escape code preservation (zero-width)
- Diff-based rendering (only changed cells)
- Bottom-aligned content (recent messages visible)

```rust
// Shadow buffer structure
pub struct ShadowBuffer {
    cells: Vec<Vec<Cell>>,  // [y][x]
    width: usize,           // Terminal width
    height: usize,          // Visible scrollback rows
}

pub struct Cell {
    ch: char,               // Visible character
    style: Style,           // Ratatui style (colors, etc.)
}
```

**Key Methods:**

1. **flush_output_safe()** - Main entry point
   ```rust
   // Check if message is NEW or UPDATE
   if self.scrollback.get_message(msg_id).is_none() {
       // NEW: Write to scrollback via insert_before()
       self.scrollback.add_message(msg.clone());
       new_messages.push(msg.clone());
   }
   // UPDATE: Already in scrollback, Arc<RwLock<>> propagates changes

   // Write new messages to terminal scrollback
   if !new_messages.is_empty() {
       self.terminal.insert_before(num_lines, |buf| {
           // Write wrapped lines above viewport
       })?;
   }

   // Blit updates to visible area
   if !messages.is_empty() {
       self.blit_visible_area()?;
   }
   ```

2. **blit_visible_area()** - Diff-based updates
   ```rust
   // Render all messages to shadow buffer
   self.shadow_buffer.render_messages(&all_messages);

   // Find changes since last frame
   let changes = diff_buffers(&self.shadow_buffer, &self.prev_frame_buffer);

   if changes.is_empty() {
       return Ok(()); // No changes
   }

   // Group by row for efficient clearing
   let mut changes_by_row: HashMap<usize, Vec<(usize, char)>> = HashMap::new();

   // Apply changes (clear + rewrite changed rows)
   for (row, _cells) in changes_by_row {
       execute!(stdout, cursor::MoveTo(0, row), Clear(ClearType::UntilNewLine))?;
       execute!(stdout, cursor::MoveTo(0, row), Print(line_content))?;
   }

   // Update previous frame buffer
   self.prev_frame_buffer = self.shadow_buffer.clone_buffer();
   ```

3. **render_messages()** (shadow_buffer.rs) - Message → 2D array
   ```rust
   // Format all messages
   let mut all_lines: Vec<String> = Vec::new();
   for msg in messages {
       let formatted = msg.format();
       for line in formatted.lines() {
           all_lines.push(line.to_string());
       }
   }

   // Calculate wrapping (visible length excludes ANSI codes)
   for line in &all_lines {
       let visible_len = visible_length(line);
       let rows = (visible_len + width - 1) / width.max(1);
       // ...
   }

   // Bottom-align (recent messages visible)
   let start_row = height.saturating_sub(accumulated_rows);

   // Write wrapped chunks to 2D buffer
   for line in lines_to_render {
       let rows_consumed = self.write_line(current_y, line);
       current_y += rows_consumed;
   }
   ```

4. **visible_length() / extract_visible_chars()** - ANSI handling
   ```rust
   // Strip ANSI escape codes for accurate width calculation
   pub fn visible_length(s: &str) -> usize {
       let mut len = 0;
       let mut chars = s.chars().peekable();

       while let Some(c) = chars.next() {
           match c {
               '\x1b' => {
                   // Skip CSI sequences (\x1b[...m)
                   // Skip OSC sequences (\x1b]...\x07)
               }
               _ => len += 1,
           }
       }
       len
   }
   ```

**Architecture Principles:**

1. ✅ **insert_before() = New messages only**
   - Called once per message when added to ScrollbackBuffer
   - Writes to terminal scrollback (permanent, scrollable)
   - Check: `scrollback.get_message(msg_id).is_none()`

2. ✅ **Shadow buffer + blitting = Updates only**
   - Handles changes to existing messages efficiently
   - Diff-based updates (only changed cells)
   - Messages update via Arc<RwLock<>>, shadow buffer sees changes automatically

3. ✅ **No "complete vs incomplete" distinction**
   - ALL messages go to scrollback immediately
   - Status doesn't affect scrollback writing
   - Users can scroll up during streaming responses

4. ✅ **ScrollbackBuffer prevents duplicates**
   - Each message written exactly once via `get_message()` check
   - No need for separate tracking (e.g., `written_message_ids`)

5. ✅ **Proper wrapping and ANSI handling**
   - Long lines wrap cleanly at terminal width
   - ANSI color codes preserved (zero-width)
   - No truncation or text bleeding

**Benefits:**

- **Immediate scrollback**: ALL messages appear in scrollback immediately (not after completion)
- **Efficient updates**: Diff-based blitting (only changed cells updated)
- **Full history**: Users can scroll up during streaming (Shift+PgUp)
- **Clean architecture**: Simple separation (insert_before = new, blitting = updates)
- **Professional UX**: No text ghosting, proper wrapping, synchronized updates

**Key Files:**
- `src/cli/tui/mod.rs` - TuiRenderer, flush_output_safe(), blit_visible_area()
- `src/cli/tui/shadow_buffer.rs` - ShadowBuffer, diff_buffers(), visible_length()
- `src/cli/tui/scrollback.rs` - ScrollbackBuffer (message tracking)
- `src/cli/tui/input_widget.rs` - Input area rendering (tui-textarea)
- `src/cli/tui/status_widget.rs` - Status bar rendering

**Implementation Details:**

See `TUI_SCROLLBACK_FIX_COMPLETE.md` for:
- Full implementation details
- Flow diagrams
- Testing procedures
- Architecture verification

#### 6. **Tool Execution System** (`src/tools/`)

**Purpose:** Enable Claude to inspect and modify code

**Tools:**
- `Read` - Read file contents (code, configs, docs)
- `Glob` - Find files by pattern (`**/*.rs`)
- `Grep` - Search with regex (`TODO.*`)
- `WebFetch` - Fetch URLs (documentation, examples)
- `Bash` - Execute commands (tests, build, etc.)
- `Restart` - Self-improvement (modify code, rebuild, restart)

**Features:**
- Multi-turn execution (tools → results → more tools)
- Real-time output visibility
- Infinite loop detection
- Conversation state validation
- Permission system with patterns
- XML-structured results

**Key Files:**
- `src/tools/executor.rs` - ToolExecutor, multi-turn loop
- `src/tools/implementations/` - Individual tool implementations
- `src/tools/permissions.rs` - PermissionManager, approval patterns

#### 7. **Claude Client** (`src/claude/`)

**Purpose:** Forward queries to Claude API, collect training data

**Features:**
- HTTP client with retry logic
- Streaming support (SSE parsing)
- Tool definitions sent with requests
- Logs (query, response) for LoRA training
- Graceful fallback when streaming unavailable

**Key Files:**
- `src/claude/client.rs` - ClaudeClient, send_message(), send_message_stream()
- `src/claude/types.rs` - API request/response types

#### 8. **Context Assembly** (`src/context/`)

**Purpose:** Discover and inject project-level AI instructions into the system prompt at startup, matching Claude Code behavior.

**How It Works:**

On startup, `ClaudeGenerator::new()` calls `collect_claude_md_context(cwd)` which:

1. Reads `~/.claude/CLAUDE.md` — user-level Claude Code defaults
2. Reads `~/.finch/FINCH.md` — user-level Finch defaults
3. Walks from filesystem root down to `cwd`, loading any `CLAUDE.md` then `FINCH.md` found in each directory (outermost first; cwd wins)
4. Joins non-empty sections with `\n\n---\n\n`
5. Injects the result into the system prompt under `## Project Instructions`

**FINCH.md as an Open Standard:**

`FINCH.md` is supported as a vendor-neutral alternative to the Anthropic-specific `CLAUDE.md` name. Teams that want their AI-assistant instructions to work across multiple tools (Finch, Cursor, other assistants) can use `FINCH.md` instead. When both exist in the same directory, both are loaded (`CLAUDE.md` first).

**Example project instruction file (`~/myproject/FINCH.md`):**
```markdown
Always prefer iterator chains over manual loops.
Never use .unwrap() in production code.
Follow the patterns in docs/ARCHITECTURE.md.
```

**Key Files:**
- `src/context/claude_md.rs` - `collect_claude_md_context()`, `read_non_empty()`
- `src/context/mod.rs` - public re-export
- `src/generators/claude.rs` - `build_system_prompt(cwd, claude_md)`, `ClaudeGenerator`

#### 9. **Configuration** (`src/config/`)

**Purpose:** User preferences and API key management

**Config File (`~/.finch/config.toml`) — Unified `[[providers]]` format:**

```toml
# One entry per provider (cloud or local).
# Use `finch setup` to generate this interactively.

[[providers]]
type = "claude"
api_key = "sk-ant-..."
model = "claude-sonnet-4-6"   # optional, overrides default

[[providers]]
type = "grok"
api_key = "xai-..."
model = "grok-code-fast-1"
name = "Grok (coding)"        # optional display name

[[providers]]
type = "local"
inference_provider = "onnx"
execution_target = "coreml"   # "coreml" | "cpu"
model_family = "qwen2"
model_size = "medium"         # "small"=1.5B "medium"=3B "large"=7B "xlarge"=14B
enabled = true

[lora]
rank = 16
alpha = 32.0
learning_rate = 1e-4
batch_size = 4
auto_train = true
auto_train_threshold = 10
high_weight = 10.0
medium_weight = 3.0
normal_weight = 1.0
adapters_dir = "~/.finch/adapters"
```

**Supported `type` values:** `claude`, `openai`, `grok`, `gemini`, `mistral`, `groq`, `local`

**Backwards-compatible:** The old `[[teachers]]` format still loads correctly; the file is
automatically rewritten to `[[providers]]` format on next save.

**Key Files:**
- `src/config/mod.rs` - Config loading, validation, migration
- `src/config/provider.rs` - `ProviderEntry` tagged enum, conversion helpers
- `src/config/settings.rs` - `TeacherEntry` (kept for internal/legacy use)

### Technology Stack

**Language:** Rust
- Memory safety without GC
- High performance
- Excellent Apple Silicon support

**ML Framework:** ONNX Runtime
- Cross-platform inference engine
- CoreML execution provider for Apple Silicon (Metal acceleration)
- CPU fallback for maximum compatibility
- KV cache support for efficient autoregressive generation
- ONNX format (optimized, portable)

**Models:**
- Base: Qwen-2.5-1.5B/3B/7B/14B (ONNX format, pre-trained)
- Source: onnx-community on HuggingFace
- Adapters: LoRA (domain-specific, ~5MB each, via Python training)

**Storage:**
- Models: `~/.cache/huggingface/hub/` (standard HF cache)
- Adapters: `~/.finch/adapters/`
- Config: `~/.finch/config.toml`
- Metrics: `~/.finch/metrics/`
- Daemon: `~/.finch/daemon.pid`, `~/.finch/daemon.sock`

**Dependencies:**
- `ort` - ONNX Runtime bindings (Rust)
- `hf-hub` - HuggingFace Hub integration
- `indicatif` - Progress bars
- `tokenizers` - Tokenization (HF tokenizers crate)
- `tokio` - Async runtime
- `axum` - HTTP server for daemon mode
- `sysinfo` - System RAM detection

## Key Design Decisions

### 1. Pre-trained vs. Training from Scratch

**Decision:** Use pre-trained Qwen models

**Rationale:**
- Immediate quality (works day 1)
- No cold start period (no months waiting for data)
- Proven performance (Qwen is well-tested)
- Broad knowledge base (trained on diverse code)
- LoRA provides domain adaptation without full retraining

**Trade-offs:**
- Pro: Instant value for users
- Pro: No expensive compute for initial training
- Pro: Smaller download than training from scratch
- Con: Slightly larger models than custom-trained ones
- Con: Includes knowledge not specific to user's domain (acceptable)

### 2. Weighted LoRA Training

**Decision:** Allow users to weight training examples

**Rationale:**
- Critical feedback (strategy errors) needs more impact
- Not all examples are equally important
- Faster adaptation to user's specific needs
- User control over what model learns

**Implementation:**
```rust
// High-weight example (10x impact)
lora.add_example(
    query,
    response,
    feedback,
    weight: 10.0,  // Critical issue
);

// This example will be sampled 10x more during training
// Model learns to avoid this pattern strongly
```

**Trade-offs:**
- Pro: Faster learning from critical feedback
- Pro: User control and transparency
- Pro: More efficient training (focus on important patterns)
- Con: Requires user to categorize feedback (worth it)

### 3. Progressive Bootstrap

**Decision:** Instant REPL startup with background model loading

**Rationale:**
- Professional UX (no waiting)
- Users can start querying immediately
- Model downloads don't block
- Graceful degradation (forward to Claude while loading)

**Implementation:**
```rust
// REPL appears instantly
let state = Arc::new(RwLock::new(GeneratorState::Initializing));

// Spawn background task
tokio::spawn(async move {
    loader.load_generator_async().await
});

// User can query immediately
// Routes forward to Claude until model ready
```

**Trade-offs:**
- Pro: 20-50x faster startup (2-5s → <100ms)
- Pro: First-run download doesn't block (5-30min → 0ms)
- Pro: Better user experience
- Con: Slightly more complex state management (acceptable)

### 4. Storage Location

**Decision:** Store everything in `~/.finch/`

**Structure:**
```
~/.finch/
├── config.toml              # User configuration
├── adapters/                # LoRA adapters
│   ├── coding_2026-02-06.safetensors
│   ├── python_async.safetensors
│   └── rust_advanced.safetensors
├── metrics/                 # Training data
│   └── 2026-02-06.jsonl
└── tool_patterns.json       # Approved tool patterns

~/.cache/huggingface/hub/    # Base models (HF standard)
├── models--Qwen--Qwen2.5-1.5B-Instruct/
├── models--Qwen--Qwen2.5-3B-Instruct/
└── models--Qwen--Qwen2.5-7B-Instruct/
```

**Rationale:**
- Simple, single directory for Shammah data
- Standard HF cache for base models (community convention)
- Clear separation: base models vs. adapters
- Easy to backup/share adapters

### 5. Command Name

**Decision:** Use `finch` as the binary name

**Rationale:**
- Distinct from `claude` command
- Meaningful (Hebrew "watchman")
- Easy to type and remember

### 6. Three Operating Modes

**Interactive REPL:**
```bash
finch
> How do I use lifetimes in Rust?
```

**Single Query:**
```bash
finch query "What is 2+2?"
```

**HTTP Daemon:**
```bash
finch daemon --bind 127.0.0.1:8000
```

## Development Guidelines

### Code Style

- **Formatting**: Always use `cargo fmt` before committing
- **Linting**: Run `cargo clippy` and address warnings
- **Documentation**: Doc comments for all public items
- **Error Messages**: User-friendly, actionable
- **Early Exit Pattern**: Prefer early returns for error cases to reduce nesting

**Early Exit Example:**
```rust
// ✅ Preferred: Early exit pattern (less nesting)
fn process_memory(config: &Config) -> Result<()> {
    if !config.memory.enabled {
        eprintln!("Memory system disabled");
        return Ok(());
    }

    // Normal path - no nesting
    let memory = MemorySystem::new(config)?;
    memory.process()?;
    Ok(())
}

// ❌ Avoid: Nested if blocks
fn process_memory(config: &Config) -> Result<()> {
    if config.memory.enabled {
        let memory = MemorySystem::new(config)?;
        memory.process()?;
    }
    Ok(())
}
```

**Benefits:**
- Reduced nesting depth
- Clearer control flow
- Easier to reason about code
- Error cases handled at function top

### Error Handling

```rust
use anyhow::{Context, Result};

fn load_config() -> Result<Config> {
    let path = config_path()
        .context("Failed to determine config path")?;

    let contents = fs::read_to_string(&path)
        .with_context(|| format!("Failed to read config from {}", path.display()))?;

    toml::from_str(&contents)
        .context("Failed to parse config TOML")
}
```

### Testing

**Philosophy**: Write reusable, comprehensive tests - not ad-hoc one-off tests.

**Why**:
- Prevent regressions when refactoring
- Document expected behavior
- Enable confident changes
- Can always remove tests later if not needed
- Ad-hoc manual testing is wasted effort

**Test Types**:

1. **Unit Tests** (`#[cfg(test)]` in module files):
   - Test individual functions in isolation
   - Mock external dependencies
   - Fast, focused, deterministic
   - Example: Command parsing, tool signature generation, schema conversion

2. **Integration Tests** (`tests/*.rs` files):
   - Test cross-module interactions
   - Use real implementations (not mocks)
   - Verify end-to-end workflows
   - Example: Tool execution with permission system, MCP client with real server responses

3. **Documentation Tests** (in doc comments):
   - Verify examples in documentation work
   - Keep docs synchronized with code

**Test Organization**:
```
src/
  cli/
    commands.rs          # Module with #[cfg(test)] mod tests
  tools/
    mcp/
      client.rs          # Module with #[cfg(test)] mod tests
      connection.rs      # Module with #[cfg(test)] mod tests

tests/                   # Integration tests
  mcp_integration_test.rs
  tool_execution_test.rs
  command_integration_test.rs
```

**Test Coverage Goals**:
- All public APIs have tests
- Critical paths have integration tests
- Error handling paths tested
- Edge cases documented with tests

**Running Tests**:
```bash
# All tests
cargo test

# Specific module
cargo test --lib tools::mcp

# Integration tests only
cargo test --test mcp_integration_test

# With output
cargo test -- --nocapture
```

### Logging

```rust
use tracing::{debug, info, warn, error};

#[instrument]
async fn load_model(config: &QwenConfig) -> Result<GeneratorModel> {
    info!("Loading Qwen model");
    debug!(?config, "Model configuration");

    let model = QwenLoader::load(config)
        .context("Failed to load model")?;

    info!("Model loaded successfully");
    Ok(model)
}
```

### Git Workflow

**Commit After:**
- Implementing complete feature
- Fixing a bug
- Adding/updating documentation
- Refactoring (maintains functionality)

**Include in Commit:**
- Code changes
- Test updates
- Documentation updates
- Design document updates (if needed)

**Commit Message Format:**
```
feat: add weighted LoRA training

Enables users to weight training examples (10x/3x/1x) for faster
adaptation to critical feedback patterns.

Changes:
- Add weight parameter to LoRA training API
- Implement weighted sampling in training loop
- Add /feedback high|medium|normal commands
- Update documentation

Co-Authored-By: Claude Sonnet 4.5 <noreply@anthropic.com>
```

### Release Process

**How to publish a new release:**

```bash
# 1. Bump version in Cargo.toml
#    [package] version = "X.Y.Z"

# 2. Commit the version bump
git add Cargo.toml
git commit -m "chore: bump version to vX.Y.Z"

# 3. Tag and push — this triggers the release workflow automatically
git tag vX.Y.Z
git push origin main
git push origin vX.Y.Z
```

The GitHub Actions release workflow (`.github/workflows/release.yml`) will:
- Build `finch-macos-arm64.tar.gz` (macOS Apple Silicon, `macos-14` runner)
- Build `finch-linux-x86_64.tar.gz` (Linux x86_64, `ubuntu-24.04` runner)
- Create the GitHub Release with both binaries attached

**Re-releasing the same tag** (e.g. to fix a broken release before anyone installs it):
```bash
git tag -d vX.Y.Z
git push origin :refs/tags/vX.Y.Z
git tag vX.Y.Z
git push origin vX.Y.Z
```

**Platform notes (as of Feb 2026):**
- Intel macOS (`x86_64-apple-darwin`) is **not supported** — `ort` has no prebuilt binaries for it and GitHub deprecated Intel Mac runners (June 2025)
- Linux runner must be `ubuntu-24.04`+ — the prebuilt ONNX Runtime binary requires glibc 2.38+ (`__isoc23_strtoll` etc.); Ubuntu 22.04 only has glibc 2.35
- `CoreML`/macOS-only deps in `Cargo.toml` must stay **above** `[target.'cfg(target_os = "macos")'.dependencies]` — TOML scopes everything after a section header until the next one

## Current Project Status

**Version**: 0.5.2-dev
**Last updated**: Feb 2026

Core infrastructure is complete and production-ready. The project is a fully functional local-first AI coding assistant with ONNX Runtime inference, multi-turn tool execution, daemon architecture, LoRA fine-tuning infrastructure, and a professional TUI.

### Capabilities Summary

- **Local inference** — ONNX Runtime with Qwen 2.5 (1.5B/3B/7B/14B), Metal acceleration on Apple Silicon
- **6 model families** — Qwen, Llama, Mistral, Gemma, Phi, DeepSeek adapters
- **6 teacher providers** — Claude, GPT-4, Gemini, Grok, Mistral, Groq
- **6 tools** — Read, Glob, Grep, WebFetch, Bash, Restart (with permission system)
- **Daemon** — Auto-spawning, OpenAI-compatible API, tool pass-through, session management
- **TUI** — Scrollback, streaming, ghost text, plan mode, feedback (Ctrl+G/B), history
- **LoRA** — Weighted feedback collection + Python training pipeline (adapter loading pending)
- **Runtime switching** — `/model` and `/teacher` commands mid-session
- **Setup wizard** — Auto-detects API keys, tabbed UI, model preview, ONNX config

### Open Issues

Tracked as GitHub Issues: **https://github.com/darwin-finch/finch/issues**

Key open items:
- [#1](https://github.com/darwin-finch/finch/issues/1) LoRA adapter loading at ONNX runtime (40-80h, complex)
- [#2](https://github.com/darwin-finch/finch/issues/2) Mistral ONNX support (blocked on onnx-community publishing models)
- [#3](https://github.com/darwin-finch/finch/issues/3) Additional model adapters (CodeLlama, Yi, StarCoder)
- [#4](https://github.com/darwin-finch/finch/issues/4) Update ARCHITECTURE.md
- [#5](https://github.com/darwin-finch/finch/issues/5) Integration tests (daemon, LoRA, multi-provider, tool pass-through)
- [#6](https://github.com/darwin-finch/finch/issues/6) Remove unused Candle imports (good first issue)
- [#7](https://github.com/darwin-finch/finch/issues/7) LoRA training memory efficiency
- [#8](https://github.com/darwin-finch/finch/issues/8) src/scheduling/ stubs — not wired to real code
- [#9](https://github.com/darwin-finch/finch/issues/9) .unwrap() panic risks in production paths
- [#10](https://github.com/darwin-finch/finch/issues/10) Hardcoded ports / max_tokens duplicated across files
- [#11](https://github.com/darwin-finch/finch/issues/11) src/training/batch_trainer.rs returns fake loss (no real training)
- [#21](https://github.com/darwin-finch/finch/issues/21) CLAUDE.md/FINCH.md auto-loading — **CLOSED** (implemented in 6353f3b)

## Reference Documents

**Current Documentation:**
- **README.md**: User-facing documentation
- **CLAUDE.md**: This file (AI assistant context)
- **CHANGELOG.md**: Version history
- **docs/ROADMAP.md**: Detailed future work planning
- **docs/ARCHITECTURE.md**: System architecture overview
- **docs/DAEMON_MODE.md**: Daemon architecture details
- **docs/TOOL_CONFIRMATION.md**: Tool permission system
- **docs/TUI_ARCHITECTURE.md**: Terminal UI rendering system
- **docs/MODEL_BACKEND_STATUS.md**: Model backend comparison
- **docs/USER_GUIDE.md**: Setup and usage guide

**Archived Documentation:**
- **docs/archive/**: Completed phase documentation (PHASE_4-8, ONNX migration, tool pass-through)
  - These documents describe completed work and are kept for historical reference

## Questions?

If you're unsure about something:

1. Check this file (CLAUDE.md) for context
2. Check README.md for user perspective
3. Look at existing code for patterns
4. Ask the user if still unclear

## Key Principles

1. **Immediate Quality**: Pre-trained models work day 1
2. **Continuous Improvement**: LoRA fine-tuning adapts to user
3. **User Control**: Weighted feedback, manual overrides
4. **Privacy First**: Local inference, offline capability
5. **Professional UX**: Instant startup, graceful degradation
6. **Rust Best Practices**: Safe, idiomatic, performant code

---

This document evolves with the project. Keep it updated with new design decisions and context that helps AI assistants work effectively on Shammah.
