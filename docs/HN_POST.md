# Hacker News — Show HN

## Title

Show HN: Finch – open-source agentic coding assistant that works with any LLM (Grok, Claude, GPT-4, or local ONNX)

---

## Body

We've been building **Finch**, a terminal AI coding assistant written in Rust. It's in the same space as Claude Code, but the core design decision is different: the tool-execution loop is fully decoupled from any particular provider.

**What that means in practice:**
- The same Read/Glob/Grep/Bash/WebFetch agentic loop works identically whether you're using Grok, GPT-4, Gemini, Claude, Mistral, Groq, or a local Qwen/Llama/Gemma model running via ONNX Runtime
- You can switch mid-session with `/teacher grok` or `/teacher claude` — the conversation history round-trips correctly through all formats
- The daemon auto-spawns and exposes an OpenAI-compatible API on port 11435, so VS Code extensions and other tools that speak OpenAI can use your local model with zero cloud costs; mDNS/Bonjour lets other machines on your LAN discover it automatically

The reason we started this: X Premium+ includes free Grok API credits at console.x.ai. That means a non-developer can get an API-capable frontier model through a consumer subscription, not a developer account. We wanted to make the full agentic loop available to that use case — no Anthropic/OpenAI account required.

**What's actually working:**
- Multi-provider agentic loop with permission-gated tool use (same UX as Claude Code)
- 6 local model families via ONNX Runtime: Qwen, Llama, Gemma, Mistral, Phi, DeepSeek — downloaded from the onnx-community HuggingFace org
- On Apple Silicon: ONNX Runtime's CoreML execution provider (ops dispatch to ANE/GPU where CoreML supports them — the benefit is real but partial; many LLM ops fall back to CPU ARM)
- `--cloud-only` flag: skips local model entirely, pure cloud provider with no model download
- `/plan <task>`: 3-iteration loop where 7 adversarial personas critique each draft plan; must-address issues block convergence; works cross-provider via a universal alignment prompt that normalises JSON output
- CLAUDE.md / FINCH.md auto-loading (walks the filesystem to your working directory, exactly like Claude Code)
- Background daemon with OpenAI-compatible API + mDNS/Bonjour discovery

**What's not working yet (being honest):**
- LoRA fine-tuning: the weighted feedback collection (Ctrl+G/Ctrl+B) is wired and writing to ~/.finch/training_queue.jsonl, but training and adapter loading aren't implemented. The planned pipeline is MLX training → Olive conversion → onnxruntime-genai Adapters API at inference time (Issue #1).
- MemTree persistence: the hierarchical memory tree data structure is there and REPL-wired, but SQLite persistence and the TUI tree view aren't done yet.
- MCP plugin system: config layer is in, connection layer is partial.

**Getting started** (fastest path — Grok, no model download):

```
curl -sSL https://raw.githubusercontent.com/darwin-finch/finch/main/install.sh | sh
finch setup   # enter your API key
finch --cloud-only
```

Source: https://github.com/darwin-finch/finch
Website: https://darwin-finch.github.io

We're actively looking for contributors — especially around LoRA adapter loading, integration tests, and MCP. Happy to answer questions about the architecture.

---

## Tags to use
- Show HN
- Rust
- Local LLM
- Developer Tools

## Notes for submitter
- Post on a weekday morning US time (9–11am ET)
- Do not cross-post to HN and Reddit simultaneously; wait for one to settle
- Respond to every comment in the first 2 hours — HN rewards engagement

