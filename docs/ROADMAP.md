# Shammah Development Roadmap

**Last Updated:** 2026-02-22
**Current Version:** v0.5.2

This document is a forward-looking guide. Completed work is summarised in the "Current" section and detailed in CHANGELOG.md. Open issues are tracked at https://github.com/darwin-finch/finch/issues.

---

## Current: v0.5.2 (Feb 2026)

Core infrastructure is complete and production-ready.

**What shipped through v0.5.2:**
- ONNX Runtime inference (CoreML on Apple Silicon, CPU elsewhere) — 6 model families
- Multi-provider cloud fallback (Claude, GPT-4, Gemini, Grok, Mistral, Groq)
- Unified `[[providers]]` config with transparent `[[teachers]]` migration
- 6 tools — Read, Glob, Grep, WebFetch, Bash, Restart — with permission system
- Auto-spawning daemon with OpenAI-compatible API and mDNS/Bonjour discovery
- Professional TUI — scrollback, streaming, ghost text, plan mode, Ctrl+G/B feedback
- IMPCPD iterative planning loop (`/plan <task>`) with 7 adversarial personas
- Universal alignment prompt — JSON normalization across all 6 providers
- Live LLM test suite (`FINCH_LIVE_TESTS=1 cargo test -- --include-ignored live_`)
- LoRA weighted feedback collection + JSONL queue (adapter loading still pending)
- Progressive bootstrap — instant REPL startup, background model load
- CLAUDE.md / FINCH.md context injection (matches Claude Code behaviour)

---

## Near-term: v0.5.3 – v0.6.0

### [#1] LoRA adapter loading at ONNX runtime
**Effort:** 40–80h  **Priority:** High
The feedback collection pipeline is complete; the missing piece is loading trained LoRA
adapters into ONNX Runtime at inference time. Options:
- Option A (simpler): Python training → merge weights → re-export ONNX model
- Option B (preferred): Runtime adapter injection via weight merging in Rust

Blocked on designing the ONNX graph modification strategy.

### [#2] Mistral ONNX support
**Effort:** 4–8h  **Priority:** Medium
`onnx-community` has not yet published Mistral ONNX models. Once available, test with
the existing `LlamaAdapter` (same architecture) and document.

### [#3] Additional model adapters
**Effort:** 4–8h per model  **Priority:** Medium
- CodeLlama (Meta) — code-specialized Llama variant
- Yi (01.ai) — strong multilingual/code models
- StarCoder (BigCode) — open code-focused model

### [#5] Integration tests — daemon, LoRA, multi-provider, tool pass-through
**Effort:** 8–16h  **Priority:** Medium
Fill gaps in test coverage for cross-module workflows. The live test suite (`tests/live/`)
covers provider parity; this issue tracks daemon lifecycle, tool pass-through, and
multi-session concurrency tests.

---

## Medium-term

### [#7] LoRA training memory efficiency
Current Python-based training loads the full base model for adapter training, using 2×
the model's memory. Optimise with gradient checkpointing, 4-bit quantization of the
frozen base, or a pure-Rust training path (burn.rs / custom).

### Additional model adapters (continued)
Expand the ONNX model catalogue as onnx-community publishes more families. Phi-4,
DeepSeek-R1-Distill, and Qwen-2.5-Coder are strong candidates.

### MCP plugin system
The configuration layer and module structure (`src/tools/mcp/`) are in place. The
connection layer needs a direct JSON-RPC 2.0 over STDIO implementation (the `rust-mcp-sdk`
has private types that block the current approach). Once complete:
- Tool discovery from MCP servers
- Integration with `ToolExecutor`
- Setup wizard section for managing MCP servers
- REPL commands: `/mcp list`, `/mcp enable <name>`, `/mcp reload`

---

## Long-term

### Distributed inference
Allow multiple machines to collaborate on a single inference request. Useful for
splitting large models (70B+) across a home network.

### Multi-machine model sharing
Extend the mDNS daemon so any finch client on the LAN can transparently use the
most powerful model available on the network — not just its own.

### Quantization
INT4/INT8 quantization for lower memory usage and faster inference on Apple Neural
Engine. Trade-off: slight quality reduction for ~4× memory savings.

### Multi-GPU / multi-ANE support
Distribute tensor operations across all available compute (multiple GPUs on Linux,
multi-ANE chips on future Apple Silicon).

---

## Contribution Guidelines

1. **Check GitHub Issues** before starting — claim or open an issue
2. **Every bug fix must have a regression test** (see CLAUDE.md testing philosophy)
3. **Follow the early-exit pattern** and Rust best practices documented in CLAUDE.md
4. **Surgical git staging** — `git add <explicit paths>`, no `git add .`
5. **Update CHANGELOG.md** and relevant docs with your change

---

**Maintained By:** Shammah contributors
**Issue Tracker:** https://github.com/darwin-finch/finch/issues
