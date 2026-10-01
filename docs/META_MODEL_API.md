# Muse Spark through Meta Model API

As of 2026-10-01, Finch has a first-class, hosted-only profile for the direct
Meta Model API Chat Completions surface. It is pinned to
`https://api.meta.ai/v1`, authenticates with a Bearer `MODEL_API_KEY`, and
defaults to the current standard-tier model `muse-spark-1.3`. Finch does not
accept endpoint overrides for this profile, so a Meta credential cannot be
silently sent to another origin.

```toml
[[credentials]]
name = "meta-work"
kind = "api_key"
provider = "meta_model_api"
issuer = "meta-model-api"
secret_ref = "env:MODEL_API_KEY"
scopes = []

[credentials.audience]
family = "meta_model_api"

[credentials.lifecycle]
state = "active"
refreshable = false

[[providers]]
type = "credentialed"
provider = "meta_model_api"
model = "muse-spark-1.3"
name = "muse"
reasoning_effort = "high"

[providers.credential]
credential_ref = "meta-work"
required_scopes = []
```

The direct provider uses `POST /v1/chat/completions`; authenticated discovery
uses `GET /v1/models`. The transport supports streamed text, streamed function
arguments, multiple function calls in one turn, current model metadata, and
the documented Muse Spark reasoning-effort field. Retries are bounded to three
attempts for transport failures, HTTP 429, and 5xx responses; deterministic
4xx errors fail immediately. Stream events, response bodies, tool arguments,
and model metadata are bounded, and provider error bodies are redacted.

These similarly named products are different boundaries:

- **Direct Meta Model API** is the Finch provider above. Its named credential
  is valid only for `api.meta.ai`.
- **OpenCode Zen** is a separate third-party service and credential audience.
  It is not this profile, and Finch does not route or fall back to it.
- **Muse Code** is Meta's terminal coding harness. Finch does not execute it,
  import its login, or reuse its subscription state.
- **Muse Spark weights** are not available as an inspected, supported local
  artifact as of 2026-10-01. This provider makes no Candle, ONNX, offline, or
  local-inference claim. Muse Glimmer's separately published weights do not
  make Muse Spark an open-weight model.
- **Contributor-tier Muse models** have a different data-use contract. Finch
  does not select them implicitly; this change makes no privacy choice for a
  user and supports the standard `muse-spark-1.3` identity only.

Hermetic tests cover the endpoint, Bearer audience, request body, model and
tool identities, fragmented/parallel calls, malformed and oversized events,
cancellation, bounded retry classes, and redaction. A metered live fixture is
present but ignored by default:

```bash
FINCH_LIVE_META_MODEL_API=1 MODEL_API_KEY=... \
  ./scripts/test_brains.sh cargo test -p finch-providers \
  live_meta_model_api_muse_spark_acceptance_is_explicitly_opt_in \
  -- --ignored --exact
```

Do not run that command during ordinary development or CI. Configuration and
mock-server proof are not claims that a particular account has live service
entitlement.

Official sources reviewed 2026-10-01:

- <https://dev.meta.ai/docs/overview>
- <https://dev.meta.ai/docs/protocols/chat-completions>
- <https://dev.meta.ai/docs/api-reference/chat-completions/schemas>
- <https://dev.meta.ai/docs/pricing-rate-limits>
- <https://dev.meta.ai/models/muse-spark>
