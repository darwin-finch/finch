# Brain Runtime Convergence Plan

Status: active as of 2026-08-24 on the tested shared typed runtime.

## Goal

Make `Brain` one daemon-authoritative logical entity. The daemon owns only durable coordination
state and the event log; it has **no** workspace/environment authority. One long-lived master
frontend runner exclusively owns the active VM, provider/task execution, workspace, accessibility,
credentials, and host-effect capabilities.
Other clients are attachments and projections. A speculative typing helper, normal model turn,
scheduled callback, and subagent are `BrainRun` instances within a Brain rather than different kinds
of Brain.

The runner is the master frontend process, not an ordinary attached client. A consultant or another
attached client appends a typed inbox/prompt event to the daemon log even when the master terminal is
detached; the leased master frontend consumes that event according to Brain policy. Consultants
never receive the workspace or credential handles merely by attaching. Attachments catch up by event
cursor. If a runner is replaced on the same authorized environment, it restores the last committed VM
checkpoint and consumes only the logged events not yet acknowledged by that runner, so “catching up”
is a deterministic runtime operation rather than an LLM being asked to reconstruct missed
instructions from prose.

If that environment-owning frontend is genuinely unavailable, a consultant-originated workspace
request becomes a durable `queued_for_environment` run state. The daemon and consultant must not
invent a file/tool result or execute the request elsewhere. An explicit, authenticated control and
environment-authority handoff is the only way another frontend may process it; otherwise it waits
until the authorized master frontend reconnects.

The daemon also owns schedule definitions, due-time calculation, and delivery bookkeeping, but never
executes their workspace effects. Its default policy is coalescing: while the master frontend is
unavailable, it retains one pending `ScheduleDue` event per schedule and updates its missed-count and
time range. Reconnection delivers that one summarized event rather than an unbounded backlog. A
schedule that genuinely requires each missed occurrence must explicitly opt into a bounded catch-up
policy (`max_catch_up`, expiry, and idempotency key required); otherwise missed ticks are summarized
or skipped according to its declared policy.

A client exit, crash, update, or deliberate detach must never destroy a Brain. The daemon retains
its event log, committed VM checkpoint/deltas, pending approvals, scheduled work, and resumable
run metadata until explicit archival or retention-policy deletion. Reopening Finch attaches a new
projection to the same named Brain and resumes its visible history; interrupted runs are surfaced
as recoverable state rather than silently forgotten.

Runner crash and reboot recovery are Brain/runtime concerns independent of the process supervisor.
The runner checkpoints serializable VM state and execute-once host-effect records at committed
boundaries. If it dies, the daemon marks the run interrupted and a newly launched runner may resume
only from a validated checkpoint; it never replays already-recorded external effects.

This work must consolidate the existing implementations. It must not create another registry,
session abstraction, or transport-specific lifecycle.

## Entry gate: one working shared VM before convergence

Brain convergence must start from one executable runtime rather than preserve competing Lisp,
Co-Forth, provider-tool, or named-session implementations. The product-critical entry gate passed
on 2026-08-24 with the following evidence:

- Co-Forth and Finch Lisp both compile source directly to the common typed stack IR and execute in
  `ProgramRuntime`; the native Lisp evaluator and Lisp-to-Forth text fallback are absent.
- Both frontends have enough shared definitions, lexical/stack bindings, conditionals, metered
  loops, closures/quotations, typed collections/results, yields, and vocabulary publication to run
  real provider-authored programs. Remaining CTFE, generic, pattern, and syntax work extends this
  surface rather than defining another runtime.
- Every production primitive is generated from one typed signature/effect/implementation registry.
- Capabilities compose transitively with bounded selectors, live policy/grant rechecks, attenuation,
  revocation, audit, and typed suspend/resume. A Brain phase may add a missing host adapter only by
  binding this broker; it may not add transport-owned authority.
- `ProgramRuntime` uses one persistent typed stack, dictionary, transactional working snapshot,
  producer/task state, and bounded reducible revision archive for both languages.
- VM state inspection exposes typed values, stack shape, definitions, capability availability, and
  revisions to providers and UI.
- Provider text is executed as Lisp/Co-Forth wire source; structured diagnostics support one bounded
  source-only repair and program/output history remains visible.
- VM-local mutation rolls back on failure, cancellation, stale revision, or denied approval;
  external effects are separately journaled.
- Reducible named-Brain checkpoints restore after daemon restart without replaying effects; two
  attached consoles share committed definitions through one serialized turn lane.
- The complete current `cargo test --all-targets --no-fail-fast` target set passed, and a rebuilt
  configured-Grok smoke produced and executed raw Lisp under the persistent wire contract.
- Independent executions do not require a process-wide GIL.

This opens convergence; it does not declare the language roadmap complete. Unchecked VM items in
`TODO.md` remain required work and may block the particular Brain phase that depends on them—for
example durable output replay blocks reconnect completion, and approval lifecycle gaps block remote
authority handoff. Advanced CTFE, concepts, mixed syntax, generalized coroutines, legacy-library
migration, Cranelift optimization, AOT, and self-hosting are not prerequisites for attaching real
clients and providers to the existing shared runtime.

## Existing implementations to converge

### `SharedBrainStore`

`src/brain/shared.rs` is the strongest current persistence foundation. It owns named Brain event
logs, environment binding, revisions, program-stack projection, subscriptions, and JSONL recovery.

### Removed legacy daemon registry

The ephemeral `BrainRegistry`, its `daemon_brain` task loop, global shared-context map, HTTP routes,
Cap'n Proto operations, and frontend polling lifecycle were removed on 2026-08-24. They represented
background tasks as a second kind of Brain and silently mixed speculative summaries across sessions.
Future autonomous work enters the authoritative named Brain as a `BrainRun`; the old protocol is not
a compatibility boundary.

### Removed client-local speculative Brain

The former `BrainSession`, its separate provider loop, hidden `brain_context` prompt injection,
typing-time question/action channels, and ambient shell-command helper were removed. Typing now
updates the local vocabulary panel only. If speculative context gathering returns, it must be an
explicit cancellable `BrainRun` in the named Brain log, using the ordinary addressed approval and
environment-runner paths rather than recreating a second client-local authority.

### Transport-specific lifecycle paths

Named-Brain HTTP/WebSocket attachment and local frontend registration currently address the one
durable namespace. Attachments now have daemon-authoritative identities, roles, connection leases,
and monotonically acknowledged event cursors that survive reconnect and daemon restart. Each Brain
frontend also persists only its opaque attachment ID, scoped by durable Brain ID and local console
slot, so restarting that console resumes the daemon-owned cursor instead of minting a new identity.
The daemon remains authoritative and rejects concurrent rebinding of the same attachment. An HTTP
attach first reserves an exact connection for 15 seconds; only its WebSocket activation becomes a
canonical `ClientAttached` event. Abandoned reservations expire without advancing the Brain revision
or cursor, and an old expiry/disconnect cannot affect a replacement connection. Each Brain
also has an exclusive, expiring runner lease bound to its exact environment generation. The
ordinary frontend/daemon Cap'n Proto channel now carries a lease-bound runner callback, correlated
ProgramRun request/result records, and bootstrap revision/checkpoint state. It also exposes the
first versioned `BrainService` capability for snapshots, attachments, acknowledged cursors, detach,
participant submissions, ordered snapshot-first watches, runner-lease management, addressed runner
handoffs, and exact run inspection/cancellation. A handoff is a durable reservation naming the exact
source lease, target runner
subject, and environment generation. A remote `brain:control` participant may request or cancel it,
but only the environment-owning local service may accept it; acceptance atomically installs a new
lease, so the old frontend callback immediately becomes stale. Participant
input is a closed union rather than a forgeable event envelope; the daemon still assigns ordering,
identity, timestamps, results, and run transitions. The home TUI now keeps a cloneable local
capability on the frontend `LocalSet` and uses it for snapshot, persistent attachment, ordered watch,
acknowledgement, submission, lease renewal/release, and detach. Foreign remote attachments retain
the scoped-auth adapter behind the same client projection. HTTP performs credential and attachment
bootstrap; the resulting WebSocket is a correlated bidirectional Cap'n Proto session carrying
snapshot/event projections plus submit, acknowledge, and detach commands. Commands inherit the
exact socket attachment rather than accepting client-authored attachment IDs, and the daemon
revalidates the signed credential for each mutation and periodically while the connection is idle.

The daemon's transport-neutral submission operation never executes ProgramRuns inside the daemon.
It serializes the accepted event, requires the active environment lease's registered callback, and
dispatches the source to the frontend-owned typed `ProgramRuntime`. The runner returns its exact
output/revision/checkpoint; the daemon independently reverifies and content-addresses reducible
state beside the event log without inheriting frontend authority or replaying effects. A restarted
runner receives the durable checkpoint during callback registration before it accepts work.
Concurrent commits journal their exact runtime revision and recovery never regresses to a
later-appended older checkpoint.
Every accepted prompt or typed program now also creates one canonical, versioned `BrainRun` with a
stable `RunId`, initiating attachment, request-event cursor, kind, timestamps, detail, and explicit
`queued_for_environment`, `running`, `awaiting_approval`, `completed`, `failed`, `cancelled`, or
`interrupted` state. A request accepted without a usable lease-bound callback remains durably queued
and produces no invented result. Registering the matching runner callback drains queued runs in
event order under the Brain turn lane. Runner errors persist a correlated error result before the
run becomes failed, and the reverse approval capability projects suspension and resumption onto the
same run. Reconstruction after daemon restart leaves never-started queued work eligible for the next
valid lease but marks previously running or approval-suspended work interrupted; it never implicitly
replays work with unknown external progress.
Runs may name a parent only while that parent exists in the same Brain and remains nonterminal; the
event log preserves and reconstructs that ancestry. Cancellation is attachment-scoped: only the
connected driver that initiated a run may request it. Queued and interrupted runs cancel directly;
running and approval-suspended runs first require acknowledgement from the exact live runner lease.
Runner requests carry `RunId`, and the callback bridge services control calls concurrently so
`cancelRun` can overtake the execution RPC it stops. Local Cap'n Proto and authenticated remote
binary clients share the same cancellation contract. Live daemon fixtures prove both queued-run
cancellation and a running ProgramRun blocked in its frontend callback, including final durable
inspection as `cancelled`.
Each named Brain also owns one daemon turn lane, so concurrent attached consoles cannot interleave
input acceptance, VM commit, checkpoint publication, and Result events. Its WebSocket subscription
is snapshot-first without a snapshot/subscribe gap. A 2026-08-24 two-console smoke test shared a
definition live, restored it after daemon restart, and executed a configured-Grok Co-Forth response
against the restored dictionary. A second live provider test rejected a three-argument Lisp
`string-append`, journaled that source and exact type diagnostic, requested one source-only repair,
then committed the corrected program; another daemon restart restored the earlier definition and
continued the runtime revision. Static wire repair never retries host effects, approvals,
cancellation, or runtime-limit failures.
The transport-neutral participant-submission operation is now shared by local Cap'n Proto and the
authenticated remote binary session; the former JSON HTTP adapter has been removed. The local RPC
exposes the complete first lifecycle surface. Remote command correlation is independent of event
projection, so long provider/runner
requests do not stop that socket from receiving canonical events. JSON submit, acknowledge, detach,
and runner-lease routes are gone; HTTP remains for authenticated discovery, credential/attachment
bootstrap, and explicit administrative archive. Run budgets and generalized effect-resume
correlation still need the unified service. Runtime ownership has moved to the leased
environment runner; the daemon is now the durable coordinator for interactive prompt/program runs.
The obsolete client-local speculative agent and hidden context-injection path are absent, so they
can no longer bypass this coordinator or consume a separate provider session while the user types.

On 2026-08-24 a live attachment test used separate driver and consultant consoles against one
Brain. The driver defined and invoked a shared Lisp word, the consultant was forbidden from
submitting a program, acknowledged cursors survived reconnect, stale connection acknowledgements
returned a conflict, and WebSocket close produced a durable detach event. A short runner lease was
also observed expiring into a durable release event. This validates the compatibility transport and
lease state machine. Automated callback/dispatch tests now cover environment-runner delegation;
another live two-console smoke test remains required for the new path.

## Target model

```text
BrainAggregate (daemon-authoritative)
  BrainId                  stable UUID
  aliases                  optional unique names
  environment              machine/workspace/generation
  event log                sole durable authority
  VM revisions             typed checkpoints/deltas
  memory namespace         references, not copied ambient context
  grants/policies          policy metadata only; no usable host handles
  runs                     active and historical BrainRun records
  attachments              authenticated client cursors
  runner lease             current environment runner identity and liveness

BrainRun
  RunId
  kind                     interactive | speculative | scheduled | subagent | maintenance
  parent/ancestry
  provider/model selection
  starting-context references
  typed program/result stream
  status, budget, cancellation
  pending approvals/questions/plans
  input cursor              last consumed durable inbox/prompt event

BrainAttachment (client-side)
  BrainId + attachment identity
  role                     runner | driver | consultant | observer
  locally cached last-applied event cursor + revision
  shadow-buffer projection
  local input/draft state
  reconnect/resync state
```

### Attachment roles and visible console state

Roles describe what an attached console may ask the authoritative service to
do; they are not a second source of workspace authority.  The UI must render
the active role in the status bar and on every permission/proposal prompt so a
person can tell whether their input will execute now, queue for the runner, or
merely become context.

- **runner** — exactly one leased environment-owning console. It holds the
  workspace, accessibility, credential, and typed host-effect bindings. This
  is the only role that may execute a `BrainRun` with environment effects.
- **driver** — an interactive collaborator. A driver can append prompts,
  programs, drafts, and replies to the ordered Brain log, but has no direct
  workspace authority. When the runner is available its requests become
  queued runs; otherwise they are visibly `queued_for_environment`.
- **consultant** — can read the projected history and contribute bounded
  context, review comments, or suggested programs. It cannot start an
  executable run or approve a capability by default.
- **observer** — read-only projection access.

Approval and control are separate scoped permissions, not substitute roles:
an authorized driver may decide an approval, while a `brain:control` holder may
request a runner-lease handoff. Neither action silently transfers workspace
handles. `brain:attach` independently permits a participant to create and close
its own projection; it does not imply approval or runner control. Default
consultants do not receive `brain:approve`, default observers receive only read
and attach authority, and elevated scopes require an explicit bootstrap grant
within the role's ceiling. A `brain:control` holder can delegate only a subset of
its live authority and remaining lifetime; bounded signed ancestry makes
revoking any ancestor revoke every descendant. A compact status form should make the live condition obvious, for
example `brain: compiler-work · driver · runner online` or
`brain: compiler-work · consultant · read-only`.

The daemon persists the authoritative acknowledgement cursor for every attachment identity. A
frontend may cache its last applied event for fast reconnect, but it resumes by asking the daemon for
the acknowledged cursor, receiving a snapshot or the missing event range, and acknowledging forward
only after its projection applies each event. Retention/pruning policy uses those durable cursors and
explicit expiry rules; terminal exit never silently advances or discards an attachment's position.

The master frontend owns the environment authority, while the daemon owns the authoritative history.
The active runner obtains a leased service attachment before executing a run; it is replaceable after
failure only on the same authorized environment, and cannot append events or commit VM state without
that lease. In a standalone build, an embedded in-process daemon implements the same service interface;
only the deployment topology changes.

## Canonical events

Extend the existing numbered Brain event envelope rather than maintaining parallel text logs:

- `BrainCreated`, `AliasChanged`, `EnvironmentChanged`;
- `ClientAttached`, `ClientDetached`;
- `DraftStarted`, `DraftUpdated`, `DraftCancelled`;
- `RunStarted`, `RunStatusChanged`, `RunCancelled`, `RunCompleted`;
- `PromptCommitted`, `AssistantDelta`, `AssistantCommitted`;
- `ProgramSubmitted`, `ProgramVerified`, `ProgramCommitted`;
- `CapabilityRequested`, `CapabilityDecided`, `EffectRecorded`;
- `QuestionAsked`, `QuestionAnswered`, `PlanPresented`, `PlanResponded`;
- `MemoryReferenced`, `ScheduleChanged`, `ChildLinked`;
- `ScheduleDue { schedule_id, due_at, missed_count, first_missed_at, delivery_policy }`;
- `ProjectionMoved`, `EventSuperseded`.

Events store typed IDs, structured diagnostics, capability objects, and revision links. Display text
is a projection, never the authority. Runtime-only responders such as oneshot senders are indexed by
`RunId`/request ID and reconstructed as unavailable after restart; they are not serialized.

## Service boundary

Define one async `BrainService` used by every deployment:

```text
create / resolve / list / snapshot
attach / subscribe / resync / detach
start_run / cancel_run / inspect_run
commit_prompt / submit_program
answer_question / respond_to_plan / decide_capability
move_projection / inspect_vm
push_context
```

Implement adapters for in-process calls, Cap'n Proto over local IPC, the same Cap'n Proto schema in
binary WebSocket frames, and future authenticated peer RPC. HTTP may remain a discovery/bootstrap
surface. Handlers perform framing, authentication, and presentation only; lifecycle logic stays in
the service.

### Frontend/daemon contract

The local Finch frontend and daemon are separate processes, so their normal control and event path
is a versioned Cap'n Proto contract over the Unix socket. Remote frontends carry messages from that
same schema in ordered binary WebSocket frames. This is the binary representation of `BrainService`,
not merely a faster encoding of a few legacy RPC methods. In particular, the schema must carry
structured `BrainEvent` records, attachment cursors, role/lease state,
`ProgramRun` snapshots/outcomes, and correlated typed VM records equivalent to
`VmEffectEnvelope { execution_id, sequence, kind, arguments, origin }` and
`VmResume { execution_id, sequence, response }`.

Do not make Cap'n Proto the VM's internal value representation or require every embedder to use it.
The runtime stays transport-neutral. Ordinary word-aligned Cap'n Proto encoding is the default for
zero-copy-friendly event access; packed encoding is optional where measured bandwidth savings
justify unpacking. Large blobs remain content-addressed or separately streamed rather than embedded
in Brain events. The current JSON HTTP/WebSocket lifecycle payloads and legacy `AnyPointer`/JSON IPC
path are migration compatibility surfaces; B4 replaces them with explicit versioned schema fields
and cross-transport conformance fixtures.

Every mutating request includes `BrainId`, caller identity, expected Brain revision, environment
generation, and an idempotency key. Names are aliases resolved to IDs, not durable identity.

Workspace state is outside the event log and may change independently. A run/effect therefore records
the environment generation plus any meaningful precondition available at the boundary (for example
Git HEAD, a file identity/preimage hash, or a resource generation). A mismatch is a typed stale or
conflict outcome for the master frontend to reconcile; Finch never claims that replaying a Brain log
can reconstruct arbitrary external workspace mutation.

## Forks and process topology

Logical ownership must not depend on whether Finch currently runs client and daemon in one process,
separate processes, or a forked development topology:

- only the authoritative service appends Brain events and commits VM revisions;
- forked children never continue using inherited client routing state as identity;
- a child receives explicit `BrainId`, `RunId`, ancestry, provider/model request, context references,
  budget, and attenuated grant reference;
- after process creation it establishes an addressed service attachment and obtains its own channel;
- results are appended to the parent Brain and linked to the child `RunId`;
- disconnect and duplicate delivery are handled through event cursors and idempotency keys.

This same contract applies to Tokio tasks. Process isolation is an implementation choice, not a
different agent protocol.

## Context push and remote Brains

`push_context` appends a bounded, immutable inbox event containing sender identity, provenance,
content classification, context or memory references, target Brain ID, reason, and deduplication
key. It does not directly mutate the target VM stack, graft memory, transfer a grant, install code,
or schedule work. Receiver-local policy decides whether to surface, ingest, or run it.

mDNS is advisory discovery only. TXT records now use an explicit metadata allowlist and contain no
reusable peer credential; discovery clients likewise cannot import authority from hostile legacy
`token` properties. Remote attachment requires an authenticated encrypted channel, cryptographic
peer identity, replay protection, Brain-level ACLs, and auditable invitation/revocation. A `.local`
name is not identity.

## UI projection

The shadow-buffer UI renders one Brain with nested runs:

```text
brain: compiler-work
  interactive run                         active
  speculative context run                 completed
  child: verifier tests [grok]             running
  scheduled: nightly conformance           waiting
```

Selecting a run changes the projection, not the authoritative event head. Live output, questions,
plans, capability dialogs, cancellation, errors, and child results are all derived from the same
event stream. Reattachment begins with a snapshot and continues from a numbered cursor; gaps trigger
resync.

## Migration phases

### B0: Freeze behavior and fixtures

- Inventory all Brain endpoints, IPC methods, commands, events, persistence paths, and UI consumers.
- Add cross-transport contract tests for current create/list/get/respond/cancel behavior.
- Capture restart, reconnect, cancellation-race, and named-Brain fixtures.

Exit: existing behavior is measurable before consolidation.

### B1: Canonical identity and event schema

- Stable `BrainId`, `RunId`, attachment ID, connection ID, runner-lease ID, request-event cursor, and
  revision types are present. Extend the same identities through the remaining schedule, subagent,
  speculative, cancellation, and final-summary lifecycle.
- Version the expanded event envelope and projection rules.
- Add migrations for existing named JSONL logs and ephemeral summaries.

Exit: every current state transition has one canonical event representation.

### B2: Unified authoritative store

- Evolve `SharedBrainStore` into `BrainStore`/`BrainAggregate` storage.
- Add daemon-coordinated run state and final-summary events directly to the aggregate; do not revive
  the removed parallel registry.
- Persist typed VM checkpoint/delta references alongside committed programs.
- Event-source the ordinary home-console prompt, provider, tool, program, and result lifecycle;
  `ConversationHistory` and MemTree become projections/indexes rather than an unshareable parallel
  history. Do not synthesize missing historical Brain events from lossy semantic summaries.

Exit: one store reconstructs Brain and run projections after restart.

### B3: Unified run supervisor

- Replace client `BrainSession` ownership with `BrainRun` records plus a daemon-side coordinator and
  environment-runner supervisor.
- Model speculative typing as a cancellable run in the currently attached Brain.
- Route questions, plans, approvals, model selection, subagents, and scheduled callbacks by IDs.

Exit: every background activity has identical lifecycle and ancestry semantics.

### B4: One service, multiple transports

- Implement `BrainService` once.
- Version the Cap'n Proto frontend/daemon schema for typed Brain events, attachment cursors, run
  outcomes, and VM effect/resume correlation; eliminate its JSON-shaped lifecycle payloads.
- Use that schema over local Cap'n Proto RPC and ordered binary WebSocket frames; keep HTTP only for
  authenticated discovery/bootstrap operations that do not duplicate Brain lifecycle semantics.
- Convert daemon client, remote client, and embedded mode into adapters over the same service.
- Keep the removed legacy route/IPC spawn orchestration absent while adding only the canonical
  event/cursor/run service.

Exit: the transport conformance suite produces equivalent events and outcomes.

Current compatibility status: HTTP performs authenticated remote discovery, credential issuance,
and attachment bootstrap. Local Cap'n Proto has a typed `BrainService` for the complete event
envelope, participant submission/outcome, attachment cursor, ordered watch, runner lease, and
environment-authorized handoff acceptance,
alongside the lease-bound runner callback and checkpoint bootstrap. The local TUI consumes that
capability entirely on its `LocalSet`; a live ignored test verifies snapshot-first watch, queued run
submission, cursor acknowledgement, and detach against a restarted daemon. Remote consoles carry
the same closed submission union, typed outcomes, and scoped handoff request/cancel operations in
correlated binary WebSocket envelopes;
fixture and live-daemon tests cover attach, watch, submit, acknowledge, final detach projection, and
cleanup, including detach before an explicit watch. An additional live-daemon fixture requests an
addressed handoff with scoped remote control authority, accepts it through local Cap'n Proto,
revokes that controller before its next command, replaces the registered callback, and proves a
fresh ordinary driver sends the next ProgramRun only to the target runner.
Frontend acceptance also verifies the local Unix-socket host, normalized hostname, and canonical
workspace against the Brain environment. Generalized approval/effect resumptions remain incomplete,
as does replacement of the remaining JSON-encoded detail/context/checkpoint values with explicit
schema types. A cloneable in-process `BrainLifecycleService` now owns attachment
reservation and expiry, watch activation, acknowledgement, detach cleanup, participant submission,
queued-run resumption, and runner-lease lifetime. Embedded hosts call this boundary directly; local
RPC and remote WebSocket code are encoding/authentication adapters over it. Hermetic service tests
exercise attachment, watch, queueing, projection, and cleanup, while the ignored live conformance
fixture drives local RPC and remote binary clients through the same
attach/watch/prompt/queued-run/acknowledge/detach script and compares normalized events and outcomes.
Ordinary HTTP and WebSocket lifecycle operations now require scoped credentials on loopback as well
as remote addresses. Attachment lifecycle has its own `brain:attach` scope, and archival obtains an
explicit one-purpose `environment:admin` credential instead of relying on localhost trust.
The rebuilt-daemon remote lifecycle smoke test and local/remote conformance fixture both pass with
that stricter boundary, including explicit archive cleanup.

### B5: Client projections and shadow-buffer UI

- Replace local authoritative `brain_context` writes with attachment events/projection state.
- Render Brain/run hierarchy, reconnect status, pending interactions, and structured VM outcomes.
- Preserve responsive speculative typing and cancellation behavior.

Exit: local, daemon, and remote attachment render the same event history.

### B6: Remote security and discovery

- Keep the completed removal of advertised peer tokens covered by an authority-free allowlist test,
  and replace the remaining plaintext remote Brain authentication.
- Add cryptographic node identity, encrypted authenticated channels, invitations/ACLs, and revocation.
- Advertise only stable discovery metadata; retrieve live capabilities after authentication.

Exit: hostile-LAN, replay, confused-deputy, and cross-Brain authorization tests pass.

### B7: Remove duplicate abstractions

- Remove obsolete registries, shared string-context endpoints, transport-owned lifecycle code, and
  compatibility projections.
- Retain explicit versioned import for historical Brain logs.

Exit: `Brain`, `BrainRun`, `BrainAttachment`, `BrainStore`, and `BrainService` have one meaning each.

## Test matrix

- embedded, client/daemon, remote, reconnecting, and restarted deployments;
- concurrent attachments and optimistic-revision conflicts;
- speculative-run cancellation races and stale-result suppression;
- question, plan, and capability suspension across reconnect/restart;
- parent/child ancestry, model selection, result routing, cancellation, and attenuation;
- event cursor gaps, duplicates, idempotent retry, corrupt log, and migration recovery;
- VM checkpoint/rewind without repeating external effects;
- unauthorized context push, alias collision, replay, revoked peer, and hostile mDNS metadata;
- shadow-buffer snapshots for multiple live child/scheduled runs;
- no inherited-socket ambiguity in process-isolated child tests.

## Definition of done

- A Brain has one stable identity, event log, VM history, environment, and authority boundary.
- Daemon, embedded, local client, and remote client use the same service semantics.
- Background helpers, ordinary turns, schedules, and subagents are runs inside a Brain.
- Clients hold resumable projections rather than competing authoritative state.
- Every state change is revisioned, attributable, replay-safe, and visible through one event model.
- Existing Brain data migrates without silent loss or reinterpretation.
- Duplicate registries and shared-context stores are gone.
- Remote discovery conveys no reusable secret and remote mutation is authenticated, encrypted,
  capability-checked, and audited.
