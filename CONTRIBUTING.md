# Contributing to Finch

Thank you for improving Finch. The project accepts focused fixes, tests, documentation, and design
discussion. Before starting a large change, open or join an issue so implementation and conformance
work can be coordinated.

Finch vendors the Software Factory workflow under `.agents/`. Use `$ticket-creation` for intake,
`$backlog-grooming` to prepare ready work, `$backlog-loop` for a bounded queue, `$implement` for one
accepted unit, `$coordinate` for sibling workstreams, `$review` to decide and perform integration,
and `$codebase-audit` for periodic whole-tree findings. Codex reads the canonical `.agents/skills`
tree directly, Claude Code discovers it through `.claude/skills` symlinks, and supported harnesses
resolve model lanes through `.agents/harnesses`; none carries a private workflow copy.

Before editing a shared backlog item, the workflow requires a versioned `work-claim:v1` tracker
record with the worker, branch, workspace identity, base commit, and bounded scope. That record is
the cross-harness ownership source of truth; assignees, labels, branches, pull requests, and one
harness's live-worker list do not substitute for it. Local worktree/process inventory is private to
the clone under Git's common directory, as described in `.agents/references/local-workspace-state.md`.

## Development setup

Finch's supported CI targets are Apple Silicon macOS and x86-64 Linux. Install stable Rust and the
Cap'n Proto compiler (`brew install capnp` on macOS or `apt install capnproto` on Debian/Ubuntu),
then build and test:

```bash
cargo build --bin finch
cargo test
python3 scripts/check_docs.py
```

`make build`, `make test`, and `make install` do the same through
`scripts/factory/with-cargo-slot`, which gives the build a
worktree-isolated `CARGO_TARGET_DIR` and the shared, cross-worktree sccache cache
instead of an unshared default -- useful once more than one worktree of this repo
exists on the same machine.

Run `cargo fmt --all -- --check` and relevant Clippy checks before submitting. Every bug fix needs a
deterministic regression test at the boundary where the failure occurred. Keep commits scoped and
do not rewrite shared branch history.

## Documentation claims

Document current behavior from source, tests, generated CLI help, configuration types, server route
definitions, and release artifacts. Label experimental or planned behavior and link its tracking
issue. Do not turn design goals, configured enum variants, or an old release note into claims of
working conformance.

Run `python3 scripts/check_docs.py` after changing the current documentation set. The checker covers
local links, selected stale claims, and shell-fence syntax. It is intentionally bounded; passing it
does not replace technical review.

## Human authorship and AI assistance

Set Git to an email address verified on the GitHub account that is responsible for the commit. A
GitHub-provided private `noreply` address is fine. Check the values before committing:

```bash
git config user.name
git config user.email
```

To set repository-local values:

```bash
git config --local user.name "Your Name"
git config --local user.email "YOUR_VERIFIED_EMAIL"
```

The commit author identifies the human who takes responsibility for the contribution. Never invent
a co-author name, email address, GitHub account, or person for an AI system. Do not use
`Co-authored-by:` for Anthropic Claude, OpenAI Codex, or another product unless a real human
co-author using that identity actually contributed and authorized the trailer.

When AI assistance was material, an optional plain-text trailer can record it truthfully without
asserting legal or GitHub authorship:

```text
Assisted-by: Anthropic Claude
Assisted-by: OpenAI Codex
```

Name only the system or systems actually used for that commit. Minor completion, formatting, or
spell-check assistance does not require a trailer. Review and test generated work before submitting
it; the human author remains responsible for correctness, security, licensing, and provenance.

Historical commits used metadata that GitHub may not link to the responsible human account, while
some assistance trailers may appear as linked contributors. Correcting those displays would require
rewriting published history. Finch will not rewrite history solely to alter attribution; the policy
above applies prospectively.

## Release process

```bash
# 1. Bump version in Cargo.toml
# 2. Commit
git add Cargo.toml && git commit -m "chore: bump version to vX.Y.Z"
# 3. Tag — triggers GitHub Actions release workflow
git tag vX.Y.Z && git push origin main && git push origin vX.Y.Z
```

GitHub Actions is configured to build `finch-macos-arm64.tar.gz` (macOS 14 runner) and
`finch-linux-x86_64.tar.gz` (Ubuntu 24.04 runner). Do not describe a release as ready merely because
artifacts exist; release and installer reliability are tracked in Issues #119 and #144.

**Platform notes:**
- Intel macOS: **not supported** (`ort` has no prebuilt binaries; GitHub deprecated Intel Mac runners Jun 2025)
- Linux: must be `ubuntu-24.04`+ (requires glibc 2.38+)
- macOS-only dependencies belong **after** the `[target.'cfg(target_os = "macos")'.dependencies]`
  header so they remain target-scoped

## Maintainer

Finch was created and is maintained by **Shammah Chancellor**. Anthropic Claude and OpenAI Codex
have provided substantial development assistance, but they are not legal authors, maintainers,
people, or GitHub identities.
