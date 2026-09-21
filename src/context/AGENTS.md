# context capsule: instruction files and composer mention attachments

Supplements the root [`AGENTS.md`](../../CLAUDE.md), which still applies in full.

**Owns** `src/context/`: project instruction-file assembly (`claude_md`) and
composer `@` mention resolution (`mention`). Instruction files become the
system prompt. Mentions snapshot selected project files/directories and lower
them into structured user-turn context. This is not a published crate.

**Boundary:** the [README](README.md) traces instruction collection and interactive mentions.
[`mod.rs`](mod.rs) is the flat facade; rustdoc renders methods on exported types. Both child
modules stay private. Callers use `crate::context::Item`, not `context::mention::Item` or
`context::claude_md::Item`. Do not recreate a signature catalog.
`python3 scripts/check_facade_boundaries.py` enforces private child modules.

**Dependencies:** `providers` for `ContentBlock` only. Do not execute tools,
spawn processes, or follow credentialed network paths. Directory expansion is
bounded (`MAX_DIR_FILES` / `MAX_DIR_BYTES` / `MAX_FILE_BYTES`).

**Focused tests:** `./scripts/test_brains.sh cargo test --lib -- context::`.

## Two paths that must not be confused

- **Instruction files** (`claude_md.rs`) — load-order `AGENTS.md` → `CLAUDE.md`
  → `FINCH.md` → `CONTEXT.md` → `README.md`. Literal `@path` lines in those
  files are **not** expanded (issue #77).
- **Composer mentions** (`mention.rs`) — token-boundary `@` in the interactive
  composer (and the equivalent tokens on `finch query`). Finch resolves the
  resource and attaches a snapshot. The provider is never asked to interpret
  raw `@path` text.

## Mention policy (v1 files/directories)

- Token-boundary `@` only (start or after whitespace). `\@` is literal.
  `user@host` is not a mention. Leading `@finch` remains the collaborative
  addressee.
- Ignore `.gitignore` / `.ignore` at the project root, plus `.git`, `target`,
  `node_modules`, `__pycache__`, `.venv`. Hidden paths are omitted unless the
  filter starts with `.`. Explicitly mentioned hidden directories (`.github`,
  `.agents`) expand their children; nested hidden names stay omitted unless
  that nested directory is the mention root.
- Reject secrets, binaries, symlink escapes, paths outside the root, and
  oversized files with a speakable diagnostic that names the resource and the
  rule. Do not attach a partial hidden payload. Secret and ignore rules apply
  to the canonical target of an in-root symlink, not only the mention name.
- Snapshot at selection; submit and named-Brain replay use that digest and
  content, never a later disk read.
