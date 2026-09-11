# Context Assembly

**Purpose:** Inject project-level AI instructions into the system prompt.

## How it works

`collect_instructions(cwd, home)` returns an `InstructionSources` value: the assembled text plus
every instruction file found and what happened to it. `collect_claude_md_context(cwd)` is the
same collection using the real home directory, returning only the text.

`ClaudeGenerator` collects once at construction (`ClaudeGenerator::new` uses the process working
and home directories; `ClaudeGenerator::new_in` takes them explicitly) and sends the text as the
`## Project Instructions` part of every request's system prompt. That covers the interactive
cloud path and runtime sub-agents built for cloud profiles.

## Precedence

Lowest first; later sections win:

1. `~/.claude/CLAUDE.md` — user-level defaults (Claude Code convention)
2. `~/.finch/FINCH.md` — user-level Finch defaults
3. Each directory from the filesystem root **down** to `cwd`, outermost first, so the working
   directory wins over its ancestors. The walk goes all the way to `/`, so `~/AGENTS.md` and
   `/AGENTS.md` are loaded as ancestor files when present.

Within one directory: `AGENTS.md` → `CLAUDE.md` → `FINCH.md` → `CONTEXT.md` → `README.md`.
`AGENTS.md` is the cross-tool convention, so Finch- and Claude-specific files refine it.
`README.md` is overview context, not instructions: it keeps its last position only for
compatibility and has no authority to override the other files.

Sections are joined with `\n\n---\n\n`, and each begins with a line naming its source path.

## One file, one section

A file reached by more than one name — a symlink such as this repository's `AGENTS.md` →
`CLAUDE.md`, a hardlink, or a user-level file linked into a project — is included once, at its
last (highest-precedence) position. Identity is device and inode on Unix and the canonical path
elsewhere. Two distinct files with identical text are both included.

## Bounds and provenance

Files larger than 256 KiB (`MAX_INSTRUCTION_FILE_BYTES`) are skipped rather than truncated
mid-rule. Every existing candidate is reported in `InstructionSources::sources` with a status:
`Loaded`, `Empty`, `SupersededBy(path)`, `TooLarge`, or `Unreadable`.

## Not yet supported

- `@path` imports are not expanded. Because `AGENTS.md` is read directly, a `CLAUDE.md` that
  contains `@AGENTS.md` still gets the rules once, plus a literal line.
- Local override files (`CLAUDE.local.md`, `AGENTS.override.md`, and similar) are not read.
- Only the working directory's ancestor chain is read; files in other subdirectories are not
  discovered on demand. Start Finch inside a subdirectory to load its instructions.
- `finch query`, `finch agent`, and local-model generators do not yet receive these
  instructions.

These remain open on issue #77.

## Key files

- `src/context/claude_md.rs` — `collect_instructions()`, `InstructionSources`
- `src/generators/claude.rs` — `ClaudeGenerator::new_in()`, `build_system_prompt()`
