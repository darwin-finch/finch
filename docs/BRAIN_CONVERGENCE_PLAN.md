# Brain Runtime Convergence Plan

Status: deferred until the typed Lisp/Co-Forth VM integration gate is complete.

## Goal

Make `Brain` one daemon-authoritative logical entity. The daemon owns only durable coordination
state and the event log; it has **no** workspace/environment authority. By default on Unix, one
named master frontend runner lives in a supervised `tmux` session and exclusively owns the active
VM, provider/task execution, workspace, accessibility, credentials, and host-effect capabilities.
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

`tmux` provides terminal-disconnect survival, not crash or reboot durability. The runner therefore
checkpoints serializable VM state and execute-once host-effect records at committed boundaries. If
the runner or `tmux` server dies, the daemon marks the run interrupted and a newly launched runner
may resume only from a validated checkpoint; it never replays already-recorded external effects.
`tmux` is a preferred Unix launcher, not a semantic dependency: headless/service runners remain a
supported deployment option.

This work must consolidate the existing implementations. It must not create another registry,
session abstraction, or transport-specific lifecycle.

## Hard prerequisite: finish the shared VM first

No Brain convergence phase beyond inventory and tests begins until all of these are true:

- Co-Forth and Finch Lisp both compile source directly to the common typed stack IR.
- Co-Forth has parsed definitions/signatures, conditionals, loops, locals, quotations, closures,
  managed typed values, and versioned vocabulary publication.
- Finch Lisp has definitions, lexical bindings, conditionals, metered loops, closures, typed
  collections, bounded hygienic macros, error/result forms, and direct shared-vocabulary calls.
- Every production primitive is generated from one typed signature/effect/implementation registry.
- Capabilities compose transitively, including bounded argument-dependent selectors.
- The broker implements availability, grants, attenuation, revocation, audit, approval, and typed
  suspend/resume for files, tools, processes, network, automation, MemTree, scheduling, agents, and
  response emission.
- `ProgramRuntime` uses one persistent typed stack, dictionary, heap, transaction manager, and
  revision history for both languages.
- VM state inspection exposes typed values, stack shape, definitions, capability availability, and
  revisions to providers and UI.
- Structured diagnostics and authorization outcomes reach the shadow-buffer UI without string
  scraping.
- VM-local mutation rolls back on failure, cancellation, stale revision, or denied approval;
  external effects are separately journaled.
- Agent spawning, joining, cancellation, model selection, starting context, MemTree, automation,
  and scheduled callbacks run through typed VM primitives without shelling out.
- The Lisp-to-Forth text path, native Lisp fallback, source-text effect inference, and duplicate
  legacy model-tool paths are removed or isolated behind an explicit versioned legacy boundary.
- Parser, verifier, interpreter, broker, host-binding, concurrency, provider-conformance, and UI
  suites pass, including adversarial selector and rollback tests.
- Independent executions do not require a process-wide GIL.

The Cranelift tier is not a prerequisite. Its ABI hooks, source maps, and interpreter differential
fixtures should exist, but JIT optimization remains a later performance project.

## Existing implementations to converge

### `SharedBrainStore`

`src/brain/shared.rs` is the strongest current persistence foundation. It owns named Brain event
logs, environment binding, revisions, program-stack projection, subscriptions, and JSONL recovery.

### `BrainRegistry`

`src/server/brain_registry.rs` owns ephemeral daemon runs, names, status, pending question/plan
oneshots, text logs, and final summaries. It also contains a second legacy shared-context map that
overlaps `SharedBrainStore`.

### `BrainSession`

`src/brain/mod.rs` is a client-side speculative typing worker. It owns cancellation, writes a local
`brain_context`, and separately posts a summary to a daemon HTTP endpoint. This is a Brain run and
client projection, not another authoritative Brain.

### Transport-specific lifecycle paths

HTTP handlers, the Cap'n Proto IPC server, daemon clients, remote HTTP/WebSocket attachment, and
in-process callers currently duplicate parts of spawn/list/get/respond/cancel behavior. They should
be adapters over one service.

The named-Brain HTTP compatibility handler no longer replays the accumulated program stack through
the old Co-Forth or native Lisp evaluators. Program events and raw provider-wire responses enter one
typed `ProgramRuntime`; successful reducible revisions are stored as content-addressed checkpoints
beside the event log and restored without replaying source or effects. Concurrent commits journal
their exact runtime revision and recovery never regresses to a later-appended older checkpoint.
Each named Brain also owns one daemon turn lane, so concurrent attached consoles cannot interleave
input acceptance, VM commit, checkpoint publication, and Result events. Its WebSocket subscription
is snapshot-first without a snapshot/subscribe gap. A 2026-08-24 two-console smoke test shared a
definition live, restored it after daemon restart, and executed a configured-Grok Co-Forth response
against the restored dictionary. A second live provider test rejected a three-argument Lisp
`string-append`, journaled that source and exact type diagnostic, requested one source-only repair,
then committed the corrected program; another daemon restart restored the earlier definition and
continued the runtime revision. Static wire repair never retries host effects, approvals,
cancellation, or runtime-limit failures.
This remains a compatibility adapter, not the final runner-lease architecture: it has no approval
audience and therefore cannot acquire workspace/host grants. B2-B4 must move its runtime ownership
to the leased environment runner and retain the daemon as durable coordinator only.

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
  runner lease             current tmux/headless runner identity and liveness

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
handles. A compact status form should make the live condition obvious, for
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

Implement adapters for in-process calls, Cap'n Proto IPC, HTTP/WebSocket, and future authenticated
peer RPC. Handlers perform decoding, authentication, and presentation only; lifecycle logic stays in
the service.

### Local frontend/daemon contract

The local Finch frontend and daemon are separate processes, so their normal control and event path
is a versioned Cap'n Proto contract over the Unix socket.  It is the binary local representation of
`BrainService`, not merely a faster encoding of a few legacy RPC methods.  In particular, the
schema must carry structured `BrainEvent` records, attachment cursors, role/lease state,
`ProgramRun` snapshots/outcomes, and correlated typed VM records equivalent to
`VmEffectEnvelope { execution_id, sequence, kind, arguments, origin }` and
`VmResume { execution_id, sequence, response }`.

Do not make Cap'n Proto the VM's internal value representation or require every embedder to use it.
The runtime stays transport-neutral and HTTP/WebSocket remain suitable remote adapters.  But the
local CLI/daemon boundary must not fall back to free-form JSON event payloads for state that the
daemon has to validate, replay, or resume.  The existing legacy `AnyPointer`/JSON event path is a
migration compatibility surface; B4 replaces it with explicit versioned schema fields and
cross-transport conformance fixtures.

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

mDNS is advisory discovery only. Remove secrets from TXT records. Remote attachment requires an
authenticated encrypted channel, cryptographic peer identity, replay protection, Brain-level ACLs,
and auditable invitation/revocation. A `.local` name is not identity.

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

- Add stable `BrainId`, `RunId`, attachment ID, request ID, and revision types.
- Version the expanded event envelope and projection rules.
- Add migrations for existing named JSONL logs and ephemeral summaries.

Exit: every current state transition has one canonical event representation.

### B2: Unified authoritative store

- Evolve `SharedBrainStore` into `BrainStore`/`BrainAggregate` storage.
- Move daemon run state and final summaries out of parallel registry fields.
- Delete `BrainRegistry`'s legacy shared-context map after migrating its callers.
- Persist typed VM checkpoint/delta references alongside committed programs.

Exit: one store reconstructs Brain and run projections after restart.

### B3: Unified run supervisor

- Replace daemon `BrainEntry` tasks and client `BrainSession` ownership with `BrainRun` records plus a
  daemon-side supervisor.
- Model speculative typing as a cancellable run in the currently attached Brain.
- Route questions, plans, approvals, model selection, subagents, and scheduled callbacks by IDs.

Exit: every background activity has identical lifecycle and ancestry semantics.

### B4: One service, multiple transports

- Implement `BrainService` once.
- Version the Cap'n Proto frontend/daemon schema for typed Brain events, attachment cursors, run
  outcomes, and VM effect/resume correlation; eliminate its JSON-shaped lifecycle payloads.
- Convert HTTP, WebSocket, Cap'n Proto, daemon client, remote client, and embedded mode into adapters.
- Remove duplicated route/IPC spawn orchestration.

Exit: the transport conformance suite produces equivalent events and outcomes.

### B5: Client projections and shadow-buffer UI

- Replace local authoritative `brain_context` writes with attachment events/projection state.
- Render Brain/run hierarchy, reconnect status, pending interactions, and structured VM outcomes.
- Preserve responsive speculative typing and cancellation behavior.

Exit: local, daemon, and remote attachment render the same event history.

### B6: Remote security and discovery

- Replace advertised peer tokens and plaintext remote Brain authentication.
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
