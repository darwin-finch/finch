# Legacy ChatGPT subscription configuration

Finch no longer depends on, discovers, installs, or launches Codex or any other
provider executable. The former `chatgpt_subscription` / Codex app-server
provider is unsupported.

Old configuration still deserializes so Finch can report an actionable migration
error. It is never reinterpreted as an OpenAI Platform API key and it never starts
a process or makes a provider request. Run `finch setup`, remove the legacy
profile, and configure one of the supported providers.

OpenAI Platform remains available directly with an API key:

```toml
[[providers]]
type = "openai"
api_key = "sk-..."
model = "gpt-4o"
```

A Finch-native ChatGPT subscription flow may be added only if OpenAI publishes a
supported third-party contract for client registration, authorization, audience,
scopes, refresh and revocation, catalog access, and inference transport. Finch
must not reuse Codex client identity or credentials and must not call undocumented
consumer endpoints.
