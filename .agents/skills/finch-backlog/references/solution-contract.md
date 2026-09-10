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

The contract and its review are separate append-only events. GitHub observation metadata
(`author`, `createdAt`, `updatedAt`/`lastEditedAt`, numeric comment ID, raw body digest, and
block order) is retained outside the block and verified before admission. The digest is of
the complete raw contract comment, so it is recorded by the approval rather than inside the
body it hashes.

```text
<!-- finch-solution-contract:v1
event-id: <lowercase UUID>
contract-id: <stable ID>
issue: <number>
revision: <positive integer>
supersedes-url: <prior immutable contract URL or none>
implementation-base: <full SHA>
owner-worker: <stable worker/person identity>
owner-github-actor: <GitHub login>
scope: <bounded single-line file and semantic scope>
timestamp: <UTC RFC 3339>
-->
```

```text
<!-- finch-solution-contract-approval:v1
event-id: <lowercase UUID>
issue: <number>
contract-id: <stable ID>
contract-url: <immutable numeric-comment URL>
contract-digest: <SHA-256 of exact raw contract comment>
contract-revision: <positive integer>
implementation-base: <full SHA>
reviewer-worker: <fresh independent worker identity>
reviewer-github-actor: <responsible GitHub login>
verdict: <APPROVE|REVISE>
review-output-url: <immutable review evidence URL>
authority-comment: <prior operation-specific delegation URL or none>
timestamp: <UTC RFC 3339>
-->
```

An approval is admitted only after its contract is admitted. It must bind the exact ID,
URL, digest, revision, and base. Its GitHub author must be the reviewer actor, unless an
earlier immutable delegation names the contract, reviewer worker, substitute poster, one
verdict, and output identity. The contract author, coordinator, and implementer workers
cannot approve their own contract. Invalid attempts do not consume IDs or predecessors.
Edited/deleted accepted events, incomplete retrieval, or two valid successors from one
revision make the affected history `INDETERMINATE` and nonauthorizing.

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
