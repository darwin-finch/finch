# Contributing to Finch

Thank you for improving Finch. The project accepts focused fixes, tests, documentation, and design
discussion. Before starting a large change, open or join an issue so implementation and conformance
work can be coordinated.

Codex contributors can invoke the repository-owned `$finch-goal-seek` skill to audit the current
GitHub dependency frontier and carry an issue through an isolated worktree, regression, review, CI,
merge evidence, and cleanup. The skill lives in `.agents/skills/finch-goal-seek`; changes to the
workflow are ordinary reviewable repository changes rather than private assistant configuration.

## Development setup

Finch's supported CI targets are Apple Silicon macOS and x86-64 Linux. Install stable Rust and the
Cap'n Proto compiler (`brew install capnp` on macOS or `apt install capnproto` on Debian/Ubuntu),
then build and test:

```bash
cargo build --bin finch
cargo test
python3 scripts/check_docs.py
```

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

## Maintainer

Finch was created and is maintained by **Shammah Chancellor**. Anthropic Claude and OpenAI Codex
have provided substantial development assistance, but they are not legal authors, maintainers,
people, or GitHub identities.
