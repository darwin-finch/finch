# Reddit Posts

Two versions: one for r/rust (technical), one for r/programming (broader).

---

## r/rust — Technical audience

### Title
Finch: open-source AI coding agent in Rust — ONNX inference, multi-provider agentic loop, mDNS local model sharing

### Body

We've been building **Finch** (https://github.com/darwin-finch/finch) — a terminal AI coding agent written in Rust, in the same spirit as Claude Code but designed to work with any LLM backend.

**The Rust-specific stuff:**

The core challenge was getting agentic tool use working cleanly across six cloud providers (Claude, GPT-4, Gemini, Grok, Mistral, Groq) and six local model families (Qwen, Llama, Gemma, Mistral, Phi, DeepSeek via ONNX Runtime). Every provider has subtly different wire formats for tool calls, system prompts, and conversation history. We ended up with an `LlmProvider` trait that each adapter implements — the agentic loop never sees provider-specific types.

The TUI was the most interesting engineering work: a dual-layer system where messages go to terminal scrollback via `insert_before()` immediately on receipt (not after completion), so streaming responses are scrollable with Shift+PgUp during generation. Updates to existing messages go through a diff-based shadow buffer blit — a 2D char array that tracks changes and only rewrites changed rows, with `BeginSynchronizedUpdate` for tear-free rendering.

For local inference, we use the `ort` crate (ONNX Runtime bindings). On macOS/Apple Silicon, the CoreML execution provider dispatches ops to ANE/GPU where CoreML's op set supports them — the benefit is real but partial for LLM workloads; many transformer ops fall back to CPU ARM. We specifically did NOT use `candle-metal` — the Candle Metal backend is missing the layer-norm kernel and has matmul edge cases that produce incorrect output on Qwen. Tried `candle-coreml` (the ANEMLL crate) too; that requires models in `.mlpackage` format, got tensor dimension mismatches.

**What ships in v0.5.2:**
- Multi-provider agentic loop (6 cloud providers + local ONNX)
- Auto-spawning daemon with OpenAI-compatible API on port 11435; mDNS/Bonjour for LAN discovery
- Permission-gated tool use (Read, Glob, Grep, Bash, WebFetch) with pattern matching
- `/plan <task>`: 3-iteration 7-persona adversarial critique loop with convergence detection
- Universal alignment prompt that normalises JSON output across all 6 cloud providers (the same planning loop works on cheap Groq or expensive Claude)
- Live test suite gated by `FINCH_LIVE_TESTS=1` — verifies structural contracts with real API calls
- CLAUDE.md / FINCH.md auto-loading (filesystem walk to cwd, like Claude Code)
- Progressive bootstrap: REPL in <100ms, model loads in background

**What's not done yet (being honest with r/rust):**
- LoRA adapter loading at inference: plan is MLX train → Olive convert → `onnxruntime-genai` Adapters API (Issue #1, 40–80h)
- MemTree TUI rendering + SQLite persistence
- MCP connection layer (config is in; JSON-RPC 2.0 over STDIO is the implementation path)

1053 unit tests, 0 warnings. Happy to discuss the ONNX/Candle backend trade-offs, the TUI architecture, or the provider abstraction design.

https://github.com/darwin-finch/finch | https://darwin-finch.github.io

---

## r/programming — Broader audience

### Title
Finch: open-source AI coding terminal agent — bring your own API key, works offline with local models

### Body

We built **Finch** (https://github.com/darwin-finch/finch) because we wanted a Claude Code-style agentic experience that wasn't tied to one provider.

**The basic pitch:**

The full agentic loop — Read, Glob, Grep, Bash, WebFetch, multi-turn execution, permission dialogs before every action — works identically with Grok, GPT-4, Claude, Gemini, Mistral, Groq, or a local model running on your own hardware. Switch mid-session with `/teacher grok`. The daemon exposes an OpenAI-compatible API so VS Code and other tools can use it too.

The reason we care about provider flexibility: X Premium+ includes free Grok API credits, so someone who isn't a developer with an Anthropic/OpenAI account can still run a full agentic coding assistant. And switching to the cheapest capable provider matters when you're running it all day.

**For offline use**, it downloads and runs models via ONNX Runtime — Qwen, Llama, Gemma, Phi, DeepSeek. One self-contained Rust binary with no Python/Node/Docker dependency. First run downloads the model in the background; you can start querying immediately while it loads (falls back to your cloud provider).

**New in v0.5.2 — `/plan` command:**
Type `/plan implement X` and it runs 3 rounds of critique by 7 adversarial personas (Regression Analyst, Edge Case Hunter, Security Auditor, etc.) before presenting a plan for approval. Must-address issues block convergence. Because of a universal alignment prompt, this works on any of the 6 supported cloud providers — the structured JSON output is normalised regardless of which LLM you're on.

**What's not there yet:** LoRA fine-tuning (feedback collection is wired; training and adapter loading aren't), MemTree TUI persistence, MCP connection layer.

**Quickstart** (Grok, cloud-only, no model download needed):
```
curl -sSL https://raw.githubusercontent.com/darwin-finch/finch/main/install.sh | sh
finch setup
finch --cloud-only
```

Source + issue tracker: https://github.com/darwin-finch/finch

---

## Subreddits to consider
- r/rust (technical post above)
- r/programming (broader post above)
- r/LocalLLaMA (lead with local inference + ONNX + mDNS sharing angle)
- r/commandline (lead with TUI, daemon, pipe mode)

## r/LocalLLaMA variant title
"Finch: run Qwen/Llama/Gemma locally via ONNX, share over LAN with mDNS — and use any cloud provider as fallback"

## Timing notes
- r/rust: any weekday is fine; technical audience is active throughout the week
- r/programming: Monday–Wednesday mornings get more traction
- r/LocalLLaMA: engagement is high any time; lead with the local model angle, not the cloud fallback angle

