# Native ChatGPT subscription transport

Finch's `chatgpt_subscription` provider is a Finch-owned HTTP transport. It does
not execute Codex, app-server, or any other provider binary, and it never reads
another application's credential store. It is intentionally separate from the
OpenAI Platform and generic OpenAI-compatible transports.

## Versioned compatibility contract

The public Responses API documents the message, image, function, encrypted
reasoning, and streaming item shapes used here:

- <https://developers.openai.com/api/reference/resources/responses/methods/create>
- <https://developers.openai.com/api/docs/models/gpt-5.6-sol>

ChatGPT subscription routing is a compatibility contract derived from the
public OpenAI Codex source at commit
`6478a751fde8884b2fdc76486fe23175a8e795d4`. The relevant source files are:

- `codex-rs/codex-api/src/endpoint/responses.rs`
- `codex-rs/codex-api/src/endpoint/models.rs`
- `codex-rs/codex-api/src/sse/responses.rs`
- `codex-rs/codex-api/src/rate_limits.rs`
- `codex-rs/core/src/client.rs`
- `codex-rs/protocol/src/openai_models.rs`

Finch records that pin as
`openai-codex-responses-lite@6478a751fde8884b2fdc76486fe23175a8e795d4`.
Protocol drift fails closed; it is not silently treated as Platform behavior.
Catalog discovery sends `client_version=0.151.0`, and both catalog and
inference requests send `version: 0.151.0`. This is the released Codex
compatibility version associated with that audited source revision, not
Finch's application version or a claim that Finch is Codex. Updating either
compatibility pin requires auditing the newer public Codex catalog and
Responses-Lite contracts, updating both pins together, and rerunning the
focused tests plus the explicit live acceptance test.

## Identity and routing

Only a named `ChatgptSubscription` credential created by Finch's device-login
flow can select this provider. Its issuer, audience, account, store reference,
revision, and generation are revalidated before reuse and after refresh.

The production origin is exactly `https://chatgpt.com` with these routes:

- `GET /backend-api/codex/models?client_version=0.151.0`
- `POST /backend-api/codex/responses`

Requests use bearer authorization, `ChatGPT-Account-ID`, honest
`originator: finch`, the pinned compatibility `version`, and the pinned
protocol revision. Application identity remains separate: both routes use the
bounded static client identifier
`finch/<version> (+https://darwin-finch.github.io/)` as their
`User-Agent`, where `<version>` is Finch's package version. It contains no
username, hostname, Brain/session, account, or credential identifier. This
provides honest client identification and project discoverability only; it
does not guarantee that OpenAI exposes telemetry or metrics to Finch. Sol
requests additionally send
`x-openai-internal-codex-responses-lite: true`.
Redirects, custom endpoints, userinfo, fragments, FedRAMP, Platform fallback,
and model fallback are rejected.

The account catalog is bounded and cached by account plus credential
generation. ETags may revalidate an expired cache, but an entry from another
account is never reused. A requested `gpt-5.6-sol` or `gpt-5.6` entry must be
explicitly advertised with text, image, API, and Responses-Lite support before
inference.
Their numeric `context_window` values are authoritative account-catalog
metadata when they fall in the defensive range `1..=10,000,000`; Finch does
not treat one exact window size as a dialect or authorization discriminator.
Missing, non-numeric, zero, and excessive values fail closed without exposing
the catalog body or account credentials. The synchronous capability descriptor
reports the context window as unknown because it cannot perform credentialed
account discovery; the validated catalog retains the exact value for each
selectable model. The service may advertise either pinned identifier without
advertising both. Finch requires the exact configured/requested identifier to
be present and compatible before inference, so an alias is never silently
substituted and an unrecognized slug is never accepted.

## Request and continuation semantics

Every request uses `store: false`, `stream: true`,
`include: ["reasoning.encrypted_content"]`, and
`reasoning.context: "all_turns"`. Reasoning effort is explicit. Instructions
and tools are Responses-Lite developer items; user/assistant text, validated
PNG/JPEG inputs, function calls, and function outputs retain their history
order.

The request prefix follows the audited Codex v0.151.0 Responses-Lite shape:
ordinary function definitions are nested under the `functions` namespace, and
the `additional_tools` and developer-instructions items carry deterministic
UUID-v5 IDs with `at_` and `msg_` prefixes. Codex scopes those payload-derived
IDs to its thread UUID. Finch does not expose a Codex thread at this provider
boundary, so it scopes them to the pinned protocol revision instead; identical
visible prefix payloads therefore keep identity across retries without putting
a username, Brain, account, credential, or host identifier on the wire. The
pin must be re-audited if OpenAI changes the prefix identity contract.

The audited client also sends fields tied to Codex-owned thread and client
state, including `prompt_cache_key` and `client_metadata`. Finch does not
fabricate those values: they remain absent until Finch has an independently
defined, reviewed contract for them.

Encrypted reasoning is provider-owned opaque data. Finch bounds it, persists it
as an ordered content block through the atomic conversation path, and replays it
byte-for-byte. Finch never renders or interprets it. The IPC generation is 8;
generation 6 peers reject the typed image and complete-content stream schema before query or
stream work.

## Streaming and failure policy

The SSE parser bounds each line and event and the total response. It validates
standard `event:` names against JSON event types, requires strictly increasing
sequence numbers, accumulates completed output items by contiguous output
index, and reconciles text deltas, text-completion events, completed items, and
any terminal output. Actual-model provenance must remain compatible. Effective
model headers take precedence over the response-model fallback, and the audited
transition to a `-safety-routed` provenance suffix is accepted; contradictory
effective headers still fail closed. Bounded
`safety_buffering` event metadata is validated and intentionally not projected.
Executable tool output produces a `tool_use` stop reason. An explicit
`end_turn` value that contradicts the presence of executable tool output fails
closed. Finch currently has no provider-neutral
follow-up signal for a text-only response, so that combination fails visibly
instead of being silently finalized. Unknown fields/events, malformed tool
arguments, a terminal marker before completion, missing completion, partial EOF
before completion, idle timeout, receiver drop, and payload-limit violations
fail visibly before terminal chunks are published. A validated
`response.completed` event is authoritative and returns immediately, matching
the pinned Codex client; unread suffix data, including the optional `[DONE]`
marker, is discarded independently of HTTP chunk boundaries.

Non-success bodies are never consumed after their status is known; the response
is dropped immediately so a hostile or broken body cannot delay the typed
result. A Responses-Lite rejection retains a typed HTTP status and a
compatibility or entitlement hint, never the response body. Tokens,
account identifiers, request bodies, image data, tool arguments, reasoning
continuations, and response bodies are never placed in provider errors.

Proactive refresh is generation-bound and serialized. A 401 before the stream
starts permits one shared refresh, a fresh account catalog check, and one retry.
There is no retry after successful stream headers or any response event.

## Live acceptance

Live auth and inference are never part of automated tests. After building the
reviewed branch, a user who has explicitly completed Finch's own device login
can exercise the production Finch boundary with a disposable Brain name:

```sh
cargo build --bin finch
target/debug/finch --brain chatgpt-subscription-live-acceptance
```

This uses Finch's normal credential selection and provider path; it does not
copy credentials into a test HOME. Enter a short prompt and confirm a response
whose displayed provider/model provenance is the selected ChatGPT subscription
profile. Manual evidence supplements but never replaces the deterministic
HTTP/SSE regression suite. Until this production-boundary check is run,
live-service acceptance—including current account entitlement and server-side
compatibility with the pinned revision—remains intentionally unverified.
