# finch-tools-api: the dependency-free tool surface

This crate is the shared vocabulary every application layer (CLI, runtime, scheduler, providers)
uses to talk about tools: the `Tool` trait and registry, typed requests/results, the permission
and approval policy (including the peer hard-deny/silent-allow tables and `is_readonly_bash`), the
declared-effect vocabulary, and the tool-round protocol. It exists as a dependency-free crate — it
may not name `cli`, `server`, `runtime`, `local`, `models`, `brain`, or `programs` — so that
security-critical permission logic can be reasoned about and tested in isolation from the rest of
the application. It contains no concrete tool implementations and no executor; those stay with the
composition root in `src/tools`.

Ownership, dependencies, invariants, and test commands are in [`AGENTS.md`](AGENTS.md); the exact
Rust surface is [`src/lib.rs`](src/lib.rs).
