# server capsule: HTTP transport and named-Brain request handling

Supplements the root [`AGENTS.md`](../../CLAUDE.md), which still applies in full.

**Owns** `src/server/`: Axum router construction, HTTP authentication and rate limiting,
OpenAI-compatible request/response types, named-Brain HTTP handlers, runner callbacks, approval
bridging, the daemon-side Cap'n Proto RPC adapter and listener lifecycle, and server lifecycle
state. Daemon process lifecycle belongs to `src/daemon`; durable Brain state belongs to
`crates/finch-brain`; the domain-neutral schema/protocol/socket core belongs to `crates/finch-ipc`.

**Facade:** callers outside this directory use flat `crate::server::Item` imports from the
`pub use` list in [`mod.rs`](mod.rs); they must not name `server::handlers` or
`server::openai_types`. The daemon-side `ipc` child is private; the client uses runtime's
delivery-frame encoder directly. #1050 tracks public-signature types missing from the flat
surface. Keep wire behavior, authentication, persistence, and runner semantics unchanged in
facade-only work. Do not recreate a generated symbol catalog.

**Dependencies:** Brain services and persistence, IPC-facing runner callbacks, provider and model
adapters, the program runtime, tool execution, configuration, metrics, and local generation. The
server composes those services; transport handlers must not become their owner.

**Invariants and lifetimes:** the daemon composes one shared `AgentServer` for HTTP and IPC;
the server owns listener/background-task lifetime, while the Brain store owns durable named-Brain
state. Authentication and rate limiting belong at the transport boundary, before a handler
mutates Brain state or dispatches a runner request. An IPC disconnect or retry must not invent
a second committed Brain turn. Keep runner callback and approval protocol changes covered by
the server and supervised IPC tests, not just a helper unit test.

**Extension rule:** put domain-neutral wire contracts in `finch-ipc` and durable Brain rules in
`finch-brain`; add a flat server export only for a real application caller. Do not move root
provider, tool, model, or daemon policy into a transport handler to make a crate split look cheap.

**Focused tests:** `./scripts/test_brains.sh cargo test --lib -- server::` and
`./scripts/test_brains.sh cargo test --test daemon_stdio_binding`. Run `python3 scripts/check_facade_boundaries.py` whenever
the module surface changes, and run the full supervised workspace suite before extraction.
