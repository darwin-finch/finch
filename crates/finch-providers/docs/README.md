# `finch-providers` documentation

This directory owns implemented reference material for the extracted provider
crate: dispatch contracts, OAuth lifecycle, catalogs, and injected ports.

Current ownership and test instructions are in [`../AGENTS.md`](../AGENTS.md),
and the exact Rust surface is generated in [`../INTERFACE.md`](../INTERFACE.md).

Stable provider-neutral contracts (`LlmProvider`, `ProviderRequest`, wire types,
`StreamChunk`, credential ports, `OAuthDialect`) are the public contribution
surface. Provider-specific dialects (ChatGPT OAuth, Anthropic Messages, Gemini,
OpenAI-compatible) remain adapter-private and must not be flattened into a
lowest-common-denominator parser.

Application configuration, Brain, TUI, daemon, and tool execution stay in Finch.
