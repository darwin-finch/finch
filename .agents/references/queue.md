# Picking the next item

The bottleneck is not writing code. It is review and decision latency: the human attention a change needs before it can merge. Order the queue to maximise merged value per unit of that attention. Cheap, certain, unblocking work goes first; one large uncertain item must never starve ten small certain ones.

A [binding](../bindings/github.md) may replace the ranking function. It may not skip readiness, and it may not invent scores the tracker does not store.

## Score, and record the score

Score every ready item 1–5 on four axes, written **on the item** so the next reader inherits it instead of re-deriving it:

- **value** — what breaks, or stays broken, if this never ships;
- **cost** — implementation plus review. Review is the scarce half, so authority, credentials, money, persistence, migration, concurrency, and process-lifecycle changes cost more than their diff suggests. Cost does not pick a [model lane](task-packet.md).
- **certainty** — how sure the approach is right and the acceptance criteria unambiguous. Low certainty means the real first task is a spike, scored as its own item;
- **unblocking** — how many other ready items this releases.

Order by:

```text
(value × certainty × (1 + unblocking)) / cost
```

Work the top of that list. A five-minute high-certainty fix and anything that releases a queue both outrank a large uncertain rewrite, which is the intended effect.

Bugs in the same area outrank features that touch it, unless the feature is the fix. Encode that on the **value** axis; do not hide it as tribal knowledge.

Do not re-score merely because a new session or worker picked up the queue. Reuse the recorded score while its facts remain current. Re-score only when new evidence materially changes scope, value, cost, certainty, or unblocking, including when actual cost crosses the guard below. Token usage may inform cost, but never overrides user value, correctness, readiness, or blocking relationships.

The 1–5 cost axis is ordinal queue judgment, not additive capacity or currency. Do not sum it into a maintenance allowance or token bucket. Evidence-backed architecture work enters the same graph and competes under the same ranking function; unused willingness to perform maintenance is not a reason to manufacture work.

## Two guards, because the score is gameable

- **Record the four numbers and one sentence of justification.** A score without justification is a wish. Put it on the item, not in a plan that dies with the session.
- **If actual cost exceeds the estimate by more than double, stop.** Write down why the estimate was wrong and re-score against the queue rather than finishing out of momentum. That correction is the only thing keeping the cost axis honest.

## Intake: what "ready" means

Nothing enters the ready queue without:

- acceptance criteria;
- an owner;
- for delegated work: repository, exact base revision, and authority.

An item missing those is not low priority, it is **not ready**. Making it ready is itself a cheap, high-unblocking task that usually scores well — so do that rather than leaving it to rot at the bottom of a list it was never really on.

When investigating an unready item, perform only the smallest action needed to resolve its missing decision or evidence, or to identify the named blocker and owner. Do not implement speculatively.

Blockers are tracker relationships, not sentences in the body. Prose dependencies are invisible to the poset.

## Eligible work

Eligible work is `READY` ∩ dependency-eligible ∩ no scope mutex. Taste, nits, and hypothetical frameworks are not `READY` work.

`NEEDS_SPECIFICATION` waits for a named human. It is not a prompt to invent the design, and it is not dispatchable.

Empty `READY` is stop, even if open `NEEDS_SPECIFICATION` items remain. The loop does not refill the queue from taste.

`$implement` does not file tickets for polish. `$review` files product defects and contract holes, not style. `$codebase-audit` is occasional, not part of every loop.

## Measure, or the stopping rule is a feeling

Record per change, in the terminal claim comment or the pull request:

- review rounds;
- confirmed findings, split product versus test-only;
- wall clock from packet to merge;
- rework after merge.

Five data points are enough to tell whether the tiers are set right. If risky changes keep merging with zero product findings, the tier is too heavy. If trivial changes keep coming back as rework, it is too light.

## Replacing the ranking function

A binding that has a real scarce resource (Pyramid's future *points* are the motivating case: internal currency so dependencies have a cost) substitutes that function here and says so in the binding page. Until the substitute exists and is stored on the item, do not invent numbers the tracker will not keep. Use the tracker's own frontier if it has one, and keep scoring's *readiness* rules.
