# Harness: Claude Code

Loaded by `$backlog-loop`/`$coordinate` when the active harness is Claude Code, to resolve a packet's `model lane` to an actual model before dispatch. This file names *where* that choice lives and *how* to invoke it — it does not choose a model itself; that stays with whoever installs the kernel into a consuming repo.

## Lane definition

A lane is a named subagent-type definition:

- Personal: `~/.claude/agents/<lane>.md`
- Project: `.claude/agents/<lane>.md`

Required frontmatter: `name` (the lane, e.g. `strong`), `description`. Optional: `model` (pins the subagent to a specific model, tool access, reasoning effort). A consuming repo creates `strong.md`, `default.md`, `cheap.md` here with the `model:` value it chooses.

## Dispatch

Invoke the Agent tool with `subagent_type` set to the packet's lane name. If no agent definition exists for that name yet, an explicit `model` override on the same call is the fallback.

```text
Agent({ subagent_type: "strong", prompt: <packet> })
Agent({ subagent_type: "default", prompt: <packet> })
Agent({ subagent_type: "cheap", prompt: <packet> })
```

## Fallback

If `.claude/agents/<lane>.md` (and `~/.claude/agents/<lane>.md`) do not exist, do not fail the dispatch and do not guess a model. Inherit the current session's model and report `requested <lane>, ran inherited` in the handoff, per [task-packet.md](../references/task-packet.md). Never silently substitute a different named lane.

## Optional boundary-load capture

Claude Code `PostToolUse` command hooks receive JSON on stdin with `hook_event_name`, `cwd`, `tool_name`, and `tool_input`. Configure `.agents/scripts/boundary_path_hook.py` in `.claude/settings.json` with a narrow matcher such as `Read`, and set `SOFTWARE_FACTORY_BOUNDARY_TRACE` to an ignored local trace path. See Anthropic's [Claude Code hooks reference](https://code.claude.com/docs/en/hooks).

The adapter records structured path fields only. Record its trace as `partial` with collector `structured-post-tool-use`, version `1`, harness `claude-code`. Bash commands and unrecognized tool inputs remain unobserved, so this adapter never supplies a complete inspected-path denominator.
