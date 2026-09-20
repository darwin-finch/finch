# oauth: compatibility re-export

This directory is a thin Finch compatibility facade only — callers use `crate::oauth::Item` for
source stability, but the actual OAuth implementation, dialects, persistence, and tests live in
[`crates/finch-providers/src/oauth`](../../crates/finch-providers/src/oauth/README.md). Nothing
here should grow application logic; if you're looking for OAuth behavior, go to that crate.

Ownership and test commands are in [`AGENTS.md`](AGENTS.md).

## Further documentation

[`../../docs/OAUTH.md`](../../docs/OAUTH.md) — the OAuth boundary. Provider-specific transports:
[ChatGPT subscription](../../docs/CHATGPT_SUBSCRIPTION_TRANSPORT.md),
[OpenAI](../../docs/OPENAI_TRANSPORT.md).
