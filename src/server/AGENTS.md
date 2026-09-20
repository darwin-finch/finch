# server capsule: HTTP transport and named-Brain request handling

Supplements the root [`AGENTS.md`](../../CLAUDE.md), which still applies in full.

**Owns** `src/server/`: Axum router construction, HTTP authentication and rate limiting,
OpenAI-compatible request/response types, named-Brain HTTP handlers, runner callbacks, approval
bridging, the daemon-side Cap'n Proto RPC adapter and listener lifecycle, and server lifecycle
state. Daemon process lifecycle belongs to `src/daemon`; durable Brain state belongs to
`src/brain`; the domain-neutral schema/protocol/socket core belongs to `crates/finch-ipc`.

**Facade:** child modules are private. Callers outside this directory use flat
`crate::server::Item` imports from the `pub use` list in `mod.rs`; they must not name
`server::handlers` or `server::openai_types`. Keep wire behavior, authentication, persistence, and
runner semantics unchanged in facade-only work.

**Dependencies:** Brain services and persistence, IPC-facing runner callbacks, provider and model
adapters, the program runtime, tool execution, configuration, metrics, and local generation. The
server composes those services; transport handlers must not become their owner.

**Focused tests:** `./scripts/test_brains.sh cargo test --lib -- server::` and
`cargo test --test daemon_stdio_binding`. Run `python3 scripts/check_facade_boundaries.py` whenever
the module surface changes, and run the full supervised workspace suite before extraction.
