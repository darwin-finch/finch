# ChatGPT subscription provider

Finch's `chatgpt_subscription` profile embeds the supported Codex app-server. It is
not an OpenAI API-key profile and it does not reinterpret ChatGPT OAuth credentials
as API keys. Codex owns browser/device login, token storage, refresh, and logout;
Finch stores only the opaque `codex-app-server:managed` reference.

Configure and authenticate it with:

```console
finch auth login chatgpt
finch auth status chatgpt
finch setup
```

Each Finch request starts an ephemeral Codex thread with `approvalPolicy: never`
and a turn-level restricted read-only policy whose only readable root is a fresh
empty temporary directory. Finch clears MCP configuration, disables inherited
apps, plugins, shell, exec, web search, hooks, and subagents, and passes only a
small environment allowlist to app-server. Finch sends an encoded copy of the
complete Brain conversation on every request, so an app-server thread is never
durable conversation truth. Finch refuses to construct the provider when the
installed protocol schema cannot express the restricted read-only policy.
Codex CLI 0.149.1 does not expose the required readable-root controls, so Finch
rejects that version for all ChatGPT subscription turns; install a newer Codex
release whose generated schema includes the restricted `readOnly` access variant.
Finch additionally pins one canonical, owner-controlled Codex launcher/interpreter
identity and SHA-256 artifact pair for schema inspection and runtime, rejects any
writable installation ancestor, and accepts only explicitly audited CLI
versions. No Codex release is currently on that allowlist: a release must be audited
for effective built-in tools, managed configuration precedence, environments, and
sandbox enforcement before this provider becomes available. This deliberate fail-
closed compatibility limit prevents a future schema-compatible release from being
trusted automatically.

The profile default is `gpt-5.6-sol`; the official `gpt-5.6` alias currently routes
to that model. Finch does not substitute the lower-cost Terra tier implicitly.

This is an agent-protocol adapter, not raw Chat Completions or Responses API
parity. Finch tool calls require the installed app-server schema to advertise the
experimental `dynamicTools` thread capability. If it does not, Finch rejects that
provider before starting the turn and proceeds to the next configured provider
(for example Grok). Text-only turns require the same restricted sandbox boundary
and therefore also fall back when it is unavailable.

The ignored live smoke test is opt-in:

```console
FINCH_LIVE_CHATGPT_APP_SERVER=1 cargo test live_managed_subscription_smoke_test -- --ignored
```

Official references:

- [Codex app-server](https://developers.openai.com/codex/app-server)
- [Codex authentication](https://developers.openai.com/codex/auth)
- [GPT-5.6 Sol](https://developers.openai.com/api/docs/models/gpt-5.6-sol)
