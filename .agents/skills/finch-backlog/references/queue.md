# Picking the next item

The bottleneck is not writing code. It is review and decision latency: the human attention a change
needs before it can merge. Order the queue to maximise merged value per unit of that attention.
Cheap, certain, unblocking work goes first; one large uncertain item must never starve ten small
certain ones.

## Score, and record the score

Score every ready item 1–5 on four axes, written **on the item** so the next reader inherits it
instead of re-deriving it:

- **value** — what breaks, or stays broken, if this never ships;
- **cost** — implementation plus review. Review is the scarce half, so authority, credentials,
  money, persistence, migration, concurrency, and process-lifecycle changes cost more than their
  diff suggests;
- **certainty** — how sure the approach is right and the acceptance criteria unambiguous. Low
  certainty means the real first task is a spike, scored as its own item;
- **unblocking** — how many other ready items this releases.

Order by:

```text
(value × certainty × (1 + unblocking)) / cost
```

Work the top of that list. A five-minute high-certainty fix and anything that releases a queue both
outrank a large uncertain rewrite, which is the intended effect.

## Two guards, because the score is gameable

- **Record the four numbers and one sentence of justification.** A score without justification is a
  wish. Put it in the issue, not in a plan that dies with the session.
- **If actual cost exceeds the estimate by more than double, stop.** Write down why the estimate was
  wrong and re-score against the queue rather than finishing out of momentum. That correction is the
  only thing keeping the cost axis honest.

## Intake: what "ready" means

Nothing enters the ready queue without:

- acceptance criteria;
- an owner;
- for delegated work: repository, exact base revision, and authority.

An item missing those is not low priority, it is **not ready**. Making it ready is itself a cheap,
high-unblocking task that usually scores well — so do that rather than leaving it to rot at the
bottom of a list it was never really on.

## Measure, or the stopping rule is a feeling

Record per change, in the terminal claim comment or the pull request:

- review rounds;
- confirmed findings, split product versus test-only;
- wall clock from packet to merge;
- rework after merge.

Five data points are enough to tell whether the tiers in `SKILL.md` are set right. If tier 3 changes
keep merging with zero product findings, the tier is too heavy. If tier 1 changes keep coming back
as rework, it is too light.
