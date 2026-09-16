# finch-generation capsule: generation contract, lifecycle, and strategy ports

Supplements the root [`AGENTS.md`](../../CLAUDE.md), which still applies in full.

**Owns** `crates/finch-generation/src/`: the `GenerationBackend` dispatch
boundary, provider-neutral `GenerationRequest` / `GenerationEvent` /
`ToolCall` / `ToolResult` types, readiness and load phases, generation
strategies over shared predictive state, identity/provenance, usage/latency
terminal outcomes, injected environmental ports, generation-layer translation
from `finch-providers::StreamChunk`, and scripted test backends.

**Interface:** [`INTERFACE.md`](INTERFACE.md) is generated from `src/lib.rs`.
Child modules are private; the `pub use` list is the whole public surface.
Finch consumes this crate through `src/generators`.

**Documentation:** [`docs/README.md`](docs/README.md) owns crate-local
reference material.

**Dependencies:** this unpublished crate depends on `finch-providers` and
async/serde libraries. It never depends on the root `finch` crate, Brain,
TUI, daemon, CLI orchestration, tool execution (`ToolExecutor`), or
application `Config`. Environmental effects are injected through
[`GenerationPorts`](src/ports.rs).

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

**Agent-context audit:** a worker can understand, implement against, and test
this crate from this capsule plus `INTERFACE.md` without opening Finch
application code. Finch-owned adapters (Claude, Qwen, daemon-local) stay in
`src/generators`.

**Named remainders (not this extraction):**
- IPC projection of native `ThinkingDelta`/`ToolCallDelta` (schema still
  carries tool calls as `ContentBlockComplete`).
- Request construction still uses ad-hoc Claude-shaped `ToolDefinition`
  schemas; bijective provider tool-binding tables are #241.
- Local model architecture rewrite (ONNX/Candle/Qwen internals) is out of scope.
- `generate_interfaces.py` matches `fn`, not `async fn` (hygiene Issue 6).
  `GenerationBackend::generate` and `Sleeper::sleep` are therefore absent from
  `INTERFACE.md`. Read the trait in `src/backend.rs` / `src/ports.rs` for those
  methods until the generator is fixed.
- `GenerationPorts` progress/loader/cache/telemetry/scheduler are construction
  scaffolding. The supervisor uses clock, sleeper, and hardware metadata.
  Backends do not receive ports on `generate`; stitch them at construction.
  `GenerationEvent::Loading` is for backends to emit; the supervisor reports
  load state via `Readiness`.
- Thread ports into `GenerationBackend::generate` and cancel-on-switch resource
  accounting beyond the generation-id fence (#776 follow-up).
