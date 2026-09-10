# Finch work-claim protocol

The versioned GitHub issue-comment events below are the only authoritative cross-tool ownership record. Do not replace them with assignees, labels, project fields, branches, pull requests, local files, or prose comments. Those surfaces may aid humans but do not acquire or release a claim.

If a worker cannot post a comment and obtain its GitHub URL, it must not start implementation. A coordinator with GitHub access may post the event on the worker's behalf using the worker's real identity.

## Claim an issue

After creating the branch/worktree and before editing production files, post a human-readable summary followed by exactly this machine-readable block. The machine-readable block is an HTML comment and does not render on GitHub, so the human-readable line must itself name the owning worker and its GitHub actor; a reader must be able to tell who holds the issue without viewing the comment source. The names in that line must match the block's `worker` and `github-actor` values exactly.

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

Use a lowercase UUID for `claim-id`, a full 40-character commit for `base`, a single-line `scope`, and UTC RFC 3339 seconds for `timestamp`. Make `worker` identify the tool and session distinctly enough that a reader can tell two concurrent workers apart, including when both post under the same `github-actor`. Every field is required; use the literal `none` only for `github-actor` or `worktree` when genuinely unavailable. Never put credentials, host secrets, private prompts, or untrusted multiline content in the block.

Save the returned GitHub comment URL. Immediately reread all `finch-work-claim:v1` events on the issue before editing. If two active claims overlap, the claim whose GitHub comment has the earlier `createdAt` wins; if equal, the lower numeric GitHub comment ID wins. The later claimant must post a `release` event and select non-overlapping work. Client-supplied `timestamp` never decides a collision.

Claim events are append-only records. Never edit or delete an event comment. Record
its comment ID, URL, author login, `createdAt`, `updatedAt`/`lastEditedAt`, and a
SHA-256 digest of its exact body when first observed. An event is valid only when
GitHub reports that it has never been edited (`updatedAt == createdAt` through the
REST API, or `lastEditedAt == null` through GraphQL). Treat an edited event, a
changed digest, or a previously recorded event URL that no longer resolves as an
ownership-integrity failure and stop rather than reconstructing intent.

## Determine whether a claim is active

Process immutable claim events in GitHub `createdAt` order, breaking ties by numeric comment ID. A claim remains active until a later valid, issuer-authorized `release`, `complete`, or `supersede` event names the same `claim-id`. Ignore malformed blocks as ownership records and report them as diagnostics rather than guessing their intent. A block is malformed when it is not wrapped in an HTML comment, omits `event`, or renames a required field (for example `base-sha` for `base`, or `claimed-at` for `timestamp`). When a malformed block is nonetheless corroborated by a live branch, worktree, or running worker, do not take the issue: treat the corroborated work as active, report the defect, and select other work.

Order multiple blocks in one comment by block order after `createdAt` and numeric comment
ID. Authenticate and validate an attempted event before admitting its ID, predecessor, or
ownership effect. Malformed, unauthorized, unknown, or semantically invalid attempts are
diagnostics and do not poison a corrected successor. Edited/deleted/digest-changed
previously accepted records, incomplete authoritative retrieval, or multiple individually
valid successors from one accepted predecessor makes the relevant ownership history
`INDETERMINATE` and nonauthorizing.

After the issue-406 workflow PR merges, one immutable `finch-workflow-cutover:v1` event on
that issue records its merge SHA, tree, and timestamp. Only claim comments that predate it
are eligible for the two-step legacy bootstrap in the readiness protocol.

At every required claim check:

1. Search all open issues in the repository for comments containing the exact marker `finch-work-claim:v1`.
2. Fetch the full comments for every matching issue; do not decide ownership from truncated search snippets.
3. Parse valid events, verify their immutable metadata and issuer authority, reduce each `claim-id` to active or terminal state, and retain the GitHub comment URL, author login, `createdAt`, `updatedAt`/`lastEditedAt`, body digest, and numeric comment ID.
4. Compare the proposed issue, files, and semantic authority against every active claim, including claims on different issues.
5. Cross-check matching branches, pull requests, worktrees, and running workers. These are evidence about scope and liveness, not substitute claim records.

Fail closed if GitHub pagination, authentication, rate limits, network errors, comment immutability, saved-event retrieval, or malformed response data make the repository-wide result incomplete. Do not interpret “search failed” or “event disappeared” as “no claims.”

Do not treat age alone as proof that a claim is abandoned. Cross-check the named branch, pull request, worktree, running-agent state, and recent issue activity. If ownership cannot be established safely, ask or select another unblocked issue.

Two claims conflict when their promised file sets or semantic authority overlap, even if their issue numbers differ. Two claims on the same parent issue may coexist only when their scopes are independently testable and explicitly disjoint. When overlap is uncertain, treat it as a conflict until the workers or coordinator record disjoint scopes.

## Revise a claim's scope

Never edit a claim event or silently reinterpret its `scope`. A contract revision that
widens, narrows, or transfers production authority requires an append-only scope event by
the original claim issuer:

```text
<!-- finch-work-claim:v1
event: scope-revise
claim-id: <the original claim id>
worker: <the exact worker value from the original claim>
prior-scope: <the exact currently effective scope>
new-scope: <single-line bounded replacement scope>
contract-id: <approved revised contract ID>
contract-url: <immutable contract comment URL>
approval-url: <immutable approval comment URL>
timestamp: <UTC RFC 3339>
authority-comment: <none or immutable prior authorization URL>
-->
```

Verify the revised contract and approval, issuer authority, and exact `prior-scope` before
accepting the event. A mismatch is invalid and leaves the earlier scope effective. Before
widening, repeat the repository-wide collision check and establish that no active claim
overlaps the added files or semantic authority; only then append `scope-revise` and repeat
the collision check before mutation.

Delegated `scope-revise` uses the terminal-event author binding. A substitute needs an
earlier immutable authorization from the original author naming the claim ID, substitute
login, `scope-revise`, approved contract revision, and exact permitted new scope. Late,
edited, general, unrelated, or broader authorization is invalid.

## Transfer scope atomically

The child-first sequence is represented by a single reducible transaction, not by
temporarily colliding active claims. For a split, the original issuer first appends
`split-reserve`, naming a transfer ID, retained/delegated scopes, exhaustive gate map,
approved child issue/contract/claim IDs, and expected workers. The parent remains the only
active mutation owner. Each child appends `split-child-reserve` with the exact transfer,
scope, contract, branch, and worktree; the reservation is nonauthorizing. The original
issuer appends `split-activate` only after every expected reservation validates. Reduction
then atomically narrows the parent and activates every child.

```text
<!-- finch-work-claim:v1
event: <split-reserve|split-child-reserve|split-activate>
event-id: <lowercase UUID>
transfer-id: <stable transfer UUID>
claim-id: <parent or reserved child claim ID>
worker: <exact worker identity>
scope: <exact retained/delegated scope>
contract-id: <approved contract ID>
gate-map-url: <immutable exhaustive map URL>
branch: <branch or none for parent reservation>
worktree: <worktree or none>
nonauthorizing: <true for child reservation; false otherwise>
timestamp: <UTC RFC 3339>
authority-comment: <prior operation-specific authorization URL or none>
-->
```

Missing, extra, overlapping, or overbroad reservations leave the parent unchanged and all
children nonauthorizing. Ordinary overlapping-scope handoff uses the same atomic shape
with `handoff-reserve`, one `handoff-child-reserve`, and `handoff-activate`. Activation
atomically terminalizes the old claim and activates its exact successor; readiness then
uses the corresponding IN_PROGRESS or REPAIR_IN_PROGRESS self-edge. Incomplete handoff
leaves the original owner active. Client timestamps never decide either transaction.

For a same-contract split, use this collision-free order: check all active claims; create
and approve immutable disjoint child contracts; append the parent reservation; append
every nonauthorizing child reservation; repeat the repository-wide collision check; map
every inherited acceptance gate and proof exactly once; activate the transaction; append
the finding's non-discharging `SPLIT-TO` event; then append any parent `scope-revise` event.
Never narrow the parent first: doing so creates an unowned interval. Never activate child
claims before the parent transaction: doing so permits an ownership collision. Parent and
children remain open until recursive successor obligations resolve on current main.
Never claim children after narrowing; reservations precede atomic activation while the
parent remains authoritative.

## End or transfer ownership

Post exactly one terminal event when the work merges, pauses indefinitely, is handed off,
or is proven superseded. A same-contract split is not by itself terminal; terminal
supersession requires already-created replacement claims covering every inherited gate:

```text
`<worker>` (<github-actor>) is releasing claim `<claim-id>`: <merged, handed off, blocked, or superseded reason and evidence>.

<!-- finch-work-claim:v1
event: <release|complete|supersede>
claim-id: <the original claim id>
worker: <the exact worker value from the original claim>
disposition: <return-ready|blocked-external|needs-specification|infeasible|declined|slice-complete|outcome-superseded|legacy>
timestamp: <UTC RFC 3339>
replacement-claim: <claim id or none>
evidence-url: <immutable recovery/merge/disposition evidence URL>
authority-comment: <none or immutable prior GitHub comment URL>
-->
```

After workflow cutover, this disposition set is closed. A voluntary release uses
`return-ready`; an external pause uses `blocked-external`; contract invalidation uses
`needs-specification`; impossibility and owner choice use `infeasible` and `declined`;
successful scoped work uses `slice-complete`; whole-outcome replacement uses
`outcome-superseded`. `legacy` describes only valid pre-cutover terminal events. No other
post-cutover disposition authorizes a terminal.

For `return-ready`, `blocked-external`, `needs-specification`, `infeasible`, and
`declined`, post the terminal first and the matching readiness event second. That event
binds the exact terminal URL, issue, claim ID, disposition, worker/author authority, and
evidence digest. Until it is admitted, mutation is unauthorized and status reports
`PAIRING_REQUIRED`; a wrong destination, disposition, actor, claim, or evidence identity
is invalid. `infeasible` and `declined` additionally require immutable contract-owner
disposition. Successful outcome completion is ordered READY_TO_MERGE, merge/tree proof,
claim terminal, issue closure, then readiness COMPLETE. A successful slice with successors
keeps the issue in REPAIR_IN_PROGRESS and may terminalize only after merged-slice proof and
already-active successors are recorded.

The terminal event's GitHub comment author must equal the original claim comment
author, and its `worker` must byte-for-byte equal the original claim's `worker`.
Use `authority-comment: none` in that ordinary case. A different GitHub author is
valid only when `authority-comment` links to an earlier, unedited comment by the
original claim author that explicitly names the claim ID, the substitute GitHub
login, and the permitted terminal event. Verify the linked comment directly and
record its immutable metadata before accepting the terminal event. A repository
role, assignee, label, branch, or claimed coordinator title does not substitute
for that authorization.

Do not remove another worker's assignee or terminalize its claim without this
issuer evidence. If the original author is unavailable and no immutable prior
authorization exists, leave the claim active, report the impasse, and obtain user
direction rather than inventing abandonment authority. A handoff creates a new
claim reservation; only the old issuer's valid `handoff-activate` atomically ends the old
claim and activates the named replacement.
