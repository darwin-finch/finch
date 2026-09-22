# Configuration

**Config file:** `~/.finch/config.toml`

Optional `default_provider = "profile-name"` names the global default new Brains inherit once. Changing it does not rewrite existing Brain overlays.

## Format — `[[providers]]`

```toml
[[providers]]
type = "claude"
api_key = "sk-ant-..."
model = "claude-sonnet-4-6"   # optional override

[[providers]]
type = "local"
inference_provider = "onnx"
execution_target = "coreml"   # "coreml" | "cpu"
model_family = "qwen2"
model_size = "medium"         # small=1.5B medium=3B large=7B xlarge=14B
enabled = true

```

Automatic training is disabled and there are no active `auto_train` settings.
Explicit feedback is retained privately in `~/.finch/feedback.jsonl` without
triggering training. Existing legacy training queues and adapters are left
untouched.

**Configured `type` values:** `claude`, `openai`, `grok`, `gemini`, `mistral`, `groq`, `ollama`,
`remote_daemon`, and `local`. The legacy `chatgpt_subscription` value still deserializes only to
produce migration guidance and is rejected before provider construction.

**Backwards-compatible:** The removed legacy `[[teachers]]` format still loads (one private migration shim); saves write `[[providers]]` only.

## Named provider credentials

New profiles can reference a reusable, secret-free credential record. Secret
material is resolved through the configured credential store; the built-in
resolver accepts explicit `env:VARIABLE_NAME` references.

```toml
[[credentials]]
name = "openai-work"
kind = "api_key"
provider = "openai_platform"
issuer = "openai-platform"
secret_ref = "env:OPENAI_WORK_API_KEY"
scopes = []

[credentials.audience]
family = "openai_platform"

[credentials.lifecycle]
state = "active"
refreshable = false

[[providers]]
type = "credentialed"
provider = "openai_platform"
model = "gpt-5.6-sol"
name = "work-reasoning"

[providers.credential]
credential_ref = "openai-work"
required_scopes = []
```

One named credential can serve multiple model profiles when provider, kind,
issuer, normalized audience, tenant/project/account constraints, scopes, and
lifecycle all match. Resolution is once per credential while constructing the
immutable provider graph.

OpenAI Platform and ChatGPT subscription are different credential providers
and audiences. An OpenAI Platform API key cannot be used as a ChatGPT session,
and a subscription session cannot be sent to the Platform API. Finch currently
supports the documented Platform API-key transport; it does not fabricate a
ChatGPT device flow.

For standard provider URLs, Finch normalizes scheme, host, and port and binds
the credential to the provider's standard endpoint family. A custom base URL
requires `family = "custom"` and its exact normalized origin in `endpoint`;
labeling a custom host as a standard audience is rejected.

Existing `api_key` fields remain supported as explicitly provider-local legacy
configuration. Finch does not silently rewrite, share, or reinterpret those
secrets. To migrate safely, create a named credential with the exact provider,
issuer, audience, and account metadata, move the secret to the referenced
store (for example an environment variable), then replace the old provider
entry with `type = "credentialed"`. Ambiguous legacy named records fail with an
actionable `finch setup` migration error.

## Post-edit diagnostics — `[diagnostics]` (issue #757)

Diagnostics after write/edit/patch run only from a source declared here;
nothing is inferred from project files. Empty by default (the feature is
inert and edit results are unchanged):

```toml
[diagnostics]
timeout_secs = 10        # per-run bound; a hanging check is stopped, the edit is unaffected
max_output_chars = 4000  # cap on the excerpt appended to the edit result

[[diagnostics.check]]
extensions = ["rs"]
command = "cargo check --message-format=json"
```

Each `check` entry maps file extensions to a simple argv that is executed
directly — no shell — in the workspace root after a successful write/edit/patch
of a covered file. The result of that check is appended, bounded, to the same
tool result. The command's authority is evaluated through the existing bash
approval path: a command your bash policy would not allow (including a peer
session) is skipped, and constitutionally denied commands are never executed.
Shell operators and command substitution are rejected at load. LSP-server
sources are not accepted yet; unknown keys fail closed at parse time.

## Key files

- `src/config/mod.rs` — Config loading, validation, migration; re-exports credential types from `finch-providers`
- `crates/finch-providers/src/credentials.rs` — named credential schema and binding validator
- `src/config/provider.rs` — `ProviderEntry` tagged enum
- `src/config/settings.rs` — `LicenseConfig`, `LicenseType`
- `src/config/diagnostics.rs` — declared post-edit diagnostics sources (`DiagnosticsConfig`)
