# context: instruction files and composer mentions

This owns two related but distinct things: project instruction-file assembly (the `AGENTS.md` →
`CLAUDE.md` → `FINCH.md` → `CONTEXT.md` → `README.md` load order that becomes the system prompt)
and composer `@` mention resolution (snapshotting files/directories a user references into
structured turn context). Both exist here, rather than in `config`, because both are about
*loading content into a request*, not about settings.

Ownership, dependencies, and test commands are in [`AGENTS.md`](AGENTS.md).

## Further documentation

[`ASSEMBLY.md`](ASSEMBLY.md) — the load order and deduplication rules, pinned by the root
[Context invariant](../../CLAUDE.md#context).
