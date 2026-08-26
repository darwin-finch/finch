# Finch-native ChatGPT subscription provider

ChatGPT subscription access is a distinct credential and provider kind. It is
not OpenAI Platform API-key access, does not execute Codex, and never reads
`~/.codex`.

## Protocol status

OpenAI's public documentation describes ChatGPT device-code login as beta and
documents `gpt-5.6-sol` on the Platform Responses API. It does not publish the
raw ChatGPT device endpoints or `chatgpt.com/backend-api/codex` transport
contract. Finch therefore treats those details as a versioned compatibility
protocol, not a stable public API.

Public documentation consulted:

- <https://developers.openai.com/api/docs/models/gpt-5.6-sol>
- <https://developers.openai.com/api/reference/cli/resources/responses/methods/create>
- <https://developers.openai.com/codex/auth>

The implementation and fixtures are pinned to OpenAI Codex source commit
`3e4707b34b16e139fcb7ad11ab8445993b62bba1` (retrieved 2026-08-25):

- `codex-rs/login/src/device_code_auth.rs`
- `codex-rs/login/src/server.rs`
- `codex-rs/login/src/auth/manager.rs`
- `codex-rs/login/src/auth/revoke.rs`
- `codex-rs/codex-api/src/common.rs`
- `codex-rs/codex-api/src/endpoint/models.rs`
- `codex-rs/codex-api/src/sse/responses.rs`
- `codex-rs/protocol/src/models.rs`

Finch bounds every response and stream, requires recognized response shapes,
and reports protocol drift rather than interpreting unknown credential or tool
fields permissively. `CHATGPT_PROTOCOL_REVISION` records the source revision in
mock requests. Finch intentionally omits the private `client_version` query:
that field is an upstream Codex compatibility version, and sending Finch's own
semver would be misleading. A catalog entry that requires a minimum upstream
client version fails closed.

## Configuration

Only an opaque reference is stored in `config.toml`:

```toml
[[providers]]
type = "chatgpt"
credential_ref = "chatgpt:default"
model = "gpt-5.6-sol"
name = "subscription"
```

Credentials are named account records rather than per-model fields. Any number
of model or behavior profiles can share one reference because Finch enforces a
fixed authorization endpoint, OAuth client, subscription backend, and named
record boundary. JWT payload fields are parsed only as **unverified
observations** for account continuity; Finch does not claim that their issuer,
audience, subject, tenant, or scopes were signature-verified. Adding another
model does not trigger login. Use distinct references such as `chatgpt:personal` and
`chatgpt:work` for distinct subscriptions. Deleting a profile never deletes or
revokes its shared account record; logout is the explicit credential lifecycle
operation. The Platform provider is a separate credential kind and endpoint
boundary and cannot share a ChatGPT subscription record merely because both
are OpenAI products.

macOS uses modern `SecItem` data-protection Keychain operations with explicit
`AfterFirstUnlockThisDeviceOnly` accessibility, synchronization disabled, an
exact generic-password service/account query, and no widened access group.
Logout updates the item to a non-secret durable tombstone rather than physically
deleting the generation guard. Keychain policy errors fail closed and do not
silently downgrade to a plaintext file. `FileCredentialStore` is the portable injected fallback;
it uses a private 0700 directory, 0600 files, cross-process locking, atomic
replace/fsync, descriptor-bound no-follow reads, hardlink rejection, a
cross-process mutation lease, and random-generation compare-and-swap. Logout
writes a durable tombstone so stale login/refresh tasks cannot resurrect an
account after deletion. Secrets and transient token responses are zeroized on
drop where ownership permits.

The orphaned legacy plaintext `~/.finch/auth/chatgpt.json` record is migrated
once into `chatgpt:default` and removed. A malformed or conflicting legacy
record stops with an explicit recovery error instead of being silently ignored.

OpenAI Platform remains `type = "openai"` with an API key. The factory never
sends a ChatGPT OAuth token to `api.openai.com` and never sends an API key to
the ChatGPT backend.

## Setup-wizard integration contract (#105)

`ChatGptSetupFlow` owns the pending async task, cancellation token, secure
commit, model discovery, and cleanup after failed conformance. The wizard calls
`begin`, renders `pending()`, retains `cancellation_handle()` before moving the
flow into its async `finish` task, routes escape/cancel to that handle, and
persists a profile only from `ChatGptSetupOutcome`.

The setup wizard owner can integrate without owning provider internals:

The equivalent lower-level sequence is:

1. Construct `ChatGptAuth::new(credential_ref)`.
2. Call `begin_device_login()`. Render `PendingDeviceLogin.verification_url`
   and `.user_code`; do not debug-print the object because identifiers are
   intentionally redacted.
3. Call `finish_device_login(&pending, CancellationToken)` in the wizard's
   async task. Cancelling the token promptly ends polling. The method handles
   pending, slow-down, denial, expiry, PKCE exchange, storage, and rotation.
4. Call `account_status()` before save. A `None` result is not authenticated.
5. Construct `ChatGptProvider` and call `available_models()` to validate the
   direct transport. Call `preferred_account_model()` to select
   `gpt-5.6-sol` only when the account advertises it.
6. Persist a `ProviderEntry::Chatgpt` only after those checks succeed.

The equivalent scriptable commands use exactly that implementation:

```text
finch auth login chatgpt [--credential-ref chatgpt:default]
finch auth status chatgpt [--credential-ref chatgpt:default]
finch auth logout chatgpt [--credential-ref chatgpt:default]
```

The provider maps Responses `function_call` and `function_call_output` items
to Finch `ContentBlock::ToolUse` and `ContentBlock::ToolResult`. It never
executes tools or mutates Brain. The caller remains responsible for the common
Finch approval and atomic conversation lifecycle owned by #46/#57/#87.
In particular, the current streaming trait carries chunks but no
provider/model/profile provenance and #46 owns the provisional-delta commit
boundary. This branch buffers tool requests until `response.completed` and
surfaces terminal errors, but must not claim end-to-end atomic persistence until
that shared event API is integrated.

Remote continuation IDs are intentionally not persisted: Brain history is
rebuilt into each request. This makes continuation expendable and provider
scoped and avoids committing provisional provider deltas.

The setup wizard exposes ChatGPT subscription as a distinct provider row. It
runs `ChatGptSetupFlow` on a background runtime, speaks the verification URL and
one-time code as ordinary text, routes Escape through the flow cancellation
token, validates direct model discovery, and creates a profile only after the
account advertises `gpt-5.6-sol`. Existing profiles whose named credential is
missing resume the same login flow. First-run setup, `finch setup`, and `/setup`
all persist through the shared `apply_and_save` path; `finch auth ...` remains
the scriptable equivalent.
