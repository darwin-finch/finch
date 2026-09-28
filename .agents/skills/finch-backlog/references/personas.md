# UX/UI test personas

Stable personas for exercising the real `finch` binary end-to-end (not unit tests) and finding
what a real user would find odd or annoying. Each persona is a lens, not a script — the point is
a consistent, reusable point of view across testing passes over time, so findings are comparable
and personas don't need to be reinvented each session.

## How to run a persona pass

1. Build and install the current binary: `cargo install --path . --locked --force` from the repo
   root, then restart the daemon (`finch daemon-stop && finch daemon-start`) so it's running the
   code you just built, not a stale one.
2. Give the persona its own empty scratch directory to `cd` into before launching `finch`, and its
   own uniquely-named `tmux` session, so passes don't collide with each other.
3. Drive with `tmux send-keys` / `tmux capture-pane -p` (add `-e` to inspect raw SGR bytes when
   hunting rendering bugs, not just visible text).
4. Check `gh issue list --state open` for the current backlog before filing anything, so a finding
   that's already tracked gets a corroborating comment instead of a duplicate issue.
5. Bar for filing: reproducible, concrete, with evidence (exact repro steps, exact observed vs.
   expected, a captured pane or exact error text) — not a vague impression. This repo's own
   testing culture is evidence-over-vibes; match it.
6. Known standing confound as of 2026-09-28: issue #1381 — a freshly-created Brain's canonical
   workspace gets recorded as wherever the *daemon* process happens to be running from, not the
   client's cwd, so every persona pass currently hits a "workspace mismatch" banner and (likely
   related) cross-Brain memory recall on the very first message of a "fresh" session. This is
   already filed; don't refile it, but do note if a finding is actually just a symptom of it
   before treating it as new.

## Personas

### Dana — Claude Code power user
Professional engineer who uses Claude Code all day at work, trying Finch for the first time
tonight. Strong, specific muscle-memory from Claude Code and similar agent CLIs: Ctrl+C/Esc
behavior, slash commands, plan mode, tool-approval prompts, diff display, status/model line. Does
normal first-session things — write code, run a shell command, edit a file, ask Finch to explain
something, hit Ctrl+C/Esc a few times, try tab-completion, resize the terminal once or twice.
Annoyed by any *unexplained* divergence from Claude-Code convention, and equally annoyed if Finch
silently does something Claude Code doesn't, without explaining why. When filing, name what Claude
Code (or another mainstream agent CLI) does differently, so a reviewer can tell "objectively
broken" from "deliberate, undocumented divergence."

### Marcus — coming from Codex CLI
Used to Codex's approval modes, sandboxing prompts, and its own copy/paste split (Ctrl+C = copy
since Codex can't override that key; Ctrl+V reserved exclusively for image paste; Cmd+V for text —
an accepted-but-annoying split). Tests whether Finch's conventions feel familiar or jarring,
specifically copy/paste behavior and approval/permission prompts. (Not yet run as of 2026-09-28.)

### Priya — first-time agentic-CLI user, non-engineer
Product manager or technical writer, has never used a coding-agent CLI before, no prior tool
expectations to carry over. Tests onboarding cold: does `finch` just work on first launch, is the
setup wizard clear without assuming prior context, are error messages jargon-free and actionable.
Stresses the accessible-interfaces principle generally, not just the GUI-automation invariants
Sam covers. (Not yet run as of 2026-09-28.)

### Ollie — offline/local-model enthusiast
Wants to run Finch fully local for privacy/cost reasons. Tests local model routing, the setup
wizard's model selection, and local generation quality/latency. CLAUDE.md already flags local
routing as experimental (#74, #98), so this persona is expected to surface real rough edges — good
stress test precisely because it's not solid ground yet. (Not yet run as of 2026-09-28.)

### Chen — power user, marathon sessions
Runs multi-hour sessions, uses named Brains, subagents (`spawn_task`), memory/recall, background
bash tasks. Tests session persistence, Brain switching, context compaction, memory-recall UX, and
whether long sessions degrade. (Not yet run as of 2026-09-28.)

### Sam — blind developer, accessibility
Relies on GUI automation entirely through text; visual rendering is irrelevant, only what comes
back as speakable text matters. Directly tests CLAUDE.md's GUI-accessibility invariants (semantic
identifiers only, plain-text-must-stand-alone reads, app-specific words like `excel-read`,
actionable "not found" errors naming what's missing, `gui_click`-with-coordinates excluded from
non-developer tool lists, accessibility-permission errors naming the exact System Settings path).
Companion to open issue #1182 ("accessibility invariants enforced by tests but never validated
against reality") — file specific new issues for gaps found, or comment on #1182 directly when
that's the more useful home for a given finding.

### Rin — UI designer
Cares only about visual/rendering correctness: alignment, spacing, color/theme consistency,
truncation/wrapping, spinner/status feedback, dialog and wizard layout, and specifically what
happens at unusual terminal sizes and live resizes. Hunts the documented row-diff-blit stale-row
bug class (CLAUDE.md's TUI invariants section, precedent issues #1297/#1305/#1140/#1141/#1318):
a component whose row count or content shrinks/changes between two frames, where paint code
redraws only rows it thinks changed instead of fully clearing the region first. Test at multiple
widths (narrow ~80col, wide ~220col) and with live mid-flow resizes, especially across the wizard
tabs and any place content height changes between two adjacent states.

### Jordan — UX designer
Cares about interaction flow, mental models, discoverability, error recovery — not pixels (that's
Rin). Does a cold, first-principles walkthrough: fresh install, first launch, the setup wizard as
an *experience* (do the questions make sense and get explained, regardless of whether they render
correctly), a first real coding conversation, deliberately triggered error paths (bad input,
mid-operation cancel, asking for something Finch can't do) to check whether recovery is clear, and
a skim of `README.md` against what actually happens. Doesn't file model-accuracy complaints
(already tracked: #74, #98, #120, #147) — the value-add is specifically flow and self-consistency
of terminology (is "Brain" used consistently, do error messages explain what to do next, etc.).

## Run log

- **2026-09-28**: first wave (Dana, Sam, Rin, Jordan) run against finch 0.7.31 / commit `0175e4ae`
  + PR #1380. Filed: #1381 (fresh Brain workspace-mismatch + cross-Brain memory recall — likely
  the single highest-impact finding, affects every fresh session), #1382 (GUI-automation
  accessibility invariant violations), #1383 (raw internal errors leaking into transcript on a
  local-model repair round), #1384 (startup panic in `finch-routing-tree` on a fresh/near-empty
  workspace).
- **2026-09-28**: second wave (Marcus, Priya, Ollie, Chen) run against the same build plus #1380.
  Filed: #1387 (exit message contradicts `finch brain ls`, reads as data loss to a first-timer),
  #1388 (two distinct provider responses concatenated with no separator on Claude CLI
  Subscription — the concrete reproduction of an anomaly flagged-but-unconfirmed earlier that
  night; root-cause investigation and fix dispatched separately), #1389 (Claude CLI Subscription's
  inner `claude` subprocess runs unrestricted with its own auto-memory skill, writing real files
  outside Finch's data model, and can hijack terminal input via an approval dialog — serious,
  needs a design decision on subprocess scoping, not just a quick patch). Strong corroborating
  evidence added to #1381 (deliberate `finch attach <new-name>` still mismatches, ruling out
  "accidental old-brain reuse"; a fresh session actually searched/read the wrong directory because
  of it — real correctness risk, not just a confusing banner), #1228 (two independent testers hit
  the identical 500 error; local-model tool-use follow-up now hard-fails rather than degrading),
  #1383 (two more independent triggers, including one on the (non-local) Claude CLI Subscription
  provider). Copy/paste and approval-prompt flow (Marcus) and TUI rendering robustness (Rin, wave
  one) both held up well under real stress-testing — worth noting as much as the failures.
  All 8 planned personas have now run at least once.
