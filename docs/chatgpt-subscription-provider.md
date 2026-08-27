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

Finch creates a private, named Codex home for this profile. App-server writes and
refreshes its own file-mode credentials there; Finch never opens `auth.json`.
The profile is held through a descriptor-relative private directory and receives
a minimal atomically rewritten configuration, so ambient `HOME`/`CODEX_HOME`
plugins, apps, MCP servers, hooks, skills, memories, profiles, instructions, and
history are not inherited.

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
Finch resolves a canonical owner-controlled self-contained native Codex binary,
opens it without following links, and copies the bytes from that same held
descriptor into a private immutable staging directory. Schema inspection and all
runtime processes use that staged identity. Finch also reads the effective config
and managed requirements over app-server before authentication or text operations,
and rejects any enabled or unknown action surface.

The profile default is `gpt-5.6-sol`; the official `gpt-5.6` alias currently routes
to that model. Finch does not substitute the lower-cost Terra tier implicitly.

This is an agent-protocol adapter, not raw Chat Completions or Responses API
parity. The current stable adapter is text-only and does not opt into app-server's
experimental API. Finch-owned tool and approval projection remains gated on the
exact-once integration work in issues #46 and #157. Requests carrying Finch tools
therefore skip this provider and proceed to the next configured provider. Text-only
turns also fall back when the required restricted sandbox boundary is unavailable.

The ignored live smoke test is opt-in:

```console
FINCH_LIVE_CHATGPT_APP_SERVER=1 cargo test live_managed_subscription_smoke_test -- --ignored
```

Official references:

- [Codex app-server](https://developers.openai.com/codex/app-server)
- [Codex authentication](https://developers.openai.com/codex/auth)
- [GPT-5.6 Sol](https://developers.openai.com/api/docs/models/gpt-5.6-sol)
