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
Finch resolves the npm launcher only as package metadata and never executes it.
Before schema inspection, authentication, config reads, or a turn, Finch resolves
the packaged native binary and checks the launcher, both package manifests, package
provenance record, and native binary against one immutable audit tuple. That tuple
includes the package version, package and native SHA-256 digests, and (on macOS) the
signing Team ID and designated requirement. Runtime invokes that exact native binary
directly. Finch also rejects writable installation ancestors and revalidates every
pinned file before spawn. No Codex release is currently on the allowlist: a release
must be audited end-to-end for per-thread merged config semantics, effective built-in
tools, managed configuration precedence, environments, process environment, and
sandbox enforcement before this provider becomes available. This deliberate
fail-closed compatibility limit prevents a future schema-compatible release from
being trusted automatically.

## Adding an audited Codex release

`AUDITED_CODEX_ARTIFACTS` is intentionally an exact, source-controlled allowlist,
not a version range. To add a release, an independent audit must record and review:

- the exact `@openai/codex` package version and registry integrity/provenance;
- SHA-256 digests for the launcher, main manifest, platform manifest, packaged
  provenance record, and native binary (combined exactly as the implementation
  defines `package_digest`);
- the native binary's SHA-256 digest and, on macOS, its Team ID and full designated
  requirement;
- generated app-server schemas proving the restricted `readOnly` access variant
  and any advertised dynamic-tool protocol;
- end-to-end fixtures proving merged per-thread config disables every enumerated
  app, plugin, MCP, environment, profile, hook, agent, shell, skill dependency
  installer, web/browser/computer capability, and network/filesystem escape.

The reviewed values are then added as one `AuditedArtifact` tuple. Finch requires
the native path to be root-owned and immutable to the invoking user, opens it during
resolution, validates and signs/hashes that open object, and requires both the open
descriptor and the path to retain the audited identity immediately before every
schema, auth, or turn spawn. This OS-enforced immutability closes the same-UID
path-to-exec replacement window on supported platforms. Merely installing a newer
version or matching its schema never expands the allowlist.

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
