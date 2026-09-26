# Tool Execution System

**Purpose:** Enable AI to inspect and modify code during a session.

## Available tools

| Tool | Purpose |
|------|---------|
| `read` | Read file contents |
| `glob` | Find files by pattern (`**/*.rs`) |
| `grep` | Search with regex |
| `web_fetch` | Fetch URLs |
| `bash` | Execute shell commands |
| `restart` | Rebuild and restart finch itself |
| `spawn_task` | Delegate to isolated subagent (Finch's own tools) |
| memory tools | Semantic memory read/write |

## Permission system

`PermissionManager` has two roles (chosen through `PermissionManager::new` and
`PermissionManager::for_peer`; the role enum itself is crate-internal to `finch-tools-api`):

- **Owner** — human owner; uses configured per-tool rules (Allow/Ask/Deny)
- **Peer** — AI peer in a room session; asymmetric rules:
  - `read`/`glob`/`grep`: silently Allow
  - `write`/`edit`/`patch`: AskUser (caller converts to DiffPropose event)
  - `bash` (read-only command): silently Allow
  - `bash` (side effects): AskUser
  - `restart`/`spawn`: always Deny

Constitutional constraints apply to **both** roles: `rm -rf`, `sudo`, `dd if=`, fork bombs, system file reads, dangerous URL schemes, and private IPs are blocked unconditionally.

`is_readonly_bash()` approves commands that: (1) start with a known safe prefix AND (2) contain no shell operators (`;`, `|`, `>`, `<`, `&`). Operator presence always returns false.

The public surface is the facade in [`mod.rs`](mod.rs); callers outside this directory use
`crate::tools::Item`. The [README](README.md) explains the execution boundary and
[AGENTS.md](AGENTS.md) states its authority rules. Rustdoc supplies callable signatures.

## Key files

- `src/tools/mod.rs` — private children and the `pub use` list
- `crates/finch-tools-api/src/tool_loop.rs` — shared `ToolLoop` admission protocol (REPL + scheduler)
- `src/tools/executor.rs` — `ToolExecutor`, host execution after REPL/scheduler admission or from the legacy direct headless caller
- `src/tools/implementations/` — Individual tool implementations
- `crates/finch-tools-api/src/permissions.rs` — `PermissionManager` and the crate-internal role enum, `is_readonly_bash()`; `src/tools/permissions.rs` is a compatibility re-export
