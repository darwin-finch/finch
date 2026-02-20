# Building a Local-First AI Coding Agent: Grok, MemTree, and What Comes Next

We've been building **Finch** — a local-first AI coding agent written in Rust. It's in the same spirit as Claude Code or Cursor, but designed to run on your own hardware, learn your codebase over time, and work with whatever AI backend you have access to. This is the story of where it came from, what we've figured out, and what's still open.

---

## It Started as a Claude Wrapper

The first version of Finch was exactly what it sounds like: a Rust TUI that forwarded your questions to Claude's API and streamed the answers back. The interesting work was in the tool execution loop — Read, Glob, Grep, Bash — and the permission system that asked before running anything destructive. The multi-turn agentic loop came together quickly. The hard part was the terminal UI: a scrollback buffer that stays scrollable during streaming, diff-based rendering so the screen doesn't flicker, a proper multi-line input area. That took longer than the AI parts.

But it was Claude-only. You needed an Anthropic API key. That's fine if you're a developer, but it's a real barrier for anyone else, and it's expensive if you're running it all day.

---

## The Case for Grok

Grok is interesting for a few reasons. The biggest one right now: an X Premium+ subscription includes API credits at `api.x.ai`, which makes Grok the only major frontier model you can access through a consumer subscription rather than a developer account. A non-developer friend with an old MacBook and an X subscription can actually use this.

But "Grok is supported" is a long way from "Grok works correctly." When we actually traced through the code with Grok in mind, we found four bugs that would silently break the agentic loop:

**1. System prompts were being dropped for all providers.**
The `ProviderRequest` type (our internal format) didn't have a `system` field at all. The system prompt — which tells the model to be a coding agent, use tools in a specific format, maintain persona — was silently discarded before the request went out. This affected every non-Claude provider.

**2. Tool calls in assistant messages were dropped by the OpenAI-compatible path.**
Claude's API format and OpenAI's format represent tool calls differently. In Claude's format, a tool call is a `ContentBlock::ToolUse` inside an assistant message. In OpenAI's format, it goes into a `tool_calls` array on the message object. Our converter was only emitting the text content and silently dropping any `ToolUse` blocks. This meant Grok would never see that it had made a tool call, so subsequent tool results would reference IDs that didn't exist in the conversation history — breaking the loop entirely.

**3. Spurious `name` field on tool results.**
OpenAI's API accepts a `name` field on tool result messages; some providers (including Grok) reject it. We were sending it.

**4. Outdated model name.**
We were sending `grok-beta`. The current model is `grok-2`.

None of these produced loud errors. They either silently truncated the conversation or sent a request that returned a confusing response. This is the category of bug that only surfaces when you test against the real API end-to-end.

After fixing these, the architecture now has a clean `LlmProvider` trait that Claude, Grok, GPT-4, Gemini, Mistral, and Groq all implement. The system prompt is properly passed through. Tool calls in assistant messages serialize correctly for each format. The conversation history round-trips correctly through the agentic loop regardless of which backend you're using.

---

## Cloud-Only Mode

Getting a non-developer set up meant one more thing: by default, Finch tries to load a local ONNX model in the background (Qwen 2.5, selected based on your RAM). That means a 1.5–14GB download on first launch. For someone who just wants to use Grok, that's the wrong default.

We added `--cloud-only` (alias: `--teacher-only`):

```bash
finch --cloud-only
```

This skips the model download, skips the daemon, and routes everything directly to your configured teacher API. The binary stays completely self-contained — the ONNX Runtime is loaded dynamically only when a local model is actually used, so the binary has no native library dependencies at all when running in cloud-only mode.

For our friend with the old Intel MacBook Pro: he gets his Grok API key from `console.x.ai`, runs `finch setup`, picks Grok, and uses `finch --cloud-only`. No model download. No daemon. No Rust toolchain.

The release workflow now builds native binaries for both Apple Silicon (`macos-14`) and Intel (`macos-13`) via GitHub Actions.

---

## What MemTree Is, and Why It's Not Quite RAG

RAG — Retrieval-Augmented Generation — is the dominant pattern for giving LLMs long-term memory. The idea is: embed your documents as vectors, store them in a vector database, and at query time retrieve the top-k most semantically similar chunks and stuff them into the context window. It works. It's production-proven. But it has a structural limitation for a coding agent.

RAG is **flat**. Every chunk is equally a candidate for retrieval. There's no notion of "these ten chunks all belong to the same module, which belongs to the same project." You lose the hierarchy that makes code meaningful. When you retrieve five random functions from five different files, you might miss that they're all implementing the same pattern, or that three of them are about to be deleted.

**MemTree** is a different approach, based on a 2024 paper (arXiv:2410.14052). Instead of a flat vector store, it's a tree where:

- Leaf nodes contain the actual text and its embedding
- Parent nodes contain **aggregated embeddings** — an average of their children's embeddings, which makes them a compressed summary of a semantic cluster
- Insertion is O(log N): you walk down the tree by similarity until you find where a new node belongs
- At query time, you can traverse the hierarchy to pull relevant context at the right level of granularity

For a coding agent, this maps naturally onto the structure of software. A conversation about authentication in your web app shouldn't pull in random database migrations from six months ago just because they share a token. With MemTree, related conversations cluster together; the tree shape reflects semantic proximity.

The other thing MemTree does for us that RAG doesn't: it's also the UI. The MemTree console shows your conversation history as a navigable tree — user messages as parent nodes, assistant responses and tool calls as children. You can expand and collapse branches, navigate with the keyboard, and see the structure of a long agentic session at a glance rather than a flat scroll of text.

### What MemTree Has Right Now

The core data structure is implemented: O(log N) insertion, cosine similarity navigation, parent aggregation after each insertion. The console layer (event handler, node types, expand/collapse state) is wired to the REPL event system so tool calls become child nodes of the response that triggered them.

What's missing:

**Real embeddings.** The current implementation uses a hash-based TF-IDF placeholder that fits in 384 dimensions. It works for tests and gives correct structure, but the semantic similarity is weak — two functions that do the same thing but use different variable names won't cluster together. For production use, this needs to be replaced with actual sentence embeddings: either a small local ONNX model (something like `nomic-embed-text` or `all-MiniLM-L6-v2`, both available in ONNX format and fast on CPU), or an embeddings API call.

**TUI rendering.** The tree state and event wiring exist, but the ratatui widget that actually draws the tree to the terminal isn't done. This is the most visible gap — the "memtree view" mode doesn't show anything useful yet.

**Keyboard navigation.** j/k to move up/down through nodes, Enter to expand/collapse, o to open the full content of a node.

**Hierarchical retrieval.** The current `retrieve()` method falls back to flat search — it computes cosine similarity against every node and returns the top-k. The paper's actual retrieval algorithm is hierarchical: start at root, descend into the most similar subtree, then retrieve from the leaves of that subtree. This is better because it preserves context locality.

**Persistence.** The tree lives in memory. When you close Finch, it's gone. The schema.sql is there for a SQLite backend, but the code to serialize/deserialize the tree isn't written.

---

## So Do We Need RAG?

The honest answer: MemTree subsumes RAG for our use case, but only once the embedding layer is real.

RAG is simpler to implement and has a massive ecosystem. If we needed production memory tomorrow, we'd drop in SQLite + sqlite-vec and a small embedding model and call it done. It would work fine.

But MemTree is worth the extra complexity here because:

1. **Code has hierarchy.** Flat retrieval loses structural context that matters for coding agents.
2. **The console view is valuable independently.** Even if you ignored the memory aspect entirely, having a navigable tree view of a long agentic session — with tool calls as collapsible children, with latency and token counts on each branch — is a genuinely better UI for agentic work than a flat chat transcript.
3. **Cross-session memory.** Once we persist the tree, a user can ask "what did we do with the auth system last month?" and get a hierarchically organized answer rather than a bag of retrieved chunks.

The key question is embeddings. The TF-IDF placeholder needs to become a real model. The good news: there are small ONNX sentence transformer models (100-300MB) that run on CPU in milliseconds. We already have the ONNX Runtime infrastructure. It's a well-defined next step.

---

## What's Still Left to Build

In rough priority order:

**Short term:**
- MemTree TUI rendering and keyboard navigation (the thing that should be visible now but isn't)
- Real embeddings via a small local ONNX sentence transformer
- MemTree persistence (SQLite backend)

**Medium term:**
- Memory tools for Grok: `MemoryRead` and `MemoryWrite` tools that let the model explicitly store and retrieve facts across sessions. Grok doesn't have Claude Code's built-in memory system, so we need to give it the equivalent. These tools would write to and query the MemTree, giving any provider persistent memory.
- Hierarchical MemTree retrieval (the proper algorithm from the paper, not the flat fallback)
- Autonomous agent mode: run Finch headlessly overnight on a task backlog, commit with a custom git identity (persona), log everything to JSONL, periodically reflect on completed work to update the agent's own system prompt. The data structures and CLI command are already there; it needs end-to-end testing.

**Longer term:**
- LoRA adapter loading at inference time (the training infrastructure exists; the runtime loading doesn't)
- Plan mode (structured multi-step planning before execution, like Claude Code's plan approval flow)
- Additional model families (Phi, DeepSeek)

---

## The Broader Shape of the Thing

What we're building is an AI coding agent that:

1. Works offline with a local model, or in cloud-only mode with any provider you have access to
2. Accumulates structured memory across sessions rather than forgetting everything
3. Can run autonomously overnight on a task backlog, committing work as a named agent
4. Learns your patterns over time via LoRA fine-tuning on your feedback
5. Costs as little as possible — local inference is free after the model download; Grok via X Premium+ is covered by a subscription most developers already have

The architecture is in good shape. The provider layer is clean and tested. The agentic loop works. The memory structure is designed and partially implemented. The main gaps are all in the "making it visible and persistent" category: the tree UI, real embeddings, and SQLite-backed persistence.

The Grok work made one thing clear: building for multiple providers forces you to understand your own abstractions. Every bug we found while getting Grok working was a bug that existed for every provider — we just hadn't noticed because Claude's API is forgiving about some things that other APIs aren't. Supporting Grok made the system more correct, not just more flexible.

---

*Finch is written in Rust. The current release supports macOS (Apple Silicon and Intel), with Linux support in progress. Source at [github.com/schancel/shammah](https://github.com/schancel/shammah).*
