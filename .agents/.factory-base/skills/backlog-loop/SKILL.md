---
name: backlog-loop
description: Continuously triage the backlog and replenish a bounded pool of harness-native workers on eligible tickets using a dependency poset. Use when asked to work through the backlog, keep workers busy, or run the autonomous ticket loop. Use when the user runs /backlog-loop.
---

# Autonomous backlog coordinator

Use this skill when asked to work through the backlog or keep multiple ticket workers busy. Invoke `$backlog-grooming`, then delegate each selected packet to `$implement` through the current harness's native subagent mechanism.

Contract-authoring is `strong` lane, not a label: before invoking `$backlog-grooming` to author or complete an item's solution contract, check whether this session is already running at `strong` lane. If not, resolve `strong` through `.agents/harnesses/<active harness>.md` and dispatch contract-authoring as an actual delegated call — the same mechanism used below for implement/review packets, applied here too.

Load the tracker from [`.agents/binding`](../../references/load-binding.md) before any tracker verb. Pool mechanics: [parallel coordination](../../references/parallel-coordination.md).

Maintain a pool no larger than this machine can compile and test at once (cores, RAM, how heavy this repo's suite is). A user-stated count wins; otherwise use the consuming repo's `AGENTS.md` default if it has one. Dispatch [eligible](../../references/queue.md#eligible-work) tickets until that ceiling. As soon as any worker finishes, record its result, release or hand off its claim, refresh the item/poset state, and dispatch the next eligible ticket immediately. Dependency waves are ordering constraints, not barriers.

On GitHub, `.agents/scripts/ready_queue.py --workers N --format json` is the dispatchable set: READY items in dependency order, `NEEDS_SPECIFICATION` omitted. Do not intersect `ticket_poset.py` and `ticket_triage.py` by hand. On Pyramid, `pyr ready --json` is that report — do not invent scores. On Linear, `linear issue query --json` then `.agents/scripts/linear_ready.py --repo owner/name` ([linear.md](../../bindings/linear.md)). Isolate workers with `.agents/scripts/issue_worktree.sh`.

Every delegation packet includes the ticket, accepted outcome, tier, model lane, base revision, worktree, allowed and prohibited scope, proof/gates, and handoff format from [task-packet.md](../../references/task-packet.md). Before calling the native delegation mechanism for the active harness, load `.agents/harnesses/<active harness>.md` and resolve the packet's lane through it — a lane written into a packet and never resolved has no effect. Lane selection and substitution rules live in the packet; resolution mechanics live in the harness doc.

Machine-local constraints (serialized browser suites, one writer of a given store, language build mutexes) belong in the consuming repo's `AGENTS.md`. This skill will not guess them.

Stop when the user-requested budget is exhausted, [no eligible work remains](../../references/queue.md#eligible-work), or authority, safety, claim, dependency, or scope decisions require the user. Do not merge or close items merely because a worker reports success. `$implement` produces the candidate; `$review` is the only skill that lands; this skill owns selection, dispatch, replenishment, and coordinator reporting.

## Do not run this in a long chat

"Is the queue empty?" is `.agents/scripts/ready_queue.py`, `pyr ready`, or `linear_ready.py` as the binding says. No model. Do not answer it by rereading a design conversation.

Run `$backlog-loop` in a **new session** or a subagent started with no inherited history (`fork_turns="none"`). Workers already get a packet, not this chat. The coordinator should too: a long parent transcript is not the loop, and asking it for status is how you pay for 280k of prefix to print nothing.
