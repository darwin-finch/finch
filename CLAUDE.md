# CLAUDE.md - AI Assistant Context

This document provides context for AI assistants (like Claude Code) working on the Shammah project.

## Project Context

**Project Name**: Shammah (שָׁמָה - "watchman/guardian")
**Purpose**: Local-first AI coding assistant with continuous improvement
**Core Innovation**: Local ONNX inference across 6 model families, Apple Silicon acceleration via CoreML execution provider, cloud fallback during bootstrap
**Supported Models**: Qwen, Llama, Gemma, Mistral, Phi, DeepSeek (via ONNX)
**Teacher Backends**: Claude (Anthropic), GPT-4 (OpenAI), Gemini (Google), Grok (xAI), Mistral, Groq

### The Problem

Traditional AI coding assistants require:
- Constant internet connection
- High API costs for every query
- No learning from your specific patterns
- Months of training before becoming useful
- Privacy concerns (code sent to cloud)

### The Solution

Shammah provides **immediate quality** at **low cost**:
1. Uses pre-trained local models (works well from day 1)
2. Loads instantly with progressive bootstrap (<100ms startup)
3. Runs queries locally after first model download — near-zero marginal cost
4. Works offline after initial model download
5. Preserves privacy (code stays on your machine)
6. Falls back to cloud providers (Claude/GPT-4/etc.) while the local model is loading
7. LoRA fine-tuning planned — feedback collection infrastructure is in place

### Key Metrics

- **Startup Time**: <100ms (instant REPL)
- **First-Run Experience**: 0ms blocked (background download)
- **Quality Day 1**: High (pre-trained models)
- **Quality Month 1**: Same as day 1 (LoRA adaptation is planned, not yet implemented)
- **System Support**: 8GB to 64GB+ RAM (adaptive model selection)

## Architecture Overview

### Design: Local ONNX Inference with Cloud Bootstrap Fallback

Shammah uses **pre-trained local models** served via ONNX Runtime, with an optional cloud fallback while the model loads for the first time:

```
User Request
    ↓
┌─────────────────────────────────────┐
│ Router — Model Ready Check           │
│  Local model ready? → use local      │
│  Still loading?     → fallback       │
└──────────┬──────────────────────────┘
           │
    Model Ready?
           │
    ├─ NO  → Forward to Teacher API (Claude/GPT-4/Gemini/Grok/Mistral/Groq)
    └─ YES → Continue
           │
           v
    ┌──────────────────────────────────────┐
    │ ONNX Local Model                      │
    │ Qwen · Llama · Gemma · Mistral        │
    │ Phi · DeepSeek                        │
    │ CoreML EP on macOS (ANE/GPU/CPU mix)  │
    │ CUDA/CPU on Linux                     │
    └──────────┬───────────────────────────┘
           │
           v
    ┌──────────────────────────────────┐
    │ Response to User                 │
    └──────────────────────────────────┘
```

**Note:** LoRA fine-tuning (adapting the model to your coding style via feedback) is **planned but not yet implemented**. The feedback collection infrastructure (`Ctrl+G`/`Ctrl+B`, weighted JSONL logging) is in place. Loading LoRA adapters into ONNX Runtime at inference time is the next major milestone (GitHub Issue #1).

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
- Uses ONNX Runtime with pluggable execution providers
- CoreML execution provider on macOS/Apple Silicon — dispatches ops to ANE, GPU, or CPU per-op; in practice LLM workloads run mostly on CPU ARM because CoreML's op set doesn't cover all transformer ops
- CUDA/ROCm/DirectML on Linux/Windows if available; CPU fallback everywhere
- Full KV cache support for autoregressive generation
- Automatic tokenizer loading (tokenizer.json)

**Why ONNX is primary (not Candle):** Candle works well on Linux CPU/CUDA and was the original backend. `candle-metal` (Candle's Metal GPU path on macOS) is missing key ops required by Qwen — specifically layer normalisation kernels and some matmul dimension combinations — causing incorrect output or crashes. There is also a third-party `candle-coreml` crate but it requires models in ANEMLL `.mlpackage` format (completely different from PyTorch/safetensors), is not maintained by HuggingFace, and did not work with Qwen models. ONNX + CoreML EP is the practical path for macOS. ONNX also supports all 6 model families vs. Candle's Qwen2-only support.

**Key Files:**
- `src/models/loaders/onnx.rs` - OnnxLoader, LoadedOnnxModel, KV cache
- `src/models/loaders/candle.rs` - Candle backend (Linux/CPU, Qwen2 only)
- `src/models/loaders/onnx_config.rs` - Configuration types
- `src/models/unified_loader.rs` - Dispatches to ONNX or Candle based on config

#### 3. **Feedback Collection / LoRA Infrastructure** (`src/models/lora.rs`)

**Status: Infrastructure in place, training not yet implemented.**

The feedback collection pipeline is wired and working:
- `Ctrl+G` (good) / `Ctrl+B` (bad) on any response records a weighted example
- Examples are stored to `~/.finch/training_queue.jsonl` as JSONL
- Three weight tiers exist: high (10x), medium (3x), normal (1x)

The `LoRAConfig` and `LoRAAdapter` structs exist in `src/models/lora.rs` as placeholder infrastructure. The `train()` method returns `anyhow::bail!("LoRA fine-tuning not yet implemented")`. Loading adapters into ONNX Runtime at inference time is tracked as **GitHub Issue #1** (40-80h effort).

**What works today:**
- Feedback keypresses logged with weight
- JSONL queue written to `~/.finch/training_queue.jsonl`
- Config fields (`rank`, `alpha`, `learning_rate`, etc.) accepted and stored

**What is not yet implemented:**
- Actual LoRA training
- Adapter saving to `~/.finch/adapters/`
- Adapter loading at ONNX inference time

**Planned LoRA pipeline (for Issue #1):**

*Training step (external tool, not in-process):*
- On macOS: use [MLX](https://github.com/ml-explore/mlx-lm) (Apple's Python ML framework, the community standard for LoRA on Apple Silicon)
- On Linux/CUDA: use PyTorch + PEFT (`peft`, `transformers`)
- Neither Candle Metal (missing ops) nor candle-coreml (wrong model format) is viable for training on macOS

*Inference step (loading the adapter):*
- `onnxruntime-genai` supports loading pre-trained LoRA adapters as `.onnx_adapter` files at inference time via its `Adapters` API
- Adapters trained with MLX/PEFT must first be converted to `.onnx_adapter` format via the Olive toolchain
- This conversion + loading path is what Issue #1 is tracking

**Key Files:**
- `src/models/lora.rs` - LoRAAdapter, LoRAConfig, WeightedExample, ExampleBuffer (all placeholder)
- `src/training/batch_trainer.rs` - Returns fake loss; not wired to real training

#### 4. **Router** (`src/router/decision.rs`)

**Purpose:** Route queries to local model or teacher API based on model readiness.

The primary routing decision is simple: if the local model is ready, use it; if it is still loading (first run / bootstrap), optionally forward to a configured teacher API.

```rust
fn route_with_generator_check(
    query: &str,
    generator_is_ready: bool,
) -> RouteDecision
```

A threshold-based statistics router (`src/models/threshold_router.rs`) also exists and tracks per-category success rates, but the dominant routing decision in practice is the model-ready check above.

**ForwardReasons:**
- `ModelNotReady` - Model still loading; forward to teacher if configured
- `NoMatch` / `LowConfidence` - Threshold router below confidence; forward to teacher

**Key Files:**
- `src/router/decision.rs` - Router, RouteDecision, route_with_generator_check()

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
- `src/config/settings.rs` - `TeacherEntry` (kept for internal/legacy use), `LicenseConfig`, `LicenseType`

#### 10. **License System** (`src/license/`)

**Purpose:** Offline Ed25519 commercial license key validation.

**Key Format:** `FINCH-<base64url(JSON payload)>.<base64url(Ed25519 signature over payload bytes)>`

**Payload JSON:**
```json
{"sub":"user@example.com","name":"Jane Doe","tier":"commercial","iss":"2026-01-15","exp":"2027-01-15"}
```

**Validation flow (offline):**
1. Strip `FINCH-` prefix
2. Split on `.` → payload_b64, sig_b64
3. Decode base64url (error if malformed — never panic)
4. Verify Ed25519 signature using embedded public key
5. Parse JSON payload; check `exp` date against today
6. Return `ParsedLicense` with name, email, expiry

**Config:** `~/.finch/config.toml` `[license]` section — written by `finch license activate`.

**Enforcement:** Honor system — no blocking; startup notice shown weekly to Noncommercial users.

**CLI commands:**
- `finch license status` — show current license type
- `finch license activate --key <FINCH-...>` — validate key and save to config
- `finch license remove` — revert to Noncommercial

**REPL commands:** `/license`, `/license status`, `/license activate <key>`, `/license remove`

**Phase 2 (deferred):** GitHub Sponsors verification (`finch license --github`) requires OAuth App registration (client_id/secret) — not yet implemented.

**Key Files:**
- `src/license/mod.rs` — `validate_key()`, `validate_key_with_vk()`, `ParsedLicense`; 8 unit tests
- `src/config/settings.rs` — `LicenseConfig`, `LicenseType`
- `scripts/issue_license.py` — key signing script (requires `cryptography` pip package)

---

#### Issuing a Commercial License Key

When the user tells you someone has paid, use `scripts/issue_license.py` to sign a key.
Full credentials and step-by-step instructions are in `~/.claude/CLAUDE.md` (private, not in this repo).

**Lotus Network integration path (future):** When a user pays via Lotus Network, the
`checkout.session.completed` webhook can call `issue_key()` (Python logic ported to Rust)
and email the key. Private key injected as an env var in the Lotus deployment.

### Technology Stack

**Language:** Rust
- Memory safety without GC
- High performance
- Excellent Apple Silicon support

**Primary ML Framework:** ONNX Runtime (Microsoft-maintained)
- Cross-platform inference engine; uses ONNX model format (converted from PyTorch)
- On macOS/Apple Silicon: CoreML execution provider dispatches ops to ANE, GPU, or CPU per-op based on CoreML's op coverage. LLM workloads typically run mostly on CPU ARM because many transformer ops (attention patterns, complex reshapes) are not in CoreML's supported op set. There is some ANE/GPU dispatch for ops CoreML does support.
- On Linux: CUDA, ROCm, DirectML if available; CPU fallback everywhere
- KV cache support for efficient autoregressive generation
- Supports all 6 model families (Qwen, Llama, Gemma, Mistral, Phi, DeepSeek) — as ONNX format models from HuggingFace onnx-community org

**Why ONNX over Candle on macOS:**
- `candle-metal` (Candle's Metal GPU path) is missing key ops for Qwen: layer normalisation kernels and certain matmul dimension combinations cause incorrect output or crashes
- `candle-coreml` (third-party `mazhewitt/candle-cormel` crate) requires ANEMLL `.mlpackage` model format — completely different from PyTorch/safetensors — and isn't maintained by HuggingFace; we tried and hit tensor dimension mismatches with Qwen models
- ONNX + CoreML EP is the practical path on macOS despite the partial op coverage
- ONNX also supports all 6 model families vs. Candle's Qwen2-only support

**Alternative backend:** Candle (`src/models/loaders/candle.rs`)
- Works on Linux (CPU, CUDA via candle-cuda)
- Supports Qwen2 only (not all 6 ONNX families)
- macOS: CPU works correctly; Metal backend present but missing layer-norm and matmul ops needed for Qwen generation → unreliable, opt-in only

**Note on Mistral ONNX:** Models exist (from `microsoft/` and `nvidia/` HuggingFace orgs) but `onnx-community` specifically has not published Mistral. Issue #2 is tracking this.

**Models:**
- 6 families: Qwen 2.5, Llama 3, Gemma 2, Mistral, Phi, DeepSeek Coder (ONNX format)
- Source: onnx-community on HuggingFace
- LoRA adapters: planned (see issue #1)

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

### 6. Operating Modes

**Interactive REPL:**
```bash
finch
> How do I use lifetimes in Rust?
```

**Single query / pipe:**
```bash
finch query "What is a Rust lifetime?"
echo "Explain this code" | finch
cat error.log | finch "what went wrong here?"
```

**Background daemon (auto-spawned, OpenAI-compatible API):**

The daemon is spawned automatically in the background when finch starts, or can be run explicitly:
```bash
finch daemon --bind 127.0.0.1:11435
```

The daemon exposes an OpenAI-compatible HTTP API, which means:
- **VS Code extensions** (e.g. Continue.dev) can connect to it as an LLM provider by pointing at `http://localhost:11435`
- **Other tools** that speak the OpenAI API protocol work out of the box
- **Cross-machine use**: run the daemon on a powerful machine (desktop, server) and access it from laptops/thin clients on the same network

**mDNS / Bonjour advertising:**

When enabled, the daemon advertises itself over mDNS (Bonjour) so other devices on the local network can discover it automatically — no manual IP configuration needed. This allows a single machine with a large local model to serve the whole network.

```bash
finch daemon --bind 0.0.0.0:11435 --mdns
# Other machines on LAN see: finch.local:11435
```

**VS Code integration example (Continue.dev config):**
```json
{
  "models": [{
    "title": "Finch (local)",
    "provider": "openai",
    "model": "local",
    "apiBase": "http://localhost:11435",
    "apiKey": "none"
  }]
}
```

**Key files:**
- `src/cli/commands.rs` - Daemon subcommand, bind address, mDNS flag
- `docs/DAEMON_MODE.md` - Full daemon architecture docs

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

**Regression Test Requirement** (mandatory):
- **Every bug fix must have a regression test** that fails before the fix and passes after. No exceptions. This prevents the bug from silently returning during refactoring.
- **Every behavior agreed upon** (e.g., "Candle generates correctly", "ONNX tokenize works") must be covered by a unit test. If a behavior is worth discussing, it is worth testing.
- Tests live in the same file as the code they test (`#[cfg(test)] mod tests { ... }` at the bottom of the module).
- Name regression tests descriptively: `test_<thing>_<behavior>` e.g. `test_candle_qwen_kv_cache_reuses_seqlen_offset`.
- Use mocks (not real models) to test trait contracts and routing logic — real-model tests go under `#[ignore]`.

**Test Coverage Goals**:
- All public APIs have tests
- Critical paths have integration tests
- Error handling paths tested
- Edge cases documented with tests
- Stubs must have tests confirming they return errors (not panic)

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

**Version**: 0.5.2
**Last updated**: Feb 2026

Core infrastructure is complete and production-ready. The project is a fully functional local-first AI coding assistant with ONNX Runtime inference, multi-turn tool execution, daemon architecture, LoRA fine-tuning infrastructure, and a professional TUI.

### Capabilities Summary

- **Local inference** — ONNX Runtime (primary) + Candle (Linux/CPU alt); CoreML EP on macOS (partial ANE/GPU dispatch); CUDA/ROCm on Linux
- **6 model families** — Qwen, Llama, Mistral, Gemma, Phi, DeepSeek adapters
- **6 teacher providers** — Claude, GPT-4, Gemini, Grok, Mistral, Groq
- **6 tools** — Read, Glob, Grep, WebFetch, Bash, Restart (with permission system)
- **Daemon** — Auto-spawning, OpenAI-compatible API, mDNS/Bonjour discovery, VS Code integration, cross-machine local model sharing
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
- [#8](https://github.com/darwin-finch/finch/issues/8) src/scheduling/ stubs — **CLOSED** (return honest errors in 0.5.2)
- [#9](https://github.com/darwin-finch/finch/issues/9) .unwrap() panics — **CLOSED** (replaced in 0.5.2)
- [#10](https://github.com/darwin-finch/finch/issues/10) Hardcoded ports — **CLOSED** (constants in 0.5.2)
- [#11](https://github.com/darwin-finch/finch/issues/11) batch_trainer fake loss — **CLOSED** (honest error in 0.5.2)
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
