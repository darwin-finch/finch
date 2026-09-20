# ipc: Cap'n Proto CLI ↔ daemon transport

Owns the Unix-socket Cap'n Proto client and server that let the interactive CLI talk to the
background daemon, plus the event bus and socket path helpers. It exists as a dedicated transport
layer so wire framing and protocol versioning (`IPC_PROTOCOL_VERSION`) have one place to change,
rather than being duplicated at every caller that needs to reach the daemon.

Ownership, dependencies, and test commands are in [`AGENTS.md`](AGENTS.md); Brain and checkpoint
wire framing specifically live in the nested [`codec`](codec/README.md) facade.
