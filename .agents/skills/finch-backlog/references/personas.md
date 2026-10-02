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

#### Scenarios (Daily Dev & Self-Hosting Code Navigation)
- **Scenario D1 (Tree-sitter Code Inspection)**: Launch Finch in `~/repos/finch`. Run `code_outline` on `crates/finch-tui/src/scroll_view.rs` and `src/tools/implementations/code_outline.rs`. Verify that structural nodes (functions, structs, traits, impls) are returned deterministically and rendered cleanly without clipping.
- **Scenario D2 (Symbol Definition & References with `code_hop`)**: Ask Finch to find where `ScrollView` or `ToolViewport` is defined and all places it is referenced across crates. Verify Tree-sitter symbol hops resolve accurately to exact line spans.
- **Scenario D3 (Muscle-Memory Interrupts & Stream Cancellation)**: Start a long explanation or code generation and press `Ctrl+C` or `Esc`. Verify streaming immediately aborts, cursor restores to input prompt, and no zombie tasks remain.
- **Scenario D4 (Diff Inspection & Inline Edits)**: Ask Finch to make a targeted docstring or comment improvement in a Finch source file. Verify diff highlights additions in green and deletions in red, and that tool expansion (`F6`) starts at line 0 without clearing the screen.

### Marcus — coming from Codex CLI
Used to Codex's approval modes, sandboxing prompts, and its own copy/paste split (Ctrl+C = copy
since Codex can't override that key; Ctrl+V reserved exclusively for image paste; Cmd+V for text —
an accepted-but-annoying split). Tests whether Finch's conventions feel familiar or jarring,
specifically copy/paste behavior and approval/permission prompts. (Not yet run as of 2026-09-28.)

#### Scenarios (Approvals, Pasting & Sandboxing)
- **Scenario M1 (Multiline Paste & Indentation)**: Paste a 25-line nested Rust match expression into the composer. Verify that indentation, brackets, and newlines are preserved verbatim without triggering premature execution or cursor desync.
- **Scenario M2 (Tool Approval Prompts)**: Trigger a file modification in Finch. When the approval dialog appears, test `y` (once), `a` (always/session), `n` (reject), and `Esc` (dismiss). Verify keyboard focus lands squarely on the dialog and the final decision renders cleanly inline on the tool row without orphan UUID lines.
- **Scenario M3 (ANSI Selection & Clipboard)**: Highlight colored compiler output or diff lines in the TUI. Copy to clipboard and verify pasted text is sanitized of raw ANSI escape codes (`\x1b[...]`).

### Priya — first-time agentic-CLI user, non-engineer
Product manager or technical writer, has never used a coding-agent CLI before, no prior tool
expectations to carry over. Tests onboarding cold: does `finch` just work on first launch, is the
setup wizard clear without assuming prior context, are error messages jargon-free and actionable.
Stresses the accessible-interfaces principle generally, not just the GUI-automation invariants
Sam covers. (Not yet run as of 2026-09-28.)

#### Scenarios (Cold Onboarding & Setup Wizard)
- **Scenario P1 (First Launch in Empty Workspace)**: Launch `finch` in a fresh, empty directory without `~/.finch/config.toml`. Verify the welcome flow and setup wizard guide the user through provider selection, daemon connection, and workspace initialization without technical jargon.
- **Scenario P2 (Wizard Keyboard Navigation & Abort)**: Navigate wizard steps using `Tab`, `Shift+Tab`, `Arrow` keys, and `Enter`. At step 2, abort via `Ctrl+C` or `Esc` and verify the terminal returns to cooked mode without broken cursor visibility or echo.
- **Scenario P3 (First Natural Question)**: Ask a beginner question like `"What does this project do?"` in a new workspace and verify the initial greeting and explanation are reassuring, clear, and actionable.

### Ollie — offline/local-model enthusiast
Wants to run Finch fully local for privacy/cost reasons. Tests local model routing, the setup
wizard's model selection, and local generation quality/latency. CLAUDE.md already flags local
routing as experimental (#74, #98), so this persona is expected to surface real rough edges — good
stress test precisely because it's not solid ground yet. (Not yet run as of 2026-09-28.)

#### Scenarios (Local Models & Hybrid Routing)
- **Scenario O1 (Local Model Selection)**: Switch to a local model (Ollama / local Gemma) via the wizard or `/model`. Verify the switch registers without crashing.
- **Scenario O2 (Graceful Degradation on Tool Calls)**: Ask the local model to perform a file edit. When the continuation round is reached, verify it displays the clean, documented notice (`#1228`) instead of crashing, leaking raw JSON, or emitting unhandled 500 errors.
- **Scenario O3 (Seamless Cloud Switch)**: Switch back to a cloud provider mid-session and verify conversation history is preserved and subsequent tool calls succeed.

### Chen — power user, marathon sessions
Runs multi-hour sessions, uses named Brains, subagents (`spawn_task`), memory/recall, background
bash tasks. Tests session persistence, Brain switching, context compaction, memory-recall UX, and
whether long sessions degrade. (Not yet run as of 2026-09-28.)

#### Scenarios (Marathon Sessions & Finch-on-Finch Self-Hosting)
- **Scenario C1 (Named Brain Lifecycle)**: Create and attach to a named Brain: `finch attach finch-self-host`. Run multiple turns across hours, detach, and reattach. Verify history and workspace context persist accurately.
- **Scenario C2 (Background Build & Check)**: Ask Finch to run `./scripts/test_brains.sh cargo check --all-targets` via `background_bash`. Continue conversing while the check runs; verify the background task completes, logs to its descriptor, and reports its exit status cleanly without interrupting the composer.
- **Scenario C3 (Context Compaction & Recall)**: After a heavy session with multiple large file reads and tool runs, trigger `/compact`. Verify context is summarized without losing key architecture decisions or working memory.
- **Scenario C4 (End-to-End Self-Hosting Edit & Test)**: Ask Finch to navigate to a specific helper function in `src/tools/implementations/`, propose a clean enhancement, apply the edit via `edit`, and run the test suite to verify the change. Finch working on and improving Finch!

### Sam — blind developer, accessibility
Relies on GUI automation entirely through text; visual rendering is irrelevant, only what comes
back as speakable text matters. Directly tests CLAUDE.md's GUI-accessibility invariants (semantic
identifiers only, plain-text-must-stand-alone reads, app-specific words like `excel-read`,
actionable "not found" errors naming what's missing, `gui_click`-with-coordinates excluded from
non-developer tool lists, accessibility-permission errors naming the exact System Settings path).
Companion to open issue #1182 ("accessibility invariants enforced by tests but never validated
against reality") — file specific new issues for gaps found, or comment on #1182 directly when
that's the more useful home for a given finding.

#### Scenarios (Accessibility & Text-Only Interface)
- **Scenario S1 (Screen Reader Readability)**: Run Finch with raw/piped text output or pipe the TUI through a terminal screen reader. Verify tool invocations, status messages, and model responses read as continuous, speakable English without relying on visual ASCII art.
- **Scenario S2 (Actionable Error Messaging)**: Deliberately trigger file access errors or missing binaries. Verify the resulting error explicitly names the target path, command, and exact remedy.
- **Scenario S3 (Semantic Tool Identification)**: When querying or using tools, verify that all actions are identified by semantic names rather than cryptic codes or spatial screen coordinates.

### Rin — UI designer
Cares only about visual/rendering correctness: alignment, spacing, color/theme consistency,
truncation/wrapping, spinner/status feedback, dialog and wizard layout, and specifically what
happens at unusual terminal sizes and live resizes. Hunts the documented row-diff-blit stale-row
bug class (CLAUDE.md's TUI invariants section, precedent issues #1297/#1305/#1140/#1141/#1318):
a component whose row count or content shrinks/changes between two frames, where paint code
redraws only rows it thinks changed instead of fully clearing the region first. Test at multiple
widths (narrow ~80col, wide ~220col) and with live mid-flow resizes, especially across the wizard
tabs and any place content height changes between two adjacent states.

#### Scenarios (UI Layout, Sizing & Row-Diff-Blit Robustness)
- **Scenario R1 (Terminal Size Extremes)**: Test Finch at `80x24` (compact standard), `120x35` (default dev), and `220x50` (ultra-wide). Verify prompt line, status bars, and borders fit without wrap-tearing or clipping.
- **Scenario R2 (Mid-Stream Window Resizes)**: During active streaming or while a tool output viewport is open, trigger live window resizes (`tmux resize-window`). Verify the canvas repaints without duplicate text, ghost lines, or corrupted borders.
- **Scenario R3 (Tool Expansion / Collapse Stale-Row Checks)**: Expand a tool output (`F6`), scroll through lines, and collapse it. Verify that every row of the expanded view is fully wiped and that paint code does not leave stale artifact rows.
- **Scenario R4 (Scrollback Hint & Physical Line Navigation)**: Scroll up into long multi-turn history. Verify the scroll position hint (`↑ X more above · ↓ Y more below`) renders cleanly, survives resizing, and physical line scrolling does not skip long wrapped lines.

### Jordan — UX designer
Cares about interaction flow, mental models, discoverability, error recovery — not pixels (that's
Rin). Does a cold, first-principles walkthrough: fresh install, first launch, the setup wizard as
an *experience* (do the questions make sense and get explained, regardless of whether they render
correctly), a first real coding conversation, deliberately triggered error paths (bad input,
mid-operation cancel, asking for something Finch can't do) to check whether recovery is clear, and
a skim of `README.md` against what actually happens. Doesn't file model-accuracy complaints
(already tracked: #74, #98, #120, #147) — the value-add is specifically flow and self-consistency
of terminology (is "Brain" used consistently, do error messages explain what to do next, etc.).

#### Scenarios (UX Mental Models & Error Recovery)
- **Scenario J1 (Command Discoverability & Autocomplete)**: Type `/` and navigate the command popup. Verify parameter hints (`/model <name>`, `/attach <name>`, `/compact`) guide the user on syntax.
- **Scenario J2 (Recovery from Invalid Commands & Inputs)**: Enter malformed commands (`/model`, `/bogus`). Verify Finch provides clear correction suggestions rather than silent failure or raw stack traces.
- **Scenario J3 (Terminology & Boundary Clarity)**: Verify that the TUI consistently uses clear terminology for "Workspace" (client cwd), "Daemon" (background service), and "Brain" (conversation/run identity) across all status indicators, headers, and exit messages.

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
  night; fixed same night, PR #1390 — `TurnRecord::tool_use_bridges_next_text`), #1389 (Claude CLI Subscription's
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
- **2026-09-29**: full re-test, all 8 personas, against a build with every issue above fixed and
  merged (#1181, #1380, #1382 partial/#1395, #1383/#1396, #1384/#1398, #1387/#1397, #1388/#1390,
  #1389/#1392). Wave 1 (Dana, Sam, Rin, Jordan) immediately found **#1381 was not actually fixed
  live** despite being merged and closed — three independent testers reproduced the exact original
  symptom on the fixed build. Root cause: the merged fix (#1393) only closed one of two independent
  Brain-creation paths; `EventLoop::hydrate_brain_selection` (which runs *before*
  `register_home_brain` in real startup) can create a Brain first via a wholly separate HTTP route
  the original fix never touched, and Brain-metadata creation is first-write-wins. This also
  explained why #1387 regressed at the same time (the workspace-verification step then fails,
  short-circuiting the code path that sets `home_brain_registered`). Fixed for real in #1400
  (`set_provider_selection_for_client` + `x-finch-workspace` header), verified live directly (not
  just via test suite) before closing #1381/#1387 a second time. Wave 2 (Marcus, Priya, Ollie,
  Chen), run after the #1400 fix, unanimously confirmed #1381/#1383/#1387/#1388/#1389 all hold up
  live now, with fresh corroborating evidence for each. Zero new bugs found in this final
  full-coverage pass — every remaining observation (wizard Ctrl+C footer wording, `background_bash`
  not exposed to the Claude CLI Subscription provider) was checked and confirmed either
  intentional/documented or too low-confidence to file. **Lesson for future passes, now in
  `crates/finch-brain/AGENTS.md`**: a fix's own test suite passing is not sufficient evidence a
  live regression is actually closed when the bug crossed a real end-to-end sequence (startup
  order, in this case) that the tests exercised only in isolated pieces — re-verify live before
  trusting a "fixed" issue closed again, especially for anything touching multi-step
  client/daemon sequencing.
- **2026-09-29 (later that night)**: focused two-persona pass (Rin, Chen) specifically to
  live-validate the newest fixes that landed after the full 8-persona pass above: the transcript
  scroll-position indicator (#1252/#1412), the named-Brain VM-grant security narrowing (#1410),
  the text-selection-reselect fix, the local-model-crash-after-tool-call fix, and a re-check that
  the fresh-Brain workspace-mismatch fix (#1381/#1400) still holds. Caught mid-run that the local
  `main` checkout was one commit behind `origin/main` — the scroll-indicator PR (#1412) had merged
  minutes into the session — so the first build under-tested it; pulled and rebuilt
  (`e320bb5d`) before drawing any conclusion, which is itself worth flagging: a build kicked off
  at session start can go stale if a fix lands mid-session, so re-check `git fetch`/`origin/main`
  before trusting a "not present" finding on a long-running pass.
  **Held up, confirmed live:**
  - Fresh-Brain workspace correctness (#1381/#1400): both personas' brains showed the correct
    cwd-derived workspace from the very first message, no mismatch banner, across two fresh named
    Brains (`misty-ford-07f72e`, `quiet-moor-45672f`) and two more spun up mid-pass.
  - Local-model crash after a tool call: reproduced the exact scenario (local-gemma-2-9b calls a
    tool, then the continuation round) four separate times across both personas' sessions; every
    time it returned the clean `"Local models don't yet support continuing a conversation after a
    tool call (#1228); try a cloud provider for tool-using turns."` 500 response instead of
    crashing or leaking a raw error. Matches the documented fix framing exactly (turns a hard
    crash into a named, already-tracked gap).
  - Named-Brain tool-call round followed by a text-only round (the `tool_work_unit` clear fix):
    ran a tool-using round then an explicit "no tools, just say hi" round on the same Brain
    (`quiet-moor-45672f`) — completed cleanly, no stuck "running" tool row, no leftover
    work-unit indicator.
  - Background bash task: launched via plain conversational request, completed and wrote its
    expected output file (`/tmp/finch-persona-chen/bg_task.log` contained `background done`) —
    functioned correctly end-to-end.
  - Scroll-position indicator (#1252/#1412) itself: confirmed working correctly across four
    independent, deliberately-varied fresh sessions and multiple widths — 100 cols (`"↑ 17 more
    above · ↓ 32 more below"`), 60 cols (correctly shrinks/ellipsizes: `"↑ 26 more above ·…"`),
    and 40 cols (correctly drops entirely, leaving identity the full width, exactly as documented
    — identity keeps priority over the hint on a narrow terminal). Also confirmed it survives a
    live resize and a mid-flight provider switch (the same `/model`/`/providers`/`/provider`
    sequence that produced a "could not be persisted" warning) without misbehaving.
  - VM-grant security narrowing (#1410): not isolated with a dedicated adversarial test, but every
    tool call across both personas' many named-Brain turns (which all go through the fixed
    `dispatch_named_brain_run` path) correctly re-prompted for approval rather than silently
    reusing an earlier grant — consistent with, not a substitute for, the fix's own regression
    test.
  **One anomaly, investigated but NOT filed (no clean repro):** the scroll-position hint never
  appeared in exactly one session (`misty-ford-07f72e`, Rin's very first session of the pass),
  reproducibly within that session across repeated PageUp presses and even after a live resize,
  despite the transcript visibly scrolling (different numbered items came into view) and `offset()`
  clearly nonzero. Tried to isolate the trigger — fresh session, fresh session with the same
  #1228 failure + provider switch, fresh session with a resize, fresh session with the exact
  mid-flight `/model`/`/providers`/`/provider`-during-an-active-query sequence that produced the
  "could not be persisted" warning in the original — and the hint appeared correctly in every one
  of those four deliberate reproduction attempts. Given the bar for filing is a clean, reproducible
  repro and this one only reproduces inside one specific, heavily-interacted-with session whose
  exact triggering state couldn't be isolated, this is recorded here as a maybe-real, low-confidence
  lead rather than filed as an issue — worth another look if a future pass hits the same
  no-hint-while-clearly-scrolled symptom, since a second independent occurrence would raise
  confidence a lot.
  **Also observed, not filed (pre-existing environmental condition, not a new regression):**
  every Brain touched this pass showed `"recalled 0 · index did not finish loading"` for its whole
  session, and the daemon log showed a permanent per-Brain `MemTree hydration failed` state
  (`"the MemTree loader ended without finishing"`) recurring every ~10s indefinitely once
  triggered. Traced this to the already-shipped #1384/#1398 fail-closed fix (refuses writes on an
  embedding-dimension mismatch instead of panicking) — this dev daemon's on-disk memory store has
  clearly been rebuilt across multiple different embedding engines over many days of testing, so
  the fail-closed path is firing as designed, not a new bug. Confirmed it is environment-wide, not
  specific to a fresh Brain: a brand-new Brain with nothing in its own history hit it too. The
  10-second-forever retry with no backoff once a run's memory projection is permanently `Failed`
  for the process's lifetime is mildly wasteful log spam, but out of scope for this pass's five
  target fixes — flagging here rather than filing a new ticket for it.
  **spawn_task on local-gemma-2-9b**: the 9B local model could not reliably emit a well-formed
  Finch wire tool-call for `spawn_task` (emitted a raw XML `<tool_use>` block instead of the
  expected Forth program, got the wire-repair prompt, then gave up with an empty code fence) — a
  model-capability limitation consistent with CLAUDE.md's own "local routing and provider parity
  remain experimental" framing (#74/#98), not something to file. `spawn_task` and
  `background_bash` are also confirmed absent from the Claude CLI Subscription provider's own
  restricted MCP bridge tool list (`src/cli/claude_cli_bridge.rs`'s `register_tool_schemas` only
  registers Bash/Edit/Glob/Grep/Read/Write) — consistent with the prior pass's note that this is
  intentional subprocess scoping, not a gap.
  **Net result: no new issues filed.** All five targeted fixes held up under live re-testing.
- **2026-09-29 (still later that night)**: focused two-persona pass (Jordan, Chen) against a build
  with four more fixes landed: the `present_plan`-in-a-batch deadlock (#26/#363), GUI/Excel
  automation tools registering unconditionally on macOS (#421), approval decisions rendering
  inline on the tool-call row (#439), and stale-runner-lease reattach showing a calm transition
  (#423). Built via `cargo install --path . --locked --force` (commit `c99ca1a9`), daemon
  restarted. Session-naming note: the plain `persona-jordan`/`persona-chen` tmux session names
  collided mid-run with another concurrent agent independently running the same task in this
  session (its content showed up under `persona-jordan-<timestamp>` after a rename) — switched to
  PID+timestamp-suffixed session names (`jrd-<ts>-<pid>`/`chn-<ts>-<pid>`) to isolate cleanly;
  future passes should default to unique suffixes from the start rather than the bare persona name
  when multiple agents might be active in the same session.
  **Plan-approval deadlock (#26/#363) — held up, tried hard to break it:** drove the full flow live
  on a fresh Brain (`hollow-shore-dd3a65`): entered `/plan`, had the model read the directory,
  present a plan via `present_plan`, approved it through the real dialog ("Approve and execute").
  Confirmed the specific batch shape the fix targets — a round with `present_plan` *and* a `write`
  tool call together (`Tools (2 calls): present_plan(...), write(one.txt)`) — completed normally,
  not just a trivial single-call case. After approval the session was immediately responsive (no
  hang), `one.txt` was created with correct content, and five more turns/approvals (further writes,
  a `gui_inspect` call) all completed normally in the same session afterward. No hang, no stuck
  spinner, no dead composer at any point. This is the single most important result of the pass:
  the deadlock fix holds live, under genuine multi-tool-call batch pressure, not just its own unit
  test.
  **Stale-runner-lease reattach (#423) — held up:** started Chen's session (`dark-moor-39d8b9`),
  `kill -9`'d the client process to leave the lease dangling (a graceful exit releases it, so this
  is the real ungraceful-death trigger), then immediately ran `finch attach dark-moor-39d8b9`.
  Transcript showed exactly one calm line — `dark-moor-39d8b9: Runner role is transferring from a
  previous session — reconnecting…` — matching `RunnerRecovery::LeaseTransferring`'s
  `human_message()` verbatim, with no raw exception text and no "Failed:" line, then reconnected
  automatically within moments and resumed the prior conversation history correctly.
  **Approval decision inline on the tool-call row (#439) — did NOT hold up for the common case,
  filed as #1426:** in a subagent/remote-Brain-viewing context #439's fix works as designed, but
  the single most common scenario — a plain home-session `write` needing approval, the exact
  scenario #439's own bug report described — still rendered the decision as a separate,
  opaque-id-labelled row (`approval toolu_9XLCrgChNywWovb9OX6gqhBT — approve_once by
  shammah@...`), not folded onto the `write(...)` row. Root-caused: `project_remote_brain_run_event`
  (`src/cli/repl_event/event_loop.rs`) skips creating a `tool_rows` entry for a tool call already in
  `locally_rendered_tool_ids` (populated by the separate `LocalBrainProjection` mechanism that
  renders a home session's own turns directly), so #439's `tool_rows.contains_key(approval_id)`
  check never matches for a locally-rendered call — exactly the common path. #439's own regression
  test doesn't exercise `LocalBrainProjection` at all, which is why it passed without catching this.
  Filed as #1426 with the full trace.
  **GUI automation tools (#421) — registration confirmed correct by source, not confirmed via a
  live model response:** `src/cli/repl.rs` registers all nine GUI/Excel tools unconditionally
  inside one `#[cfg(target_os = "macos")]` block, matching the fix. Tried to get a live
  `gui_inspect(query: "availability")` response through the TUI, but both usable local providers
  hit the pre-existing #1228 "can't continue after a tool call" wall on the very next round (so the
  model's own text summary of the result never renders), and `finch query` (single-shot mode) uses
  a different, more minimal tool registry that doesn't include `gui_inspect` at all (not a bug —
  wrong test surface, not the interactive REPL path #421 touched). Did not attempt to expand the
  collapsed tool-call row in the TUI to read the raw result directly (out of budget for this pass).
  Net: registration and the tool's own unit tests (`test_gui_inspect_availability_reports_disabled_
  state_even_when_flag_is_off`) are solid evidence #421 is correctly wired, but this pass did not
  get a live end-to-end confirmation of the actual returned text the way the other four fixes got.
  **Bonus finding, unrelated to this pass's four target fixes, filed as a reopen of #1384:** every
  fresh `finch` launch tonight (5/5, two brand-new directories/Brains dedicated to this check plus
  three more used for the main tests) panicked a background `tokio-rt-worker` thread with the exact
  signature `#1384` was originally filed against (`routing_tree.rs:181`, "index out of bounds: the
  len is 0 but the index is 0" — a dimension-0 anchor/direction). #1384 was closed by #1398, but
  #1398's fix and its own regression test address a different mechanism (reopening an existing
  store built under one *nonzero* embedding dimension with a different *nonzero* dimension) than
  what #1384 actually reported (a degenerate, completely empty anchor/direction on a decision
  node) — `load_routing_tree`'s new dimension check wouldn't fire for the latter. Reopened #1384
  with the live evidence rather than filing a duplicate, since the signature and repro match
  exactly. Caveat noted in the reopened issue: this dev machine's single shared `~/.finch/memory.db`
  has been rebuilt across many embedding engines over many days of testing, so a clean-machine
  fresh install might not reproduce it — but the fact that it still reproduces on the very build
  that was supposed to fix it is the point.
  **Chen's other checks (named Brains, background tasks, a memory ack) all held up**: two
  sequential background bash tasks both completed and wrote their expected files
  (`bg_task.log`/`bg2.log`); one query round did fail with `"Tool continuation could not be
  admitted: LLM continuation is unavailable"` immediately after the *first* background-task
  round (a 2-second admission-handshake timeout in `commit_tool_round_and_continue`,
  `src/cli/repl_event/event_loop.rs`) — but the background task's own side effect (the file write)
  completed correctly regardless, and an identical second background-task request right after
  succeeded cleanly with no error. Only reproduced once, immediately following the forced
  `kill -9`-and-reattach test on the same session, so plausibly leftover session state from that
  rather than a background-task-specific bug; below the bar for filing (not independently
  reproducible), noted here in case a future pass hits the same message after a reattach.
