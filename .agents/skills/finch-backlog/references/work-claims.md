# Finch work-claim protocol

Use issue comments as the cross-tool ownership record. Assignees identify the responsible GitHub user; claim events distinguish concurrent people, Codex sessions, Claude sessions, and other workers that may share that account.

## Claim an issue

Before editing production files, post a human-readable summary followed by this machine-readable block:

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

Use an opaque UUID or an equally collision-resistant value for `claim-id`. Never put credentials, host secrets, private prompts, or untrusted multiline content in the block.

## Determine whether a claim is active

Process claim events for the issue in timestamp order. A claim remains active until a later `release`, `complete`, or `supersede` event names the same `claim-id`.

Do not treat age alone as proof that a claim is abandoned. Cross-check the named branch, pull request, worktree, running-agent state, and recent issue activity. If ownership cannot be established safely, ask or select another unblocked issue.

Two claims conflict when their promised file sets or semantic authority overlap, even if their issue numbers differ. Two claims on the same parent issue may coexist only when their scopes are independently testable and explicitly disjoint.

## End or transfer ownership

Post a terminal event when the work merges, pauses indefinitely, is handed off, or is proven superseded:

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
