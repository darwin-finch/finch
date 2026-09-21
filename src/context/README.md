# Finch project context

This module owns two distinct ways project files enter a turn: ordered instruction files in
the system prompt and explicitly selected `@` mentions attached to a user prompt. It does not
execute tools or read provider credentials. Mention selection snapshots content and digest at
selection time; later submission and Brain replay must not re-read a changed file.

When a cloud generator starts, `src/generators/claude.rs` calls `collect_instructions` with the
working and home directories, then includes the collected text in the provider system prompt.
The instruction collector owns precedence and duplicate-file handling; the generator owns prompt
assembly. Literal `@path` lines inside instruction files are not composer mentions.

For interactive `@` selection, `src/cli/mention_session.rs` uses `MentionCatalog` and
`snapshots_for_prompt` to resolve bounded project resources and hands immutable attachments to
the TUI/Brain submission path. The CLI owns selection state and submission timing; this module
owns ignore, secret, symlink, and byte-bound policy. The one-shot `finch query` path enters
through `prepare_prompt_for_query` on the same facade.

Read [AGENTS.md](AGENTS.md) for safety rules, [mod.rs](mod.rs) for the flat callable surface,
and root-package rustdoc for methods. [ASSEMBLY.md](ASSEMBLY.md) details instruction-file
precedence and current limitations.
