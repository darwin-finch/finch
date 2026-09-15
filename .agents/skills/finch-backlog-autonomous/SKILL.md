---
name: finch-backlog-autonomous
description: Continuously triage the Finch backlog and replenish a bounded pool of harness-native workers on eligible tickets.
---

# Finch autonomous backlog coordinator

Use this skill when asked to work through the Finch backlog or keep multiple ticket workers busy.
Invoke `$finch-backlog-triage`, then delegate each selected packet to `$finch-implement-ticket`
through the current harness's native subagent mechanism.

Maintain at most the requested worker count. Dispatch eligible, ready, non-overlapping tickets until
full. As soon as any worker finishes, record its result, release or hand off its claim, refresh the
issue/poset state, and dispatch the next eligible ticket immediately. Dependency waves are ordering
constraints, not barriers.

Every delegation packet includes the ticket, accepted outcome, tier, provider/model lane, base
revision, worktree, allowed and prohibited scope, proof/gates, and handoff format. Use the native
mechanism for the active harness: Finch uses `spawn_agent`; Codex, Claude Code, OpenCode, and Grok
Build use their own task/subagent APIs. Preserve the packet fields and report the provider/model
actually selected. If the harness cannot select the requested lane, report that instead of silently
substituting the primary model.

Stop when the user-requested budget is exhausted, no eligible work remains, or authority, safety,
claim, dependency, or scope decisions require the user. Do not merge or close issues merely because
a worker reports success. `$finch-implement-ticket` owns each individual ticket lifecycle; this
skill owns selection, dispatch, replenishment, and coordinator reporting.
