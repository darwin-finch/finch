# Efficient execution and evidence

Reduce context and tool-output cost without reducing engineering proof. Do not use hard token
budgets that can truncate work, omit evidence, or change priorities.

## Communicate only state changes

Report outcomes, decisions, blockers, failures, review findings, and completed integrations. Omit
routine narration and successful intermediate steps. Keep each update brief and independently
understandable. Do not repeat facts already present in the current issue, pull request, claim, or
contract; link the durable record instead.

A final handoff contains only the change, verification, remaining risk, ownership, and next action.

## Bound discovery and command output

Search narrowly before opening a file and read only relevant ranges of large files. Bound command
output and filter compiler or test output to result summaries and actionable failures. Never paste
complete issue histories, CI logs, warning streams, diffs, or source files into conversation.
Retain the exact failure, relevant state, and reproduction command needed to understand and repeat
the problem.

## Preserve context durably

When an issue or pull request exists, record accepted decisions, measurements, score changes,
solution contracts, findings, and terminal status there. Keep those records understandable without
chat history. Later sessions read the concise durable record instead of reconstructing exploration.
This does not add issue or pull-request ceremony to a direct request.

## Parallelize bounded work

Parallelize independent tasks when that reduces elapsed time. Use the compact packet in
[task packets](task-packet.md); do not delegate overlapping semantic or file scopes, duplicate an
investigation, or send the whole conversation. Workers return findings and proof, not exploration
logs. The coordinator continues useful independent work while workers run.

Use a replenishing pool, not fixed waves: keep at most `N` workers active, dispatch the next
dependency-eligible ticket as soon as any worker returns, and refresh the poset after an accepted
integration or any change to blocker state. A dependency wave is an ordering constraint, not a
barrier. Do not wait for the slowest worker when another ticket is eligible and the pool has room.
If a worker fails, keep its slot accounted for until its claim is released or handed off; do not
silently reuse its worktree or dispatch overlapping scope.

## Test once at the narrowest sufficient level

Run the smallest relevant local gate first and broaden only when risk or repository rules require
it. Do not rerun unchanged broad suites. Prefer `scripts/factory/gates`, which already emits bounded
summaries and actionable failure tails. Summarize successful tests by command and result count; on
failure retain actionable diagnostics and suppress unrelated warnings. Use monitoring when
available, or check CI after meaningful intervals rather than polling frequently.

Efficiency never weakens fail-before/pass-after regression proof, production-boundary coverage,
required gate stages, or risk-proportional review.
