# Finch work-claim protocol

For backlog-wrapper work, the existing `finch-work-claim:v1` comment is the sole recorded ownership
mechanism. It helps people avoid editing the same files or semantics concurrently while remaining a
coordination record, not cryptographic authentication.

Before editing, perform a procedural conflict check across open work: inspect active claim comments,
branches, pull requests, worktrees, and reachable workers. Compare both file scope and semantic
scope. If evidence is incomplete or overlap is plausible, pause and coordinate. Age, assignment,
labels, branches, and status reports do not by themselves release a claim.
When active backlog claims overlap, the earlier GitHub `createdAt` wins; if those timestamps are
equal, the lower numeric comment ID wins. The later claimant stops and coordinates.

## Claim an issue

Post a short human-readable summary followed by this block:

```text
`<worker>` (<github-actor>) is claiming implementation of #<issue> for <bounded outcome and file/semantic scope>.

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

Use a unique lowercase UUID, a full base commit, and a bounded single-line scope. Keep credentials,
private prompts, and untrusted multiline content out of the comment. Save the returned URL and check
that the rendered summary and fields describe the same worker and scope.

Recheck conflicts after claiming and before expanding scope or merging. A claim remains active until
the original worker/issuer records an allowed terminal event or explicitly authorizes another person
to do so. When liveness or intent is ambiguous, coordinate with the named people; do not invent a
machine-derived ownership conclusion.

## End ownership

Post a readable reason followed by this compatible terminal block:

```text
`<worker>` (<github-actor>) is releasing claim `<claim-id>`: <merged, handed off, blocked, or superseded reason and evidence>.

<!-- finch-work-claim:v1
event: <release|complete|supersede>
claim-id: <the original claim id>
worker: <the exact worker value from the original claim>
timestamp: <UTC RFC 3339>
replacement-claim: <claim id or none>
authority-comment: <none or immutable prior GitHub comment URL>
-->
```

Do not edit away earlier claim history. A replacement reference does not activate new work or prove
that inherited acceptance obligations are complete.

## Handoff and split

Avoid claims of atomic transfer. Preserve valuable work, end or narrow the old ownership record,
recheck conflicts, then let the new worker claim the released scope before editing. During any gap,
no one owns mutation rights merely because a future claim is planned.

Split only genuinely separable work. Each child needs a bounded outcome, owner, and proof, while the
parent remains open for any acceptance obligation not yet integrated on current main.

Every process step must demonstrably reduce defect risk or improve shipping confidence at a cost
proportional to the change; otherwise remove it. Tooling is advisory mechanical lint, never an
authority engine.
