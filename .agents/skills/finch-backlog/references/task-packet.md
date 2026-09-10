# Finch implementation task packet

Give each collaborator the complete packet below. Replace every placeholder; never rely on
shared conversational memory or ask the recipient to rediscover coordination state.

```text
Issue and outcome
- GitHub issue: #<number> — <plain-language title>
- Observable outcome and acceptance gates: <behavior and every gate>
- Approved solution contract: <ID, revision, immutable URL, digest, approval URL>
- Work claim: <claim ID and immutable issue-comment URL>

Starting point
- Repository/worktree: <absolute path>
- Branch: <dedicated remote branch>
- Claim base: <exact full SHA used for fail-before proof>
- Integration base: <current-main SHA, or who must establish it and when>
- Frozen tip: <exact full SHA when reviewing>
- Relevant files/docs/commits: <paths and immutable identities>

Dependencies and accounting
- Depends on / blocks: <issues with plain-language outcomes>
- Original gates and current owner/proof path for each: <exhaustive map>
- Confirmed facts: <facts the worker may rely on>
- Unverified assumptions: <facts requiring tests or live acceptance>

Authority (each row occurs exactly once; begin with explicit YES or NO and give nonempty bounds)
- Edit authority: <YES | NO> — <allowed files and semantic bounds>
- Disposable prototype authority: <YES | NO> — <location, effects, and retention>
- External-action/message authority: <YES | NO> — <service and allowed operations>
- Push authority: <YES | NO> — <exact remote branch>
- Merge authority: <YES | NO> — <PR/branch and prerequisites>
- Issue-close authority: <YES | NO> — <issue and prerequisites>
- Claim-terminal authority: <YES | NO> — <claim/event/issuer evidence>
- Cleanup authority: <YES | NO> — <exact worktree/artifacts and safety proof>
- Forbidden scope: <overlap, credentials, destructive or unrelated work>

Required verification
- Named regression and why it fails on the claim base: <production boundary>
- Focused checks and useful failure diagnostics: <commands>
- Integration/artifact/user-visible proof: <identity and method>
- Independent review perspectives and exact convergence rule: <requirements>
- Cross-platform/feature/CI and resource limits: <matrix/budget>

Expected deliverable
- Coherent pushed commit(s), clean worktree, exact final SHA.
- Changed-file list, fail-before/pass-after evidence, review record, residual risks.
- Do not exercise any authority marked no or omitted.
```

Reviewer/discovery packets additionally state that prototypes use an isolated disposable copy at
the frozen tip and cannot mutate the implementation worktree, push, read credentials, perform
undeclared external effects, merge/cherry-pick, or retain production commits. If a required
reviewer is unavailable, record `UNAVAILABLE`, try at most one bounded fresh-context fallback,
and leave the gate unresolved absent explicit repository-owner risk acceptance.
