# Finch work-claim protocol

The existing versioned GitHub issue-comment events below are the sole authoritative cross-tool
mutation-ownership record. Do not replace them with readiness records, assignees, labels,
projects, branches, PRs, local files, status reports, or prose. This remains the v1 protocol;
there is no cutover or alternate ownership authority.

If a worker cannot post a claim and obtain its GitHub URL, it must not start implementation. A
coordinator with GitHub access may post for the worker while naming the worker's real identity.

## Claim an issue

After creating the branch/worktree and before production edits, post a human-readable summary
followed by exactly this base-compatible block. The rendered summary must name the same `worker`
and `github-actor` as the block.

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

Use a lowercase UUID, full 40-character base, single-line scope, and UTC RFC 3339 seconds. Every
field is required; literal `none` is allowed only for genuinely unavailable `github-actor` or
`worktree`. Never include credentials, secrets, private prompts, or untrusted multiline content.

Save the returned URL and immutable observation: comment ID, author login, `createdAt`,
`updatedAt`/`lastEditedAt`, exact-body digest, and original issuer. Never edit/delete an event.
Accept it only when GitHub directly shows it unedited. A changed digest, edit, missing saved URL,
or incomplete retrieval is an ownership-integrity failure; stop instead of reconstructing intent.

## Repository-wide collision procedure

At each required check:

1. search every open repository issue for exact marker `finch-work-claim:v1`;
2. fetch every full comment across all pages, never deciding from snippets;
3. inspect valid immutable claims and later valid issuer-authorized terminal events in GitHub
   `createdAt` order, breaking ties by numeric comment ID;
4. retain all immutable metadata, body digest, URL, issuer, claim ID, bounded file/semantic scope,
   and active/terminal conclusion with diagnostics for malformed records;
5. compare the proposed issue/files/semantics to every active claim; and
6. corroborate branches, PRs, worktrees, and workers as liveness evidence, never ownership.

Fail closed on pagination, authentication, rate limit, network, immutability, retrieval, or
response ambiguity. A malformed claim corroborated by live work blocks competing mutation until
resolved. Age never proves abandonment. If ownership cannot be safely established, ask or choose
other work.

Two claims conflict when file sets or semantic authority overlap, even on different issues. Same
parent claims coexist only with independently testable explicitly disjoint scopes. When uncertain,
treat overlap as a conflict. Among overlapping active claims, earlier GitHub `createdAt` wins; a
tie uses lower numeric comment ID. Client `timestamp` never decides. The later claimant publishes
an authorized `release` and does not edit.

## End ownership

A claim stays active until a later immutable issuer-authorized `release`, `complete`, or
`supersede` names that claim ID. Post one terminal event when work merges, pauses indefinitely,
is handed off, or is proven superseded:

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

The terminal author must equal the claim author and `worker` must byte-for-byte match. Use
`authority-comment: none` then. Another author is valid only when the field links an earlier
unedited comment by the original issuer explicitly naming claim ID, substitute login, and allowed
terminal event. Verify it directly. Repository role, coordinator title, assignment, label, branch,
or PR does not substitute. Without issuer evidence, leave the claim active and obtain direction.

## Scope expansion and conservative handoff

Scope expansion requires an immutable revised approved contract and complete collision check,
then an issuer-authorized whole-claim replacement covering the new scope, followed by another
repository-wide check before edits. Never append a partial scope that obscures retained ownership.

For a split/handoff, do not claim atomic transfer:

1. keep the parent claim active while approving exhaustive disjoint child scopes and mapping
   every inherited gate to exactly one intended owner/proof path;
2. have the original issuer publish the allowed terminal/supersession or covering whole-claim
   replacement, preserving valuable branch/commit evidence;
3. recompute all repository claims;
4. let child workers claim only released disjoint scope;
5. recompute again before any child edit; and
6. keep all unclaimed obligations visibly open and the parent outcome open until every leaf is
   merged and proven on current main.

A temporary no-owner interval authorizes no mutation. It is acceptable; overlapping active
mutation claims are not. A terminal, replacement, successor link, or split record never by itself
discharges an acceptance gate.
