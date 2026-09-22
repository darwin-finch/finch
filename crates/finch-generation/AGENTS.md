# finch-generation capsule: generation contract, lifecycle, and strategy ports

Supplements the root [`AGENTS.md`](../../CLAUDE.md), which still applies in full.

**Owns** `crates/finch-generation/src/`: the `GenerationBackend` dispatch
boundary, provider-neutral `GenerationRequest` / `GenerationEvent` /
`ToolCall` / `ToolResult` types, readiness and load phases, generation
strategies over shared predictive state, identity/provenance, usage/latency
terminal outcomes, injected environmental ports, generation-layer translation
from `finch-providers::StreamChunk`, and scripted test backends. Those types
are a development seam, not a production wire: change them when justified;
do not persist a parallel copy or bind an external client as if they were frozen.

**Boundary:** [README.md](README.md) traces the external example and lifecycle-test callers.
[`src/lib.rs`](src/lib.rs) is the facade; its child modules are private. Rustdoc renders methods
on exported types. `src/generators` re-exports part of this development contract, but the
production REPL still uses its older `Generator`/`StreamChunk` path. Do not regenerate a
signature catalog or claim production adoption without a traced call path.

**Dependencies:** this unpublished crate depends on `finch-providers` and
async/serde libraries. It never depends on the root `finch` crate, Brain,
TUI, daemon, CLI orchestration, tool execution (`ToolExecutor`), or
application `Config`. Environmental effects are injected through
[`GenerationPorts`](src/ports.rs).

**Surface tiers (issue #959 audit).** The `pub use` list in
[`src/lib.rs`](src/lib.rs) is the cross-crate contract; everything below it is
tiered so implementation detail cannot leak back in:

- **Crate-internal (`pub(crate)`):** `GenerationId::new`,
  `GenerationIdentity::{for_dispatch, with_actual_model}`,
  `TerminalOutcome::identity`, `ReadinessReport::ready`, and
  `GenerationCapabilities::for_strategy`. Widening any of these is a capsule
  change, not cleanup.
- **Module-internal (private):** `validate_model_id`,
  `ProviderGenerationBackend::for_model` (construct through `new`),
  `ResourceBudget::unlimited`, `ScriptedBackend::set_readiness` (callers use
  `set_phase`), and the default port fixtures behind `GenerationPorts::test()`
  (`InstantSleeper`, `TracingProgress`, `UnknownHardware`, `ReadyLoader`,
  `EmptyCache`, `TracingTelemetry`, `InlineScheduler`).
- **Test-only (`#[cfg(test)]`):** `ToolResult::{success, error}` and
  `FrozenMonotonicClock::advance`.

Kept exported though production code never calls them, each with a traced
external-caller seam: `select_backend` (production-boundary selection tests in
`tests/lifecycle.rs`), the `GenerationBackend` trait and methods (dynamic
dispatch through `Arc<dyn GenerationBackend>` in the supervisor and lifecycle
tests), `GenerationMetadata`/`Usage`/`RejectedBackend` (variant and field
types of public items), the eight port traits (public field types of
`GenerationPorts`, whose `clock`/`sleeper` fields lifecycle tests inject
through), `ControllableSleeper`/`FrozenMonotonicClock`/`ScriptedBackend`/
`ScriptedStep` (lifecycle tests and the `scripted_backend` example), and
`SystemMonotonicClock`/`TokioSleeper` (production clock/sleeper constructors;
`GenerationPorts` must stay constructible outside the crate). Deleted as
unreferenced by the same audit: `GenerationEvent::is_terminal`,
`GenerationId::as_uuid`, `GenerationPorts::production`, and
`GenerationRequest::with_tools` (every `.with_tools` hit is a provider-layer
request type, not this one).

**Invariants:**
- Callers use the generation interface without Finch application types.
- Local, cloud, and test backends share the same lifecycle and cancellation
  contract. Terminal outcomes are exactly-once; no post-terminal events.
- A generation-id fence drops stale backend completions after a switch.
- Requested vs resolved vs actual provider/model identity is authoritative
  and secret-free. Invalid model ids error without echoing the value.
- Tool calls become semantic `ToolCall` only after adapter validation.
  Generators never execute tools.
- Opaque reasoning/replay material is not display content.
- Strategies (causal AR, masked/refinement, direct prediction, hybrid) are
  comparable at a matched `ResourceBudget`. Presence of a loader is not
  conformance.
- Provider-specific parsers stay in `finch-providers`. This crate translates
  `StreamChunk` into `GenerationEvent`, including
  `ContentBlockComplete(ToolUse)` → `ToolCallComplete`. OpenAI and Claude
  also emit native `ToolCallDelta`/`ToolCallComplete`; the event loop's
  ToolLoop treats dual encoding of the same id+input as one call.

**Focused tests:**
```bash
./scripts/test_brains.sh cargo test -p finch-generation --lib
./scripts/test_brains.sh cargo test -p finch-generation --test crate_boundary
./scripts/test_brains.sh cargo test -p finch-generation --test lifecycle
./scripts/test_brains.sh cargo test -p finch-generation --example scripted_backend
```

**Agent-context audit:** a worker can implement against the development contract using this
capsule, README, facade, and rustdoc without opening Finch application code. Finch-owned
adapters (Claude, Qwen, daemon-local) stay in `src/generators`.

**Named remainders (not this extraction):**
- IPC projection of native `ThinkingDelta`/`ToolCallDelta` (schema still
  carries tool calls as `ContentBlockComplete`).
- Request construction still uses Claude-shaped `ToolDefinition` schemas as
  semantic identities. `finch-providers` compiles those into per-request
  bijective wire-binding tables (issue #241) at the validated dispatch
  boundary; this crate must keep storing semantic names, not provider aliases.
- Local model architecture rewrite (ONNX/Candle/Qwen internals) is out of scope.
- Rustdoc, not a checked-in generated catalog, supplies trait method signatures, including
  asynchronous `GenerationBackend::generate` and `Sleeper::sleep`.
- `GenerationPorts` progress/loader/cache/telemetry/scheduler are construction
  scaffolding. The supervisor uses clock, sleeper, and hardware metadata.
  Backends do not receive ports on `generate`; stitch them at construction.
  `GenerationEvent::Loading` is for backends to emit; the supervisor reports
  load state via `Readiness`.
- Thread ports into `GenerationBackend::generate` and cancel-on-switch resource
  accounting beyond the generation-id fence (#776 follow-up).
