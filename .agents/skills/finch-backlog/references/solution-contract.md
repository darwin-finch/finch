# Finch solution-contract protocol

Every production change starts from an immutable, explicitly approved solution contract.
The contract is the outcome and constraint boundary against which claims, implementation,
review findings, regressions, integration, and completion are evaluated. A branch or pull
request is not a contract.

## Contract identity

Publish the contract as an append-only GitHub issue comment. Record all of:

- contract ID, revision, issue-comment URL and numeric comment ID;
- SHA-256 digest of the exact comment body and its unedited GitHub metadata;
- full 40-character claimed implementation-base SHA;
- contract owner and plan-reviewer identity;
- immutable approval-comment URL and explicit `APPROVE` verdict.

A revision is a new immutable comment that names the contract it supersedes. Never edit a
contract or approval. Verify both comments and their digests before a readiness transition,
task handoff, production edit, scope revision, and merge. The immutable claim base remains
the historical base for regression and review lineage; record the current integration base separately
and never rewrite the claim or contract to make them match.

## Required content

Both compact and full contracts state:

- reproduced failure at the production boundary and the observable user outcome;
- explicit non-goals and accepted constraints;
- invariants and every acceptance gate, each with exactly one current owner and proof path;
- ownership, API boundary, and exact files allowed and excluded;
- deterministic fail-before/pass-after regression and why the claim base fails;
- current-main integration plus artifact, tree-equivalence, or user-visible proof;
- inherited work and how it is reused, narrowed, or superseded;
- finite hostile cases, deletion/reversion plan, and assumptions needing validation.

An obvious isolated fix may use a compact contract, but it still contains every item above
and receives one fresh independent plan reviewer. Risk determines additional perspectives:
nontrivial, broad, cross-subsystem, security, authority, persistence, provider, credential,
destructive, concurrency, compatibility, or lifecycle work gets the applicable expanded
panel from [the review protocol](review-protocol.md). Resolve every confirmed design blocker
and record explicit approval before production edits.

The reviewed contract is immutable. If evidence changes the outcome, constraints, files,
ownership, API boundary, gates, or proof, stop production edits. Recheck repository-wide
claim collisions, publish a new contract revision, obtain fresh approval, then append the
corresponding claim-scope revision before resuming.

## Discovery contract

Work in `NEEDS_SPECIFICATION` uses a compact discovery contract, not a production solution
contract. It records reproduction evidence, the exact missing decision, a concrete question,
the decision owner, the nearest minimally specified outcome, and a bounded read-only or
disposable-prototype plan. It grants no production mutation authority. See
[issue readiness](issue-readiness.md).
