# Finch implementation task packet

Give each independent collaborator all of the following. Replace every placeholder; do not send a collaborator to rediscover the coordination state.

```text
Issue and outcome
- GitHub issue: #<number> — <title>
- Concrete outcome: <observable behavior and acceptance gate>
- Readiness state/event: <state and immutable finch-issue-readiness:v1 event URL>
- Work claim: <claim id and issue-comment URL>
- Effective claim scope/revision event: <scope and immutable URL, or original claim>
- Solution contract: <contract ID, URL/numeric comment ID, SHA-256 body digest, revision>
- Contract approval: <APPROVE comment URL and plan-reviewer identity>

Starting point
- Repository: <absolute path>
- Branch/worktree: <dedicated branch and absolute worktree>
- Immutable claim/implementation base: <full 40-character SHA>
- Current integration base: <full 40-character current-main SHA>
- Exact candidate tip: <full 40-character SHA or none before implementation>
- Relevant files/docs/commits: <paths and immutable SHAs>

Dependencies and assumptions
- Depends on: <issues/commits>
- Blocks: <issues/gates>
- Confirmed facts: <facts the agent may rely on>
- Unverified assumptions: <claims that require tests or live acceptance>

Authority and scope
- May edit: <bounded areas>
- Must not edit: <overlapping work or excluded areas>
- Role: <implementer, plan reviewer, finding reviewer, verifier, or coordinator>
- Prototype authority: <none, or disposable exact-tip worktree/branch only>
- Merge authority: <accountable actor and exact scope, or none>
- Issue-close authority: <accountable actor and exact issue, or none>
- Claim-terminal authority: <original issuer/proxy and permitted event/disposition, or none>
- Worktree-cleanup authority: <accountable actor and proven-safe targets, or none>
- External-effect authority: <specific permitted action, target, and credential boundary, or none>
- No credential access, destructive cleanup, external messages, or merge authority unless explicitly stated.
- Reviewer prototypes must not mutate the frozen worktree, push, use credentials, perform
  external effects, merge, or cherry-pick; the implementer reproduces any correction.
- Preserve unrelated changes and avoid broad formatting.

Required verification
- Regression that fails before and passes after: <production boundary>
- Focused tests/static checks: <commands or CI jobs>
- Cross-platform/feature/release coverage: <required matrix>
- Independent review: <security/authority/persistence/provider/etc. or not required>
- Finding/ledger IDs: <stable finding IDs, ledger identity, successor issue/claim IDs>
- Acceptance-gate ownership: <each gate's one current owner and current-main proof path>
- Resource constraints: <local/remote build and memory limits>

Expected deliverable
- Coherent commits pushed to the assigned branch.
- Clean worktree and exact final SHA.
- PR or handoff with changed files, named regression, test/CI links, residual risks, and merge recommendation.
- Final evidence: <SAFE TO MERGE or ESCALATE WITH EXECUTABLE REPAIR/SPLIT; post-merge
  current-main artifact/tree-equivalence and user-visible proof when completing>
- Do not merge, close issues, or remove the worktree unless explicitly authorized.
- Publish the required claim terminal event when ownership ends.
```

## Discovery task packet

`NEEDS_SPECIFICATION` work uses a smaller packet and never an implementation packet or
production claim. Name the issue, discovery-contract URL/digest, exact missing decision,
concrete question, decision owner, nearest minimally specified outcome, bounded commands,
and evidence destination. Authority is either read-only inspection or a disposable local
worktree/branch. It excludes production-worktree mutation, push, credentials, undeclared
network or external effects, merge, cherry-pick, and retention of prototype commits.
Record the command/result, useful evidence, and proven cleanup; a prototype cannot be
promoted by cherry-pick and must be reproduced after READY under a production claim.
