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
| `spawn_task` | Delegate to isolated subagent |
| memory tools | Semantic memory read/write |

## Permission system

`PermissionManager` has two roles:

- **`ExecutorRole::Owner`** — human owner; uses configured per-tool rules (Allow/Ask/Deny)
- **`ExecutorRole::Peer`** — AI peer in a room session; asymmetric rules:
  - `read`/`glob`/`grep`: silently Allow
  - `write`/`edit`/`patch`: AskUser (caller converts to DiffPropose event)
  - `bash` (read-only command): silently Allow
  - `bash` (side effects): AskUser
  - `restart`/`spawn`: always Deny

Constitutional constraints apply to **both** roles: `rm -rf`, `sudo`, `dd if=`, fork bombs, system file reads, dangerous URL schemes, and private IPs are blocked unconditionally.

`is_readonly_bash()` approves commands that: (1) start with a known safe prefix AND (2) contain no shell operators (`;`, `|`, `>`, `<`, `&`). Operator presence always returns false.

The public surface is the facade in [`mod.rs`](mod.rs); callers outside this directory use
`crate::tools::Item`. See the capsule [`AGENTS.md`](AGENTS.md); `mod.rs` itself has the exact
signatures.

## Key files

- `src/tools/mod.rs` — private children and the `pub use` list
- `src/tools/tool_loop.rs` — event-loop-owned `ToolLoop` protocol (REPL + scheduler)
- `src/tools/executor.rs` — `ToolExecutor`, host execution after ToolLoop admits a call
- `src/tools/implementations/` — Individual tool implementations
- `src/tools/permissions.rs` — `PermissionManager`, `ExecutorRole`, `is_readonly_bash()`
