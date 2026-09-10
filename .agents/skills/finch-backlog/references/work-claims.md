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

Save the returned URL and immutable observation: comment ID, author login, `createdAt`, REST
`updatedAt`, GraphQL `lastEditedAt`, SHA-256 of the exact raw body, and original issuer. Never
edit/delete an event. Accept it only when GitHub directly proves REST `updatedAt == createdAt`,
GraphQL `lastEditedAt == null`, and the retrieved raw body has the saved SHA-256 digest. A changed
digest, edit, missing saved URL, or incomplete retrieval is an ownership-integrity failure; stop
instead of reconstructing intent.

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

### Resolve a corroborated malformed attempt

A malformed comment never becomes a claim, cannot receive a v1 terminal, and grants no ownership.
If its corroborated live work blocks reuse, only the original comment author may publish a new,
immutable cessation attestation naming the malformed comment URL, numeric comment ID, and SHA-256
body digest. Verify
that attestation with REST `updatedAt == createdAt` and GraphQL `lastEditedAt == null`.

Before recording `RESOLVED_MALFORMED_NO_OWNER`, directly inspect or contact every identifiable
worker and verify there is no running worker, mutating PR, or live session. Record the exact dirty,
unpushed, and unique-commit state of every branch/worktree, preserve all valuable work, and retain
the malformed comment and attestation as diagnostics. A clean pushed branch may remain as
read-only evidence. If the original author is unavailable, liveness is unknown, or unique work is
not preserved, the malformed attempt remains blocking.

`RESOLVED_MALFORMED_NO_OWNER` is diagnostic only: it is not a claim, terminal, retroactive
validation, authority grant, or proof that any acceptance gate is discharged. Before anyone
reuses the scope, require a READY outcome with an approved immutable contract, a new dedicated
branch/worktree, a fresh valid v1 claim, and complete collision checks both before and after that
claim. Cherry-pick or reimplement preserved work only after those prerequisites hold.

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
