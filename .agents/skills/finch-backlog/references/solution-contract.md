# Finch solution-contract protocol

Every production change starts from an immutable, explicitly approved solution contract. It
defines the observable outcome, constraints, gates, and proof against which claims,
implementation, findings, integration, and completion are reviewed. A branch or PR is not a
contract, and a contract is not mutation ownership.

## Identity and approval checklist

Publish the contract as an append-only GitHub issue comment and independently verify:

- stable contract ID and revision, numeric comment URL/ID, unedited metadata, and SHA-256 of
  the exact raw body;
- full claim-base SHA, owner identity, exact bounded scope, and superseded revision if any;
- a separate immutable approval comment binding that ID, URL, digest, revision, and base;
- a fresh independent reviewer identity and explicit `APPROVE` verdict; and
- complete retrieval with no edited, deleted, mismatched, duplicate, or ambiguous artifact.

The coordinator records the checklist decision. A parser, status tool, label, or caller-provided
boolean cannot authenticate the artifacts or grant `READY`. Recheck the literal immutable
identities before task handoff, production edit, scope change, and merge. Keep the claim base
for historical regression lineage and record current integration base separately.

## Retained PR #542 protocol gates

- `P542-01`: issue readiness, candidate correction, and finding attributes are distinct concepts.
- `P542-02`: procedural READY requires an immutable solution contract and fresh independent approval before any production branch, worktree, claim, mutation, or implementation. The coordinator records the checklist decision; no status script grants authority.
- `P542-03`: reviewers are find-and-help partners; every confirmed finding includes a stable ID, concrete failure, smallest credible correction vector, deterministic proof, affected invariant, and contract-fit assessment.
- `P542-04`: confidence, severity, locality, obligation, and lifecycle axes remain independent and append-only.
- `P542-05`: severity and finding count prioritize work but never cancel, close, waive, resolve, reject, or automatically split it.
- `P542-06`: confirmed same-contract blockers and required regression debt stay in bounded repair; only causally separable concerns become owned independent follow-ups.
- `P542-07`: replacement and split records do not discharge obligations. Every original acceptance gate keeps exactly one current owner and proof path until its leaf is merged and proven. This is an accounting discipline checked by reviewers/coordinator, not a reducer-derived fact.
- `P542-08`: when the same stable blocker survives two competent repair attempts, end the implementation epoch, run one bounded independent diagnosis/prototype, and change strategy, representation, contract, assignment, or executable split. Agent failure never proves infeasibility.
- `P542-09`: review converges only when the transitive same-contract blocker/regression ledger is zero and exactly one fresh independent clean exact-tip pass finds no new confirmed blocker. Do not keep reviewing that frozen blocker-free tip.
- `P542-10`: task packets explicitly name edit, prototype, external action, push, merge, issue closure, terminal-event, and cleanup authority.
- `P542-11`: outcome completion requires current-main merge, every accepted gate, ticket closure, current artifact/user-visible proof, valid claim terminal, safe cleanup, and frontier recomputation.
- `P542-12`: a narrowed PR slice may merge while a broader parent remains open for concretely owned successors; PR integration and outcome completion are distinct.
- `P542-13`: `finch-work-claim:v1` remains the sole mutation-ownership authority. There is no deferred cutover.
- `P542-14`: obvious isolated fixes use compact contracts and one fresh plan reviewer; additional perspectives are derived only from actual diff risk.
- `P542-15`: reviewer/discovery prototypes use an isolated disposable worktree/copy at the frozen tip and never mutate the implementation worktree, push, read credentials, perform undeclared external effects, merge/cherry-pick, or retain production commits.
- `P542-16`: a required unavailable reviewer is recorded `UNAVAILABLE` with reason and gets at most one bounded fresh-context fallback; failure leaves the gate unresolved unless the repository owner explicitly accepts the named risk.
- `P542-17`: pre-cutover wording is removed. Any scope expansion requires a revised immutable approved contract, a complete repository-wide v1 collision check, and an issuer-authorized whole-claim replacement covering the new scope before edits, followed by another collision check.

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
sections: <observed_failure,intended_behavior,non_goals,invariants,boundaries,regression_proof,integration_proof,reversion_plan,hostile_cases,gate_ownership>
timestamp: <UTC RFC 3339>
-->
```

Approval is recorded in human-readable immutable prose with the exact identity fields above,
reviewer, verdict, failure analysis, and correction disposition. It deliberately does not form
a machine admission or proxy-authorization record. If a coordinator posts a reviewer's result,
the prose names both identities and reviewers independently verify the record.

## Required content

Compact and expanded contracts both state:

- reproduced failure at the production boundary and observable user outcome;
- explicit non-goals, accepted constraints, invariants, hostile cases, and assumptions;
- every acceptance gate, each with exactly one current owner and proof path;
- ownership/API boundary and exact allowed/excluded files;
- deterministic fail-before/pass-after regression and why the claim base fails;
- current-main integration plus artifact/tree-equivalence/user-visible proof;
- inherited work and how it is reused, narrowed, or conservatively superseded; and
- finite review, reversion/deletion, safe cleanup, closure, and frontier accounting.

An obvious isolated fix may use a compact expression of every item and one fresh plan reviewer.
Additional perspectives are derived only from actual correctness, authority, persistence,
compatibility, lifecycle, concurrency, or test risk.

The approved contract is immutable. If evidence changes outcome, constraints, files, ownership,
API boundary, gates, or proof, stop edits. Publish a new revision and obtain fresh approval,
perform a repository-wide v1 collision check, publish an issuer-authorized whole-claim
replacement covering the complete new scope, and repeat collision checking before edits.

## Discovery and unavailable review

`NEEDS_SPECIFICATION` may use a compact discovery contract recording reproduction evidence,
the exact missing decision, a concrete question, decision owner, nearest minimally specified
outcome, and a bounded read-only or isolated disposable-prototype plan. It grants no production
mutation authority. Prototypes never alter the implementation worktree, push, read credentials,
make undeclared external effects, merge/cherry-pick, or retain production commits.

Record a required unavailable reviewer as `UNAVAILABLE` with the reason and try at most one
bounded fresh-context fallback. Failure keeps the gate open unless the repository owner
explicitly accepts the named risk.

## Successor and completion discipline

Splits and replacements never discharge acceptance obligations. Before handoff, enumerate
exhaustive disjoint scopes and preserve one owner/proof path for every gate through its merged,
current-main leaf. Use the conservative serialized claim procedure in
[work claims](work-claims.md); do not represent the transition as atomic.

Completion is current-main and user-visible, not a contract, status, or review declaration.
It requires every accepted gate, merge/current-main identity, current artifact or visible proof,
claim terminal, ticket closure, safe cleanup, and post-closure frontier accounting in the order
the controlling contract specifies. A narrow PR may integrate while its parent remains open for
owned successors.
