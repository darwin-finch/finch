# Harness: Codex

Loaded by `$backlog-loop`/`$coordinate` when the active harness is OpenAI's Codex CLI/app, to resolve a packet's `model lane` to an actual model before dispatch. This file names *where* that choice lives and *how* to invoke it — it does not choose a model itself; that stays with whoever installs the kernel into a consuming repo.

## Lane definition

A lane is a named custom-agent TOML file:

- Personal: `~/.codex/agents/<lane>.toml`
- Project: `.codex/agents/<lane>.toml`

Required fields: `name` (the lane, e.g. `strong`), `description`, `developer_instructions`. Optional: `model` (pins the agent to a specific model), `model_reasoning_effort`, `sandbox_mode`. A consuming repo creates `strong.toml`, `default.toml`, `cheap.toml` here with the `model` value it chooses.

A global fallback lives in `config.toml`'s `[agents]` section:

```toml
[agents]
enabled = true
default_subagent_model = "..."
```

## Dispatch

Spawn a subagent thread naming the custom agent whose file matches the packet's lane. If no custom-agent file exists for that lane, the `[agents].default_subagent_model` default is the fallback; if that is also unset, the parent session's model applies.

## Fallback

If neither `.codex/agents/<lane>.toml` nor `~/.codex/agents/<lane>.toml` exist, do not fail the dispatch and do not guess a model. Inherit the current session's model (or the `[agents]` default, if set) and report `requested <lane>, ran inherited` in the handoff, per [task-packet.md](../references/task-packet.md). Never silently substitute a different named lane.

## Optional boundary-load capture

Codex `PostToolUse` hooks expose structured arguments for shell, patch, MCP, and most local function tools, but the documented hook path is not a complete enforcement boundary and shell arguments are not normalized inspected paths. See the official [Codex hooks documentation](https://learn.chatgpt.com/docs/hooks).

`.agents/scripts/boundary_path_hook.py` can be configured as a `PostToolUse` command hook for path-bearing MCP or local navigation tools. Set `SOFTWARE_FACTORY_BOUNDARY_TRACE` to an ignored local trace path. The adapter appends recognized structured path arguments. Record its result as `partial` with collector `structured-post-tool-use`, version `1`, harness `codex`. Shell commands and unrecognized tool inputs are deliberately not guessed; this adapter alone can never supply the complete inspected-path denominator. `codex_boundary_hook.py` remains as a compatibility entry point.
