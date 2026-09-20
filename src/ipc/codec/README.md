# ipc/codec: Brain and checkpoint framing

Owns the codecs that translate Brain, runtime, and VM domain types into the generated Cap'n Proto
schema and back: the Brain remote-envelope codec and the closed typed-runtime checkpoint codec. It
exists as a nested facade (the same pattern as `tools/mcp` inside `tools`) so wire-byte stability
for these two specific translations is reviewed and tested apart from RPC dispatch and socket
transport, which stay on the parent `ipc` facade.

Ownership, dependencies, and test commands are in [`AGENTS.md`](AGENTS.md).
