# finch-providers: LLM transports, OAuth, and credential ports

This crate is Finch's boundary to the outside world of LLM providers: dispatch contracts
(`LlmProvider`/`ProviderBackend`), provider-neutral wire types and stream events, the model
catalog, OAuth lifecycle and dialects, and the Claude / OpenAI-compatible / Gemini / ChatGPT /
SuperGrok adapters. It exists as its own crate so provider integration can be built and tested
without Finch application code — Brain, TUI, daemon, tool execution, or `Config` never appear here.

Current ownership and test instructions are in [`AGENTS.md`](AGENTS.md); the exact Rust surface is
[`src/lib.rs`](src/lib.rs).

## Scope

Stable provider-neutral contracts (`LlmProvider`, `ProviderRequest`, wire types, `StreamChunk`,
credential ports, `OAuthDialect`) are the public contribution surface. Provider-specific dialects
(ChatGPT OAuth, Anthropic Messages, Gemini, OpenAI-compatible) remain adapter-private and must not
be flattened into a lowest-common-denominator parser. See
[`src/oauth/AGENTS.md`](src/oauth/AGENTS.md) for the OAuth sub-subsystem specifically.
