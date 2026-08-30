# Finch work-claim protocol

The versioned GitHub issue-comment events below are the only authoritative cross-tool ownership record. Do not replace them with assignees, labels, project fields, branches, pull requests, local files, or prose comments. Those surfaces may aid humans but do not acquire or release a claim.

If a worker cannot post a comment and obtain its GitHub URL, it must not start implementation. A coordinator with GitHub access may post the event on the worker's behalf using the worker's real identity.

## Claim an issue

After creating the branch/worktree and before editing production files, post a human-readable summary followed by exactly this machine-readable block:

```text
Claiming implementation of #<issue> for <bounded outcome and file/semantic scope>.

<!-- finch-work-claim:v1
event: claim
claim-id: <globally unique stable id>
worker: <tool/person and stable session or agent identity>
github-actor: <responsible @user or none>
branch: <remote branch>
worktree: <absolute path or remote environment id>
base: <full commit SHA>
scope: <single-line bounded scope>
timestamp: <UTC RFC 3339>
-->
```

Use a lowercase UUID for `claim-id`, a full 40-character commit for `base`, a single-line `scope`, and UTC RFC 3339 seconds for `timestamp`. Every field is required; use the literal `none` only for `github-actor` or `worktree` when genuinely unavailable. Never put credentials, host secrets, private prompts, or untrusted multiline content in the block.

Save the returned GitHub comment URL. Immediately reread all `finch-work-claim:v1` events on the issue before editing. If two active claims overlap, the claim whose GitHub comment has the earlier `createdAt` wins; if equal, the lower numeric GitHub comment ID wins. The later claimant must post a `release` event and select non-overlapping work. Client-supplied `timestamp` never decides a collision.

## Determine whether a claim is active

Process claim events in GitHub `createdAt` order, breaking ties by numeric comment ID. A claim remains active until a later `release`, `complete`, or `supersede` event names the same `claim-id`. Ignore malformed blocks as ownership records and report them as diagnostics rather than guessing their intent.

At every required claim check:

1. Search all open issues in the repository for comments containing the exact marker `finch-work-claim:v1`.
2. Fetch the full comments for every matching issue; do not decide ownership from truncated search snippets.
3. Parse valid events, reduce each `claim-id` to active or terminal state, and retain the GitHub comment URL, `createdAt`, and numeric comment ID.
4. Compare the proposed issue, files, and semantic authority against every active claim, including claims on different issues.
5. Cross-check matching branches, pull requests, worktrees, and running workers. These are evidence about scope and liveness, not substitute claim records.

Fail closed if GitHub pagination, authentication, rate limits, network errors, or malformed response data make the repository-wide result incomplete. Do not interpret “search failed” as “no claims.”

Do not treat age alone as proof that a claim is abandoned. Cross-check the named branch, pull request, worktree, running-agent state, and recent issue activity. If ownership cannot be established safely, ask or select another unblocked issue.

Two claims conflict when their promised file sets or semantic authority overlap, even if their issue numbers differ. Two claims on the same parent issue may coexist only when their scopes are independently testable and explicitly disjoint. When overlap is uncertain, treat it as a conflict until the workers or coordinator record disjoint scopes.

## End or transfer ownership

Post exactly one terminal event when the work merges, pauses indefinitely, is handed off, or is proven superseded:

```text
Releasing claim `<claim-id>`: <merged, handed off, blocked, or superseded reason and evidence>.

<!-- finch-work-claim:v1
event: <release|complete|supersede>
claim-id: <the original claim id>
worker: <tool/person and stable session or agent identity>
timestamp: <UTC RFC 3339>
replacement-claim: <claim id or none>
-->
```

Do not remove another worker's assignee or declare its claim superseded without evidence. A coordinator may record a transfer after confirming it with the current worker or proving its branch/worktree safely abandoned.
