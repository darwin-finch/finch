# Why the loop exists

Read this only when changing the workflow itself. These safeguards answer failures that have already occurred; they are evidence for the policy, not steps to load during ordinary issue work.

| Observed failure | Preventive step |
| --- | --- |
| Two workers began the same issue after both saw it as free. | Publish and verify one immutable claim before editing shared issue work. |
| A regression passed because it ran only on the fixed tip. | Run it against an isolated base and the candidate, recording both results. |
| Two "fix" commits each introduced a new error; reviewers trusted the PR story. | A correction is not a lower bar. Verify against original evidence and the actual diff, not the commit message. Derived-data repos add a recompute-from-inputs gate locally. |
| A replacement left an adapter and duplicate implementation indefinitely. | Name create/switch/delete stages, an owner and a removal trigger before review. |
| Repeated process reviews added ceremony without changing the patch. | Stop after the first clean risk-proportional review round. |
| A wording fix consumed a feature's process. | Name the tier and run only that tier. |
| A test-harness finding opened another product review round. | Record test-only gaps as follow-up work; they do not open another round. |
| Interesting work displaced a small fix blocking three pull requests. | Score ready work and take the highest return on review attention. |
| A model took the shortest path and painted over a known next feature. | Cost-of-reversal questions in the contract, a shape perspective in review, and an occasional whole-tree audit. |
| Boy scout cleanup rode along in the feature PR and review stalled. | Optional campground is a later ticket. A *required* refactor or migration is a stack: tests → refactor/migration → feature. Do not mix steps in one landing. |
| One all-in-one `backlog` skill was loaded for the wrong job. | Separate intake, grooming, dispatch, implement, coordinate, review, and audit. |
| Loop refilled from taste after READY was empty. | Eligible work is READY ∩ dependency-eligible ∩ no mutex; empty READY is stop even if NEEDS_SPECIFICATION remains. |
| A long design chat spent hundreds of thousands of tokens to report an empty queue. | `ready_queue.py` / `pyr ready` is status. Run the loop in a fresh session, not the conversation that designed it. |
| Ordinary work increasingly opened dependency internals, but raw token and file counts could not say whether the boundary had degraded. | Record versioned boundary-load evidence with explicit observation coverage; compare only claims with complete inspected-path telemetry, then send an advisory candidate through audit and the normal ticket graph. |
| Periodic architecture audits either repeated old findings or treated filed tickets as completed maintenance. | Use an immutable completed-pass tag to suppress stale candidates; advance it only after a clean audit or integrated corrective pass, never merely because tickets were filed. |
| A packet named a vendor model; the next session was a different harness, or review was treated as “use a bigger model.” | Packets name a lane. Cost does not pick it. Review is the protocol. The kernel documents how each harness resolves a lane to a model (`.agents/harnesses/`); it never names or chooses the vendor model itself. |
