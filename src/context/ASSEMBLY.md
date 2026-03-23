# Context Assembly

**Purpose:** Inject project-level AI instructions into the system prompt at startup.

## How it works

`collect_claude_md_context(cwd)` builds the system prompt context by:

1. Reading `~/.claude/CLAUDE.md` — user-level defaults (Claude Code convention)
2. Reading `~/.finch/FINCH.md` — user-level Finch defaults
3. Walking from filesystem root **down** to `cwd`, loading context files from each directory (outermost first; cwd wins)
4. Joining non-empty sections with `\n\n---\n\n`

**Load order within a single directory:** `CLAUDE.md` → `FINCH.md` → `CONTEXT.md` → `README.md`

## Supported filenames

| File | Use |
|------|-----|
| `CLAUDE.md` | Anthropic/Claude Code convention |
| `FINCH.md` | Finch-specific; vendor-neutral |
| `CONTEXT.md` | Tool-agnostic; recommended for new projects |
| `README.md` | General project overview |

## Key files

- `src/context/claude_md.rs` — `collect_claude_md_context()`, `read_non_empty()`
- `src/generators/claude.rs` — `build_system_prompt()`, `ClaudeGenerator`
