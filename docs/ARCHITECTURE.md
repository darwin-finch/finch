# Architecture

This document describes the technical architecture of Shammah, a local-first AI coding assistant that uses pre-trained ONNX models with LoRA fine-tuning.

## Overview

Shammah provides **immediate, high-quality AI assistance** using pre-trained local models (Qwen via ONNX Runtime) or cloud fallback (Claude, GPT-4, Gemini, Grok), then continuously improves through weighted LoRA fine-tuning to adapt to your specific coding patterns.

**Current State (v0.5.2):**
- ✅ ONNX Runtime with KV cache support
- ✅ Pre-trained Qwen models (1.5B/3B/7B/14B) + 5 other families
- ✅ Daemon architecture with auto-spawn
- ✅ OpenAI-compatible HTTP API
- ✅ Tool execution with pass-through
- ✅ SSE streaming for local and remote
- ✅ LoRA fine-tuning infrastructure (feedback collection + JSONL queue)
- ✅ Multi-provider teacher support (6 providers)
- ✅ Unified `[[providers]]` config with transparent migration
- ✅ Tabbed setup wizard with ONNX model selection
- ✅ IMPCPD iterative planning loop (`/plan` command)
- ✅ Universal alignment prompt (JSON normalization across providers)
- ✅ Live LLM test suite (gated by `FINCH_LIVE_TESTS=1`)
- 🚧 MCP plugin system (partial)

**Key Innovation:** Pre-trained models + weighted LoRA fine-tuning = immediate quality + continuous improvement.

## Architecture Overview

```
User runs finch
    ↓
Daemon auto-spawns (if not running)
    ↓
Background: Load ONNX model (if enabled)
    ↓
REPL appears instantly (<100ms)
    ↓
┌─────────────────────────────────────┐
│   User Query                        │
└──────────┬──────────────────────────┘
           │
           v
┌──────────────────────────────────────┐
│  Router with Model Check             │
│  - Crisis detection (safety)         │
│  - Local model ready? Use local      │
│  - Model loading? Forward to teacher │
└──────────┬───────────────────────────┘
           │
    Model Ready?
           │
    ├─ NO  → Forward to Teacher API (Claude/GPT-4/Gemini/Grok)
    └─ YES → Continue
           │
           v
    ┌──────────────────────────────────┐
    │ ONNX Runtime Inference           │
    │ (Qwen 1.5B/3B/7B/14B)           │
    │ + LoRA Adapters (optional)       │
    │ Device: CoreML/CPU               │
    └──────────┬───────────────────────┘
           │
           v
    ┌──────────────────────────────────┐
    │  Response to User                │
    │  (Streaming via SSE)             │
    └──────────┬───────────────────────┘
           │
           v
    User Feedback?
           │
    ├─ 🔴 Critical issue → High-weight training (10x)
    ├─ 🟡 Could improve → Medium-weight training (3x)
    └─ 🟢 Looks good → Normal-weight training (1x)
           │
           v
    ┌──────────────────────────────────┐
    │  Background LoRA Fine-Tuning     │
    │  (Python script, non-blocking)   │
    │  - Weighted sampling             │
    │  - Saves to safetensors          │
    └──────────────────────────────────┘
```

## Core Components

### 1. Daemon Architecture

**Auto-Spawning Daemon:**
- REPL client checks for running daemon (PID file at `~/.finch/daemon.pid`)
- If not running, spawns daemon process automatically
- Health checks ensure daemon is responsive
- Graceful restart on crashes

**OpenAI-Compatible HTTP API:**
- Port 11435 (11434 is used by Ollama)
- Endpoint: `POST /v1/chat/completions`
- Drop-in replacement for OpenAI/Claude clients
- Session management with concurrent client support

**Architecture:**
```
┌─────────────────────────────────────┐
│   REPL Client (tui-based)           │
│   - Keyboard input handling         │
│   - Streaming UI rendering          │
│   - Tool confirmation dialogs       │
└───────────┬─────────────────────────┘
            │ HTTP (port 11435)
            v
┌─────────────────────────────────────┐
│   Auto-Spawned Daemon               │
│   - PID management                  │
│   - Health monitoring               │
│   - Session cleanup                 │
└───────────┬─────────────────────────┘
            │
      ┌─────┴─────┬─────────┬─────────┐
      v           v         v         v
┌─────────┐ ┌─────────┐ ┌──────┐ ┌──────┐
│ ONNX    │ │ Teacher │ │ Tool │ │ LoRA │
│ Runtime │ │ APIs    │ │ Exec │ │Train │
└─────────┘ └─────────┘ └──────┘ └──────┘
```

**Key Files:**
- `src/daemon/server.rs` - Axum HTTP server
- `src/daemon/lifecycle.rs` - PID management, auto-spawn
- `src/client/daemon_client.rs` - HTTP client with health checks

### 2. ONNX Model Integration

**Purpose:** Load and run pre-trained models with KV cache for efficient autoregressive generation.

**Model Selection (RAM-based):**
- 8GB Mac → Qwen-2.5-1.5B (1.5GB RAM, fast)
- 16GB Mac → Qwen-2.5-3B (3GB RAM, balanced)
- 32GB Mac → Qwen-2.5-7B (7GB RAM, powerful)
- 64GB+ Mac → Qwen-2.5-14B (14GB RAM, maximum)

**Features:**
- ONNX Runtime with CoreML execution provider (macOS/Apple Silicon: dispatches ops to ANE, GPU, or CPU per-op; LLM workloads typically run mostly on CPU ARM due to partial CoreML op coverage)
- Full KV cache support (56+ dynamic inputs for 28 layers)
- Autoregressive generation with cache reuse
- Graceful CPU fallback
- Automatic tokenizer loading from HuggingFace Hub

**KV Cache Architecture:**
```rust
// Empty cache initialization (shape: [1, 2, 0, 128])
let mut kv_cache: Vec<Array4<f32>> = Vec::new();
for layer in 0..28 {
    kv_cache.push(Array4::zeros((1, 2, 0, 128))); // K and V
}

// Each generation step:
1. Bind input_ids, attention_mask, position_ids
2. Bind 56 KV cache inputs (28 layers × 2)
3. Run inference
4. Extract logits and updated KV cache
5. Reuse updated cache for next token
```

**Key Files:**
- `src/models/loaders/onnx.rs` - OnnxLoader, KV cache management
- `src/generators/qwen.rs` - QwenGenerator with multi-turn execution
- `src/models/adapters/qwen.rs` - Output cleaning and prompt formatting

### 3. Tool Execution System

**Purpose:** Enable AI to inspect and modify code through structured tools.

**Available Tools:**
- **Read** - Read file contents (code, configs, docs)
- **Glob** - Find files by pattern (`**/*.rs`)
- **Grep** - Search with regex (`TODO.*`)
- **WebFetch** - Fetch URLs (documentation, examples)
- **Bash** - Execute commands (tests, build, etc.)
- **Restart** - Self-improvement (modify code, rebuild, restart)

**Tool Pass-Through Architecture:**
```
┌─────────────────┐
│  REPL Client    │
│  (runs locally) │
└────────┬────────┘
         │ 1. Send query
         v
┌─────────────────┐
│  Daemon Server  │
│  (model, API)   │
└────────┬────────┘
         │ 2. Returns tool_use blocks
         v
┌─────────────────┐
│  REPL Client    │
│  - Executes tool│  ← Client has filesystem access
│  - Shows dialog │  ← Client owns terminal UI
│  - Collects     │
│    results      │
└────────┬────────┘
         │ 3. Send tool results
         v
┌─────────────────┐
│  Daemon Server  │
│  (final response)│
└─────────────────┘
```

**Why Pass-Through?**
- Client has filesystem access (daemon may not)
- Client owns terminal UI for confirmation dialogs
- Proper security model (user approves tools on their machine)
- Simple: daemon is stateless for tool execution

**Multi-Turn Loop:**
```
User Query → Daemon (with tool definitions)
    ↓
Model returns tool_use blocks
    ↓
Client executes tools → collects results
    ↓
Send results back to daemon (maintain conversation)
    ↓
Model returns final response (or more tool uses)
    ↓
Repeat up to 5 iterations
```

**Key Files:**
- `src/tools/executor.rs` - ToolExecutor, multi-turn loop
- `src/tools/implementations/` - Individual tool implementations
- `src/tools/permissions.rs` - PermissionManager, approval patterns
- `src/cli/repl_event/tool_execution.rs` - Client-side execution

### 4. IMPCPD Planning Loop

**Purpose:** Generate high-quality implementation plans through an iterative adversarial critique loop before any code is written.

**Command:** `/plan <task>` — initiates the loop. The REPL transitions to `ReplMode::Planning` during execution, then `ReplMode::Executing` after approval.

**Three-Iteration Loop:**
```
Iteration 1–3:
    generate_plan(task, previous_steering)
        ↓
    critique_plan(plan) → Vec<CritiqueItem>
        ↓
    Check convergence:
      delta_pct < 15% AND no must-address items? → converged
      Hard cap at iteration 3
        ↓
    (if not converged) steer → next iteration
        ↓
    Present plan to user (approve / steer / cancel)
        ↓
    Approved → ReplMode::Executing, cleared conversation context
```

**Seven Adversarial Personas** (defined in `impcpd_methodology.md`, sent verbatim to LLM):
- Always-active: Regression, Edge Cases, Completeness, Tests & Docs, Repo Hygiene, Git Discipline
- Keyword-activated: Security, Architecture, Scope Creep

**Key Types:**
- `PlanLoop` — orchestrates the loop; `PlanLoop::run(task, tui)` is the entry point
- `CritiqueItem` — `{ severity: Critical|Major|Minor, persona, description, must_address: bool }`
- `ImcpdConfig` — iteration cap, convergence threshold, persona activation keywords
- `PersonaSelector` — activates keyword-triggered personas based on task text

**Integration:**
- `Command::Plan(task)` in `event_loop.rs` → `handle_plan_task()` → `PlanLoop::run()`
- `Command::PlanModeToggle` → old simple toggle (unchanged)
- Alignment prompt injected into both `generate_plan` and `critique_plan` calls

**Key Files:**
- `src/planning/mod.rs` — public API
- `src/planning/loop_runner.rs` — `PlanLoop`, `generate_plan()`, `critique_plan()`
- `src/planning/types.rs` — `CritiqueItem`, `ImcpdConfig`, `PlanIteration`
- `src/planning/personas.rs` — `PersonaSelector`, persona definitions
- `src/planning/impcpd_methodology.md` — full methodology spec (embedded via `include_str!`)

### 5. Universal Alignment Prompt

**Purpose:** Normalize LLM output format (JSON structure, numbered lists, schema fidelity) across all six providers so the planning loop — and any structured caller — can safely swap to the cheapest available provider without format drift.

**How It Works:**

`UNIVERSAL_ALIGNMENT_PROMPT` (`src/providers/alignment.rs`) is a short system-prompt fragment that instructs the model to:
- Return valid JSON when a JSON schema is requested
- Use consistent numbered list format for critique items
- Include all required fields (`severity`, `persona`, `description`, `must_address`)
- Avoid wrapping JSON in markdown fences

`with_alignment(system_prompt)` prepends the fragment to any system prompt. The planning loop calls it on both `generate_plan` and `critique_plan` requests.

**Why It's Needed:**
Different providers (Claude vs. Gemini vs. Groq vs. Grok) have subtly different defaults for structured output. Without normalization, the critique parser would need per-provider special-casing. The alignment prompt provides a single, provider-agnostic contract.

**Key Files:**
- `src/providers/alignment.rs` — `UNIVERSAL_ALIGNMENT_PROMPT`, `with_alignment()`
- `src/planning/loop_runner.rs` — alignment wired into `generate_plan` and `critique_plan`

### 6. SSE Streaming Implementation

**Purpose:** Provide real-time token-by-token response streaming for both local and remote models.

**Architecture:**
```
┌─────────────────────────────────────┐
│  Generator (Local or Remote)        │
│  - Token-by-token generation        │
│  - Callbacks on each token          │
└────────────┬────────────────────────┘
             │
             v
┌─────────────────────────────────────┐
│  SSE Event Stream                   │
│  - Server-Sent Events format        │
│  - Bounded channel (size 2)         │
│  - Natural backpressure             │
└────────────┬────────────────────────┘
             │
             v
┌─────────────────────────────────────┐
│  TUI Renderer                       │
│  - Real-time UI updates (20 FPS)    │
│  - Shadow buffer diff rendering     │
│  - Scrollback integration           │
└─────────────────────────────────────┘
```

**Benefits:**
- Prevents connection timeouts on long queries (>10s)
- Responsive UI showing generation progress
- Cancel queries mid-generation (Ctrl+C)
- Works with both local ONNX and remote APIs

**Key Files:**
- `src/daemon/streaming.rs` - SSE event formatting
- `src/cli/tui/mod.rs` - TUI streaming response handling
- `src/generators/qwen.rs` - Token callbacks

### 7. LoRA Fine-Tuning Infrastructure

**Purpose:** Efficient domain-specific adaptation with weighted examples.

**Architecture (what works today → planned pipeline):**
```
User Feedback (10x/3x/1x weight)
    ↓
Feedback logged with weight to JSONL
    ↓
~/.finch/training_queue.jsonl
    ↓
[Pending] External training step:
  macOS  → MLX (mlx-lm, Apple Silicon native)
  Linux  → PyTorch + PEFT (transformers)
    ↓
[Pending] Convert adapter → .onnx_adapter (Olive toolchain)
    ↓
[Pending] Load via onnxruntime-genai Adapters API at inference
```

**Weighted Training:**
- **High-weight (10x)**: Critical issues (strategy errors)
  - Example: "Never use .unwrap() in production"
  - Impact: Model strongly learns to avoid this
- **Medium-weight (3x)**: Style preferences
  - Example: "Prefer iterator chains over manual loops"
  - Impact: Model learns your preferred approach
- **Normal-weight (1x)**: Good examples
  - Example: "This is exactly right"
  - Impact: Model learns normally

**Current Status:**
The feedback collection pipeline works; training and adapter loading are not yet implemented (see Issue #1). Key clarifications:
- ONNX Runtime itself has no training API. The training step uses an external tool: **MLX** on macOS (Apple Silicon) or **PyTorch/PEFT** on Linux/CUDA.
- `onnxruntime-genai` *does* support loading pre-trained adapters at inference time via `.onnx_adapter` format — this is not blocked by ONNX's lack of training API.
- `candle-metal` cannot be used for LoRA training on macOS — same missing ops (layer norm) that break inference also break training.
- `candle-coreml` (ANEMLL crate) uses a completely different model format and is not viable.

**Key Files:**
- `src/models/lora.rs` - `WeightedExample`, `LoRAConfig`, `ExampleBuffer` (placeholder infrastructure)
- `src/training/batch_trainer.rs` - Returns honest error; not wired to real training

### 8. TUI Renderer System

**Purpose:** Professional terminal UI with scrollback, streaming, and efficient updates.

**Dual-Layer Architecture:**

1. **Terminal Scrollback** (permanent, scrollable with Shift+PgUp)
   - Written via `insert_before()` for new messages
   - Pushes content above the inline viewport
   - Preserves full history (scrollable by user)

2. **Inline Viewport** (6 lines at bottom, double-buffered)
   - Separator line (visual boundary)
   - Input area (4 lines, tui-textarea)
   - Status bar (1 line, model/token info)

**Key Innovation: Immediate Scrollback with Efficient Updates**
```
New message → Write to scrollback immediately via insert_before()
Message updates → Diff-based blitting to visible area only
```

**Shadow Buffer System:**
- 2D char array with proper text wrapping
- ANSI escape code preservation (zero-width)
- Diff-based rendering (only changed cells)
- Bottom-aligned content (recent messages visible)

**Key Files:**
- `src/cli/tui/mod.rs` - TuiRenderer, flush_output_safe(), blit_visible_area()
- `src/cli/tui/shadow_buffer.rs` - ShadowBuffer, diff_buffers()
- `src/cli/tui/scrollback.rs` - ScrollbackBuffer (message tracking)
- `src/cli/tui/input_widget.rs` - Input area rendering
- `src/cli/tui/status_widget.rs` - Status bar rendering

### 9. Multi-Provider Teacher Support

**Purpose:** Flexible fallback to multiple cloud AI providers.

**Supported Providers:**
- **Claude** (Anthropic) - Primary, full capability
- **GPT-4** (OpenAI) - Full capability
- **Gemini** (Google) - Full capability
- **Grok** (xAI) - Full capability
- **Mistral** (Mistral AI) - Full capability
- **Groq** (Groq) - Fast inference

**Provider Adapters:**
Each provider has an adapter that handles:
- API request formatting (convert to provider's schema)
- Tool definition translation
- Response parsing
- Capability mapping (streaming, tool use, etc.)

**Adaptive Routing:**
```
1. Try local model if ready
2. On failure/unavailable, try first teacher
3. On API error, try next teacher in priority list
4. Graceful degradation ensures user always gets response
```

**Unified `[[providers]]` Config Format:**

```toml
[[providers]]
type = "claude"
api_key = "sk-ant-..."

[[providers]]
type = "grok"
api_key = "xai-..."
model = "grok-code-fast-1"

[[providers]]
type = "local"
inference_provider = "onnx"
execution_target = "coreml"
model_family = "qwen2"
model_size = "medium"
enabled = true
```

The `ProviderEntry` enum (`src/config/provider.rs`) has variants for each cloud provider plus `Local`. The factory (`src/providers/factory.rs`) converts entries to `Arc<dyn LlmProvider>` instances. The old `[[teachers]]` format is still accepted and auto-migrated on save.

**Key Files:**
- `src/providers/` - Provider-specific adapters
- `src/config/provider.rs` - `ProviderEntry` tagged enum
- `src/config/settings.rs` - `TeacherEntry` (legacy, kept for internal use)
- `src/providers/factory.rs` - `create_provider_from_entry()`, `create_providers_from_entries()`
- `src/cli/setup_wizard.rs` - Multi-provider setup UI

### 10. MCP (Model Context Protocol) Plugin System

**Status:** 🚧 Infrastructure complete, connection layer in progress

**Purpose:** Enable Shammah to connect to external MCP servers and use their tools dynamically.

**What is MCP?**
MCP (Model Context Protocol) is Anthropic's open standard for connecting AI assistants to external tools and data sources. Servers expose tools via JSON-RPC 2.0 over STDIO or HTTP+SSE.

**Example Use Cases:**
- `@modelcontextprotocol/server-github` - GitHub operations (issues, PRs, repos)
- `@modelcontextprotocol/server-filesystem` - Enhanced file operations
- `@modelcontextprotocol/server-postgres` - Database queries
- Custom servers for internal APIs, tools, etc.

**Architecture (Client-Side Execution):**
```
┌─────────────────────┐
│  REPL Client        │
│  (runs locally)     │
│                     │
│  ┌───────────────┐  │
│  │ MCP Client    │  │  ← Manages server connections
│  └───┬───────────┘  │
│      │              │
│      v              │
│  ┌───────────────┐  │
│  │ MCP Server    │  │  ← Subprocess (npx, cargo, etc.)
│  │ (STDIO)       │  │  ← JSON-RPC over stdin/stdout
│  └───────────────┘  │
└─────────────────────┘

User: "Create GitHub issue..."
    ↓
Daemon: [tool_use: mcp_github_create_issue]
    ↓
Client: Execute MCP tool locally
    ├─ Connect to MCP server (if not cached)
    ├─ Send JSON-RPC request
    ├─ Receive result
    └─ Return to daemon
    ↓
Daemon: "Issue created: #123"
```

**Why Client-Side?**
- MCP servers may need local filesystem access
- User controls external API keys (GitHub, Slack, etc.)
- Proper security: user approves tools on their machine
- Consistent with tool pass-through architecture
- MCP connections cached per client session

**Configuration Example:**
```toml
# ~/.finch/config.toml

[mcp_servers.filesystem]
command = "npx"
args = ["-y", "@modelcontextprotocol/server-filesystem", "/Users/finch"]
transport = "stdio"
enabled = true

[mcp_servers.github]
command = "npx"
args = ["-y", "@modelcontextprotocol/server-github"]
transport = "stdio"
env = { GITHUB_TOKEN = "$GITHUB_TOKEN" }
enabled = true

[mcp_servers.custom]
url = "http://localhost:3000/mcp"
transport = "sse"
enabled = false
```

**Implementation Status:**
- ✅ Configuration system (`McpServerConfig`)
- ✅ Config integration (TOML loading, validation)
- ✅ Module structure (`src/tools/mcp/`)
- ✅ Dependency (`rust-mcp-sdk` v0.8.3)
- 🚧 Connection layer (blocked by SDK private types)
- ❌ Client coordinator (depends on connection)
- ❌ Tool executor integration
- ❌ Setup wizard MCP section
- ❌ REPL `/mcp` commands (list, enable, disable, reload)

**Technical Challenge:**
The `rust-mcp-sdk` crate has private internal types (`ClientRuntime`) that can't be stored in structs. Completion requires either:
1. Direct JSON-RPC 2.0 implementation over STDIO (recommended)
2. Type-erasure workaround with `Box<dyn Any>`
3. Wait for SDK API improvements

**Next Steps:**
1. Implement JSON-RPC 2.0 transport (simple, well-documented)
2. Process management for MCP server subprocesses
3. Tool discovery and execution
4. Integration with ToolExecutor (same pass-through pattern)
5. Setup wizard section for managing MCP servers
6. REPL commands: `/mcp list`, `/mcp enable <name>`, `/mcp reload`

**Key Files:**
- `src/tools/mcp/config.rs` - Configuration types (COMPLETE)
- `src/tools/mcp/connection.rs` - Connection wrapper (PARTIAL)
- `src/tools/mcp/client.rs` - Client coordinator (PARTIAL)
- `docs/PHASE_4_MCP_PARTIAL.md` - Detailed implementation status

**References:**
- MCP Specification: https://modelcontextprotocol.io/specification/2025-11-25/
- MCP Servers: https://github.com/modelcontextprotocol/servers

### 11. Context Assembly (`src/context/`)

**Purpose:** Automatically discover and inject project-level AI instructions (`CLAUDE.md` / `FINCH.md`) into the system prompt at startup, without any user configuration.

**Load Order (outermost → highest priority):**

```
~/.claude/CLAUDE.md           ← user-level (Claude Code convention)
~/.finch/FINCH.md             ← user-level (Finch convention)
/CLAUDE.md                    ← filesystem root
/Users/CLAUDE.md
/Users/alice/projects/CLAUDE.md
/Users/alice/projects/myapp/CLAUDE.md   ← cwd (highest priority)
```

Within the same directory, `CLAUDE.md` is loaded before `FINCH.md`. All non-empty sections are joined with `\n\n---\n\n` and injected into the system prompt under `## Project Instructions`.

**`FINCH.md` — Vendor-Neutral Convention:**

`FINCH.md` is supported as a tool-agnostic alternative to the Anthropic-specific `CLAUDE.md` name. Teams can use `FINCH.md` for AI instructions that should work across multiple tools (Finch, Cursor, etc.).

**System Prompt Injection:**

```
[CODING_SYSTEM_PROMPT]

Working directory: /Users/alice/projects/myapp

## Project Instructions

# Project-level CLAUDE.md content here
Always prefer iterator chains.
Never use .unwrap() in production.

---

# Project-level FINCH.md content here
Match the code style in src/lib.rs.
```

**Integration point:** `ClaudeGenerator::new()` calls `collect_claude_md_context(cwd)` and stores the result in `self.claude_md_context`. `build_system_prompt(cwd, claude_md)` injects it.

**Key Files:**
- `src/context/claude_md.rs` - `collect_claude_md_context()` with 6 unit tests
- `src/context/mod.rs` - public re-export
- `src/generators/claude.rs` - `ClaudeGenerator`, `build_system_prompt()`

### 12. Conversation Management

**Purpose:** Manage multi-turn conversation history with context window limits.

**Features:**
- Automatic message trimming (last 20 messages)
- Token-based limits (~8K tokens per conversation)
- Session persistence (save/restore)
- Conversation compaction (auto-summarization)

**Compaction Architecture:**
```
Conversation grows → 80% of max tokens
    ↓
Trigger auto-compaction
    ↓
Summarize older messages (keep recent intact)
    ↓
Replace old messages with summary
    ↓
Continue conversation with reduced token count
```

**Key Files:**
- `src/cli/conversation.rs` - ConversationHistory
- `src/conversation/compactor.rs` - ConversationCompactor (to be implemented)

## System Flow

### REPL Session Flow

```
1. User starts `finch`
2. Check for running daemon (PID file)
3. If not running, spawn daemon process
4. Wait for daemon health check (up to 5s)
5. Display TUI with empty prompt
6. Background: Daemon loads ONNX model (if enabled)
7. User types query
8. Send HTTP POST to daemon
9. Daemon routes query (local or teacher)
10. Stream response tokens back to client (SSE)
11. TUI renders tokens in real-time (20 FPS)
12. If tool_use blocks, execute on client side
13. Send tool results back to daemon
14. Repeat until final response
15. Log metrics and feedback
```

### Daemon Lifecycle

```
1. Daemon starts (via auto-spawn or manual)
2. Load ONNX model (if backend enabled)
   - Download from HuggingFace Hub (first run)
   - Initialize KV cache
   - Load LoRA adapter (if exists)
3. Start HTTP server (port 11435)
4. Write PID file (~/.finch/daemon.pid)
5. Accept client connections
6. Handle queries concurrently
7. On SIGTERM/SIGINT, gracefully shutdown
8. Clean up sessions and resources
9. Remove PID file
```

## Data Flow

### Request Processing

```
1. Receive user query (HTTP POST)
2. Extract session_id (or create new session)
3. Add to conversation history
4. Router decision: local vs. teacher
5. If local:
     a. ONNX inference → local response
     b. Return response
6. If teacher:
     a. Forward to teacher API (Claude/GPT-4/etc.)
     b. Parse tool_use blocks
     c. Return tool_use to client
     d. Client executes tools
     e. Client sends tool results
     f. Forward tool results to teacher
     g. Return final response
7. Log metrics (routing, latency, tokens)
8. Update session last_activity
```

### Metrics Collection

Every request logs:
```json
{
  "timestamp": "2026-02-14T12:00:00Z",
  "session_id": "abc123",
  "routing_decision": "local",
  "response_time_ms": 650,
  "tokens_generated": 127,
  "tool_uses": 2,
  "forward_reason": null
}
```

Stored in: `~/.finch/metrics/YYYY-MM-DD.jsonl`

### Training Data Format

```json
{
  "id": "uuid",
  "timestamp": "2026-02-14T12:00:00Z",
  "query": "What is the golden rule?",
  "response": "The golden rule refers to...",
  "feedback_weight": 1.0,
  "feedback_type": "normal",
  "used_for_training": true
}
```

Stored in: `~/.finch/training_queue.jsonl`

## File Structure

```
~/.finch/
├── config.toml              # User configuration
├── daemon.pid               # Daemon process ID
├── daemon.sock              # IPC socket (unused, HTTP preferred)
├── adapters/                # LoRA adapters
│   ├── coding_2026-02-06.safetensors
│   └── rust_advanced.safetensors
├── metrics/                 # Daily JSONL logs
│   └── 2026-02-14.jsonl
├── training_queue.jsonl     # Pending training examples
└── tool_patterns.json       # Approved tool patterns

~/.cache/huggingface/hub/    # Base models (HF standard)
├── models--onnx-community--Qwen2.5-1.5B-Instruct/
├── models--onnx-community--Qwen2.5-3B-Instruct/
└── models--onnx-community--Qwen2.5-7B-Instruct/
```

## Technology Stack

**Language:** Rust 2021 edition
- Memory safety without GC
- High performance
- Excellent Apple Silicon support

**ML Framework:** ONNX Runtime (Microsoft-maintained)
- Cross-platform inference engine; requires ONNX-format models (converted from PyTorch)
- CoreML execution provider for macOS/Apple Silicon — per-op dispatch to ANE, GPU, or CPU; actual ANE usage depends on CoreML op support; LLM workloads often run predominantly on CPU ARM
- CUDA/ROCm/DirectML on Linux/Windows if available; CPU fallback everywhere
- KV cache support for efficient autoregressive generation
- ONNX format (optimized, portable)
- Note: `onnxruntime-genai` supports loading pre-trained LoRA adapters (`.onnx_adapter`) at inference time — this is the path for Issue #1

**Models:**
- Base: Qwen-2.5-1.5B/3B/7B/14B (ONNX format, pre-trained)
- Source: onnx-community on HuggingFace
- Adapters: LoRA (domain-specific, ~5MB each, via Python training)

**HTTP Server:** Axum
- Tokio async runtime
- Tower middleware stack
- Efficient request routing

**TUI:** Ratatui
- Modern terminal UI framework
- Composable widgets
- Efficient rendering

**Dependencies:**
- `ort` - ONNX Runtime bindings (Rust)
- `hf-hub` - HuggingFace Hub integration
- `tokenizers` - Tokenization (HF tokenizers crate)
- `tokio` - Async runtime
- `axum` - HTTP server
- `ratatui` - TUI framework
- `sysinfo` - System RAM detection

## Performance Targets

### Current Performance (v0.5.2)

**Startup:**
- REPL available: <100ms (instant)
- Daemon spawn: 2-3s from cache
- Model loading: 5-10s background (non-blocking)

**Response Time:**
- Local generation: 500ms-2s (depending on model size)
- With LoRA adapter: +50-100ms overhead
- Teacher API: Standard API latency (1-3s)
- Tool execution: ~50-200ms per tool

**Resource Usage:**
- RAM: 3-28GB (depending on model size)
- Disk: 1.5-14GB for base model + ~5MB per adapter
- CPU (idle): <5%

**Daemon:**
- Throughput: 1000+ requests/second (health checks)
- Latency overhead: <5ms (excluding model inference)
- Memory per session: ~20MB
- Max concurrent sessions: 100 (configurable)

## Security & Privacy

### Data Protection
- All metrics hashed (SHA256) for privacy
- Models train only on YOUR data
- No telemetry, no cloud sync
- Can delete `~/.finch/` anytime

### Tool Safety
- Permission system with approval dialogs
- Session and persistent patterns
- Wildcard and regex matching
- User controls all tool execution

### Daemon Security
- Binds to localhost by default (127.0.0.1)
- API key authentication (Phase 4 - not yet implemented)
- Rate limiting (Phase 4 - not yet implemented)
- TLS support via reverse proxy (nginx)

**Current Recommendations:**
- Only bind to localhost unless on trusted network
- Use firewall rules to restrict access
- Run behind reverse proxy (nginx) for production
- Monitor logs for suspicious activity

## Future Optimizations

### Pure Rust LoRA Training
- Current: Python-based training (2x memory overhead)
- Future: Custom Rust implementation compatible with ONNX Runtime
- Options: burn.rs, custom ONNX graph mods, or wait for ONNX Training support

### Adapter Loading at Runtime
- Load trained LoRA adapters during ONNX inference
- Requires weight merging or dynamic ONNX graph modification
- Enables instant domain switching without reloading base model

### Quantization
- INT8 quantization for lower memory usage
- Faster inference on Neural Engine
- Trade-off: slight quality reduction for 4x memory savings

### Multi-GPU Support
- Distribute inference across multiple GPUs
- Enables larger models (70B+)
- Requires model parallelism implementation

## References

- **DAEMON_MODE.md** - Detailed daemon architecture and API
- **TOOL_CONFIRMATION.md** - Tool permission system details
- **TUI_ARCHITECTURE.md** - Terminal UI rendering system
- **USER_GUIDE.md** - Setup and usage instructions
- **ROADMAP.md** - Future work planning

---

**Current Version:** 0.5.2
**Last Updated:** 2026-02-22
