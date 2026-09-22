# Finch IPC core

This crate holds the wire contract shared by the frontend and daemon: the Cap'n Proto schema,
protocol generation, Unix socket location, and JSON-value translation. It exists so
both processes agree on transport without making either one's application policy part of the
protocol crate. The frontend owns client connection and request translation; the server owns the
listener, dispatch, and authority checks. Brain and runtime own their domain-specific codecs.

For a frontend request, `src/client/ipc.rs` connects through `sock_path`, checks
`IPC_PROTOCOL_VERSION`, builds generated `finch_ipc_capnp` messages, and uses the JSON-value codec
for tool inputs and decisions. The client owns reconnection behavior and its `IpcClient` lifetime.

For a daemon request, `src/server/ipc.rs` binds the same socket, checks the same generation,
decodes the generated messages, and dispatches them to `AgentServer`. It uses the value codec for
wire values, but authorization and Brain/runtime translation remain outside this crate.

Read [AGENTS.md](AGENTS.md) for boundary and testing rules, [src/lib.rs](src/lib.rs) for the
facade, and `cargo doc -p finch-ipc --no-deps --open` for signatures. The generated Cap'n Proto
namespace is public because its generated self-references require a crate-root module; this is
the exception to the usual flat facade convention.
The event-bus continuation harness is retained only for crate-local tests, not exported to
clients or compiled into the production IPC core.
