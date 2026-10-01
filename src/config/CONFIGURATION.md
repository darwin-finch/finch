# Configuration

**Config file:** `~/.finch/config.toml`

Optional `default_provider = "profile-name"` names the global default new Brains inherit once. Changing it does not rewrite existing Brain overlays.

## Local display features

Provider reasoning is hidden by default. To show a sanitized, bounded live
reasoning disclosure for configured `openai_compatible` profiles only:

```toml
[features]
display_model_reasoning = true
```

This is a local presentation preference. It does not add or change provider
request fields, and reasoning is not added to conversation history, Brain
state, checkpoints, logs, or native terminal scrollback. The live body is
discarded on a fresh attach or restart.

## Format — `[[providers]]`

```toml
[[providers]]
type = "claude"
api_key = "sk-ant-..."
model = "claude-sonnet-4-6"   # optional override

[[providers]]
type = "local"
inference_provider = "llama_cpp"
execution_target = "auto"    # "auto" (Metal on supported Macs) | "cpu"
model_family = "qwen2"
model_size = "medium"         # descriptive size hint; GGUF file supplies the weights
model_path = "/absolute/path/to/chat-model.gguf"
enabled = true

```

The setup wizard can enter the same absolute GGUF path and checks that it exists before adding
it. Leaving the field blank for a catalogued Qwen, Gemma, or Llama selection instead persists an
immutable managed-artifact identity (repository, commit, filename, quantization, byte size, and
SHA-256). On daemon startup Finch resumes the download through Hugging Face, verifies it, and
loads the cached path. The optional top-level `huggingface_token` is used first; standard hf-hub
environment/cache credentials remain the fallback. These settings are for daemon chat LLMs only; the frontend memory
subsystem selects and downloads its own models. ONNX and Candle are no longer chat providers.
The daemon currently requires at least one configured cloud provider as a fallback alongside local models.
Normal startup rejects a legacy entry without changing the file. Running `finch setup` creates a
private adjacent `config.toml.pre-gguf-migration.bak` backup (using a numeric suffix rather than
overwriting an earlier backup), removes only the affected local `[[providers]]` block or legacy
`[backend]` block plus obsolete `[coreml]` sections, and preserves unrelated providers,
credentials, settings, and comments before opening the editor. An old repository or model file is
never treated as a GGUF path.

Automatic training is disabled and there are no active `auto_train` settings.
Explicit feedback is retained privately in `~/.finch/feedback.jsonl` without
triggering training. Existing legacy training queues and adapters are left
untouched.

**Configured `type` values:** `claude`, `openai`, `grok`, `gemini`, `mistral`, `groq`, `ollama`,
`openai_compatible`, `remote_daemon`, and `local`. The old standalone
`type = "chatgpt_subscription"` shape still deserializes only to produce
migration guidance and is rejected before provider construction. Current
ChatGPT subscription support is configured as `type = "credentialed"` with
`provider = "chatgpt_subscription"`.

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

### Generic OpenAI-compatible endpoints

A generic endpoint is an explicitly named profile, not an alias for OpenAI.
Its model capabilities are operator assertions: omitted values remain unknown
and therefore fail closed when a request needs them. Secrets stay in the named
credential store.

```toml
[[credentials]]
name = "ciru-key"
kind = "api_key"
provider = "openai_compatible"
issuer = "openai-compatible"
secret_ref = "env:CIRU_API_KEY"
scopes = []

[credentials.audience]
family = "custom"
endpoint = "https://dunamis.ciru.ai"

[credentials.lifecycle]
state = "active"
refreshable = false

[[providers]]
type = "openai_compatible"
name = "ciru"
base_url = "https://dunamis.ciru.ai/v1"
chat_path = "/chat/completions"
models_path = "/models"
model = "main"
tool_choice = "auto"
strict_tool_schemas = false

[providers.credential]
credential_ref = "ciru-key"
required_scopes = []

[providers.capabilities]
streaming = true
tools = true
parallel_tool_calls = false
image_input = false
context_window_tokens = 262144
max_output_tokens = 32768
```

The credential audience binds the secret to the normalized endpoint origin;
cross-origin chat or model paths are rejected. `tool_choice = "auto"` and
`strict_tool_schemas = false` are sent only when tools are present. The
streaming path consumes OpenAI Chat Completions SSE and converts native tool
call deltas through Finch's normal tool-binding validation.

`finch setup` exposes the same shape as **Generic OpenAI-compatible**. Its
connection screen records the profile, endpoint, model, and the name of an
environment variable containing the secret; the secret itself is never
written to `config.toml`. Its capability screen starts every assertion as
unknown and only persists support the operator explicitly selects. For the
Ciru-shaped example above, enter `CIRU_API_KEY` as the secret environment
variable, not the key value. Completing setup validates and reloads the
configuration but does not probe the service or claim live conformance. Ciru
conformance can be established only by the opt-in, credential-gated
Ciru/Dunamis live fixture tracked by #1440 (generic OpenAI-compatible provider
live conformance); configuration and hermetic mock fixtures are not a live
service attestation.

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
session) is skipped, and commands matched by the built-in dangerous-input
denylist are never executed.
Shell operators and command substitution are rejected at load. LSP-server
sources are not accepted yet; unknown keys fail closed at parse time.

## Key files

- `src/config/mod.rs` — Config loading, validation, migration; re-exports credential types from `finch-providers`
- `crates/finch-providers/src/credentials.rs` — named credential schema and binding validator
- `src/config/provider.rs` — `ProviderEntry` tagged enum
- `src/config/settings.rs` — `LicenseConfig`, `LicenseType`
- `src/config/diagnostics.rs` — declared post-edit diagnostics sources (`DiagnosticsConfig`)
