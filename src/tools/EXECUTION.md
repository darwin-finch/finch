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
| `spawn_task` | Delegate to isolated subagent (Finch's own tools); optional `provider` targets a named configured provider profile instead of the current one |
| `delegate_to_claude_code` | Delegate to the official Claude Code CLI, running as itself |
| memory tools | Semantic memory read/write |

### Sub-brains: named-Brain identity for spawned subagents (`spawn_task`)

`TaskTool` (`src/tools/implementations/spawn.rs`) can be given an optional `finch_brain::BrainStore`
via `with_brain_store`; when omitted, `spawn_task` is fully ephemeral exactly as before. When a
store is present, every `spawn_task` invocation gets its own named Brain — `sub-<generated>`, e.g.
`sub-quiet-hill-a13f09` — created and journaled through the same `BrainStore::push`/`archive` API
the interactive CLI and `finch brain rm` already use. The subagent's task prompt and final
result/error are recorded as ordinary `Prompt`/`Result` journal events (not per-tool-call detail;
that finer-grained recording is unscoped follow-up work). By default the Brain is archived
(`BrainStore::archive`, same as `finch brain rm` — moved to `brains-archive/`, not deleted)
immediately after the subagent finishes, success or failure, so `finch brain ls` never fills with
spawn noise. Passing `persist: true` in the tool input skips that auto-archive, leaving the Brain
live and visible in `finch brain ls` under its `sub-` name. The `sub-` prefix is the only thing
that visually separates spawn-originated Brains from interactively-created ones in `ls` output; no
change to `ls` itself was needed or made. `TaskTool` is registered in the REPL's tool registry and
fallback registry (`src/cli/repl.rs`) as of the provider-selection change above; nothing yet passes
`with_brain_store` at either registration site, so sub-brains are implemented and tested but not
yet wired to a live `BrainStore` in production (tracked as a follow-up, not a dead-code gap).

## Permission system

`PermissionManager` has two roles (chosen through `PermissionManager::new` and
`PermissionManager::for_peer`; the role enum itself is crate-internal to `finch-tools-api`):

- **Owner** — human owner; uses configured per-tool rules (Allow/Ask/Deny)
- **Peer** — AI peer in a room session; asymmetric rules:
  - `read`/`glob`/`grep`: silently Allow
  - `write`/`edit`/`patch`: AskUser (caller converts to DiffPropose event)
  - `bash` (read-only command): silently Allow
  - `bash` (side effects): AskUser
  - `restart`/`spawn`/`delegate_to_claude_code`: always Deny

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
