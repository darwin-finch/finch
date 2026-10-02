# Harness: Grok Build

Loaded by `$backlog-loop`/`$coordinate` when the active harness is xAI's Grok Build, to resolve a packet's `model lane` to an actual model before dispatch. This file names *where* that choice lives and *how* to invoke it — it does not choose a model itself; that stays with whoever installs the kernel into a consuming repo.

## Lane definition

A lane is a named subagent-type definition:

- `.grok/agents/<lane>.md` declares the subagent type (e.g. `strong`), extending the built-in `general-purpose` / `explore` / `plan` types.
- `.grok/config.toml` routes it to a model, e.g. `[subagents.roles.<lane>] model = "..."` (or the equivalent `[subagents.models]` table — confirm the current key against `docs.x.ai/build` before relying on it; this schema was verified once and doc formats drift).

A consuming repo creates the `<lane>.md` files and the matching `config.toml` entries with the `model` value it chooses.

## Dispatch

The model calls its `spawn_subagent` tool with `subagent_type` set to the packet's lane name; Grok Build resolves that name through the config above. Nesting is capped at depth 1 — a dispatched worker cannot itself spawn a further subagent.

## Fallback

If no `.grok/agents/<lane>.md` or matching `config.toml` route exists, do not fail the dispatch and do not guess a model. Inherit the current session's model (a subagent inherits its parent's model when nothing else is set) and report `requested <lane>, ran inherited` in the handoff, per [task-packet.md](../references/task-packet.md). Never silently substitute a different named lane.

## Optional boundary-load capture

Grok Build project hooks live in `.grok/hooks/*.json`; it also reads `.claude/settings.json`. Native `PostToolUse` events send camelCase `hookEventName`, `cwd`/`workspaceRoot`, `toolName`, and `toolInput`. Configure `.agents/scripts/boundary_path_hook.py` with a narrow matcher such as `Read`, set `SOFTWARE_FACTORY_BOUNDARY_TRACE` to an ignored local trace path, and trust the project hook through Grok's normal hook-trust flow. See xAI's [Grok Build hooks reference](https://docs.x.ai/build/features/hooks).

The adapter accepts native camelCase and Claude-compatible snake_case inputs but records structured path fields only. Record its trace as `partial` with collector `structured-post-tool-use`, version `1`, harness `grok-build`. Bash commands and unrecognized tool inputs remain unobserved, so this adapter never supplies a complete inspected-path denominator.
