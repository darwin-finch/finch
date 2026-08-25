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
and the read-only sandbox. Finch sends the complete Brain conversation on every
request, so an app-server thread is never durable conversation truth. The adapter
does not expose Codex's built-in shell, filesystem, or browser tools.

This is an agent-protocol adapter, not raw Chat Completions or Responses API
parity. Finch tool calls require the installed app-server schema to advertise the
experimental `dynamicTools` thread capability. If it does not, Finch rejects that
provider before starting the turn and proceeds to the next configured provider
(for example Grok). Plain text turns remain usable.

The ignored live smoke test is opt-in:

```console
FINCH_LIVE_CHATGPT_APP_SERVER=1 cargo test live_managed_subscription_smoke_test -- --ignored
```

Official references:

- [Codex app-server](https://developers.openai.com/codex/app-server)
- [Codex authentication](https://developers.openai.com/codex/auth)
