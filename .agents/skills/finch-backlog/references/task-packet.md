# Finch implementation task packet

Give each independent collaborator all of the following. Replace every placeholder; do not send a collaborator to rediscover the coordination state.

```text
Issue and outcome
- GitHub issue: #<number> — <title>
- Concrete outcome: <observable behavior and acceptance gate>
- Work claim: <claim id and issue-comment URL>

Starting point
- Repository: <absolute path>
- Branch/worktree: <dedicated branch and absolute worktree>
- Base commit: <exact origin/main SHA>
- Relevant files/docs/commits: <paths and immutable SHAs>

Dependencies and assumptions
- Depends on: <issues/commits>
- Blocks: <issues/gates>
- Confirmed facts: <facts the agent may rely on>
- Unverified assumptions: <claims that require tests or live acceptance>

Authority and scope
- May edit: <bounded areas>
- Must not edit: <overlapping work or excluded areas>
- No credential access, destructive cleanup, external messages, or merge authority unless explicitly stated.
- Preserve unrelated changes and avoid broad formatting.

Required verification
- Regression that fails before and passes after: <production boundary>
- Focused tests/static checks: <commands or CI jobs>
- Cross-platform/feature/release coverage: <required matrix>
- Independent review: <security/authority/persistence/provider/etc. or not required>
- Resource constraints: <local/remote build and memory limits>

Expected deliverable
- Coherent commits pushed to the assigned branch.
- Clean worktree and exact final SHA.
- PR or handoff with changed files, named regression, test/CI links, residual risks, and merge recommendation.
- Do not merge, close issues, or remove the worktree unless explicitly authorized.
- Publish the required claim terminal event when ownership ends.
```
