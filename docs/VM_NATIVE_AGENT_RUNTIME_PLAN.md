# VM-Native Agent Runtime Plan

## Outcome

Make Forth and Lisp the efficient, provider-neutral action interface for Finch. Grok, Claude,
OpenAI/Codex-class models, local models, and subagents should receive the same small runtime
contract and be able to inspect the workspace, edit files, run programs, and coordinate child
agents through the VM.

Normal work should not require a shell command. Process execution remains available for work
that inherently invokes an external program, such as a compiler, test runner, Git, or a user
command. Reading, searching, patching, persistence, HTTP, dialogs, and agent coordination use
native Rust capabilities.

This plan is the execution-focused companion to `SHARED_PROGRAM_RUNTIME_PLAN.md`. It assumes
the shared program identity, manifest, tagged values, and effect model described there.

## Product invariants

1. **One provider-neutral contract.** Provider adapters only translate tool-call transport.
   They do not decide which VM or agent features a model receives.
2. **One execution path.** Top-level agents, subagents, peers, and VM programs all enter the
   same capability broker, permission policy, audit log, and resource limiter.
3. **Programs are first-class.** A model submits Forth or Lisp and receives structured values,
   emitted output, effects, diagnostics, and the resulting VM revision.
4. **Shell is an explicit capability.** No built-in operation secretly constructs a shell
   command. `ExecuteProcess` uses an argv array and a controlled environment; a shell is used
   only when the program explicitly requests shell syntax.
5. **Fork means concurrency.** VM snapshotting/speculative evaluation and spawning an agent are
   different operations with different names and result types.
6. **No provider privilege gaps.** If Grok is selected, Grok receives the same runtime manifest
   and program tools as any other capable provider. Feature differences are reported from
   negotiated provider capabilities, not provider-name checks.
7. **Bounded execution.** Every program and child agent has fuel, time, memory/output, nesting,
   concurrency, and cancellation limits.
8. **Optional capabilities are discoverable.** Automation and other opt-in facilities appear in
   the runtime manifest only when enabled, supported on the host, and successfully initialized.
   Programs receive a typed `CapabilityUnavailable` result when availability changes.

## Target model-facing API

Expose a compact set of native function tools through every provider adapter:

```text
submit_program(language?, source, intent, declared_capabilities?, manifest_generation?)
invoke_program(program_ref, arguments, expected_effects?, manifest_generation?)
search_vocabulary(query, language?, capability?, limit?)
inspect_program(program_ref, include_source?, include_tests?)
get_vm_state(detail?)
cancel_execution(execution_id)
```

`submit_program` is the normal entry point. It compiles or reads source, derives effects,
checks the manifest generation, obtains any required approval, and executes against a defined
VM revision. Its result is structured rather than formatted terminal text:

```text
execution_id
status: completed | suspended | failed | cancelled
values: [ProgramValue]
stdout/stderr/events
diagnostics with source locations
input_revision and output_revision
inferred_capabilities, currently required capabilities, and effect outcomes
spawned task handles
resource usage
```

Keep legacy direct tools during migration, but implement them as thin calls into the same
capability broker. Once compatibility and quality gates pass, omit them from the model-facing
schema while retaining user-facing commands where useful.

## Native capability ABI

Define a language-neutral interface used by both evaluators:

```text
invoke(program, arguments, execution_context) -> ExecutionOutcome
request(capability, typed_arguments, execution_context) -> value | suspension | error
```

`ExecutionContext` carries identity and authority, not global ambient access:

- session, turn, execution, parent, and provider/model IDs;
- workspace root and active VM revision;
- permission role and approved capability grants;
- registry and environment generations;
- cancellation token and resource budget;
- event/audit sink;
- task scheduler handle.

Start with these typed native capabilities:

| Area | Native operations | Shell policy |
|---|---|---|
| Files | read, stat, list/glob, write changeset, create directory | Never |
| Search | text/regex search with paths, limits, and binary policy | Never |
| Editing | replace ranges, apply structured patch, compare hashes | Never |
| Programs | define, inspect, search, invoke, test, publish | Never |
| Session | emit text, dialog, plan, memory, VM state | Never |
| Network | HTTP request through URL and credential policy | Never |
| Agents | spawn, poll/await, cancel, send message, collect result | Never |
| Automation | inspect UI, focus, click, type, key, scroll; scheduled program management | Never by default |
| Processes | executable plus argv, cwd, env delta, timeout | Only for external programs |
| Shell | explicit script string | Only when explicitly requested and approved |

Each primitive declares an `ExecutionEffect`. Forth definitions derive the join of their words;
Lisp functions derive the join of called primitives, with dynamic or unresolved calls marked
`Unclassified`. Workspace writes first produce a hash-bound `ChangeSet`; applying it is a
separate effect. This preserves reviewable edits without forcing models to use shell redirection.

## Forth and Lisp surface

Both languages should expose equivalent operations, even when their syntax differs.

Illustrative Forth vocabulary:

```forth
s" src/**/*.rs" files.glob
s" TODO" s" src" search.text
s" src/lib.rs" file.read
patch.new ... patch.apply
s" cargo" [ s" test" s" --lib" ] process.run
['] child-work agent.spawn agent.await
```

Illustrative Lisp vocabulary:

```lisp
(files/glob "src/**/*.rs")
(search/text "TODO" :path "src")
(file/read "src/lib.rs")
(patch/apply (patch/new ...))
(process/run "cargo" ["test" "--lib"])
(agent/await (agent/spawn child-work))
```

The exact names may follow existing vocabulary conventions, but the operations, values,
effects, errors, and permission behavior must be equivalent. Do not implement the Lisp layer
by generating Forth text, or the Forth layer by generating Lisp text; both bind directly to
the shared Rust capability registry.

## Optional automation capabilities

The VM must be able to use Finch automation directly when the feature is enabled. Automation is
not injected as a permanent set of words with ambient authority. It is a broker capability
advertised by the runtime manifest and bound into both Forth and Lisp through the same typed ABI.

### Availability and discovery

Represent availability independently from permission:

```text
disabled                 user configuration is off
unsupported              host/platform has no implementation
permission_required      OS consent, such as Accessibility, is missing
available                initialized and callable
degraded(reason)         only a documented subset is available
```

The manifest includes the automation backend, version, supported operations, coordinate/display
model, and availability state. Enabling or disabling the feature, granting or revoking OS consent,
changing displays, or reconnecting to a daemon increments the environment generation and refreshes
the manifest. A program compiled when automation was available must still recheck availability and
authority at execution time.

Top-level agents do not need separate direct `gui_*` tool schemas once program submission is the
default. They discover `automation.*` in the manifest and invoke it from Forth or Lisp. Legacy
`gui_click`, `gui_type`, and `gui_inspect` remain thin broker adapters during migration.

### Native desktop automation

Expose typed operations such as:

```text
automation.displays()
automation.windows(application?)
automation.focused()
automation.find(selector)
automation.focus(element)
automation.click(element | point, button?, count?)
automation.type(text, target?, delay?)
automation.key(key, modifiers?)
automation.scroll(target | point, delta)
```

Inspection returns structured applications, windows, and accessibility elements rather than a
formatted report. Element references include the owning process, role, label, bounds, and a short
generation-bound handle. Mutating operations prefer an inspected element handle; coordinate clicks
remain an explicit fallback and include the display ID and coordinate space.

On macOS, use the Accessibility APIs for application/window/element inspection and actions, and
CoreGraphics for input events that truly require synthesized input. The current AppleScript/
`osascript` inspection path should not be the VM implementation. It may remain a named compatibility
backend temporarily, reported as `degraded`, and any use must appear as an explicit external-process
effect. No automation primitive silently constructs or launches a shell command.

Classify inspection as a sensitive external read and input/focus/click operations as external
writes. Every mutating request records the target application, element metadata or coordinates,
and originating agent ancestry. Typed text is redacted from routine logs; audit records retain a
hash and length unless policy explicitly allows content retention.

Automation grants are narrow and do not automatically flow to child agents. A parent may delegate
only a subset such as inspect-only access, a specific application, or a bounded operation count.
Background, remote-peer, and scheduled runs default to no desktop mutation even when GUI automation
is globally enabled. They require an explicit policy grant appropriate to an unattended action.

### Scheduled automation

Time-based automation is distinct from desktop control and from the in-memory fork/join scheduler.
When durable scheduling is implemented and enabled, expose:

```text
automation.schedule(program_ref, trigger, arguments, policy_ref)
automation.list-schedules(filter?)
automation.inspect-schedule(schedule_id)
automation.pause/resume/cancel(schedule_id)
automation.run-now(schedule_id)
```

A schedule stores an immutable `ProgramRef`, typed arguments, trigger/time zone, environment
requirements, owner, capability-policy reference, retry/concurrency policy, and context hashes. It
must not persist an arbitrary English task string as executable authority. When fired, it creates a
normal root execution in the runtime scheduler, resolves its provider only if the program requests
an agent, and emits the same task/effect events as an interactive run.

The existing task queue and scheduler are currently non-persistent execution stubs. Do not expose
successful VM scheduling until storage, claiming/leases, missed-run behavior, retries, cancellation,
and actual `ProgramRuntime` execution are implemented. Until then the manifest reports scheduled
automation as unavailable rather than accepting and discarding work.

### Automation UI and emergency control

An active automation operation appears beneath its owning task in the task tree, including target
application, operation type, elapsed time, and approval state. The UI provides a persistent
automation-active indicator and an emergency stop action that cancels the automation subtree and
releases held input state. The inspector shows structured observations and targets without placing
sensitive typed content in scrollback.

Approval dialogs must identify the full agent ancestry and target. A sequence may be approved once
as a hash-bound program/effect plan; changing the target, source, program version, or derived effects
invalidates that approval. The UI must never imply that global feature enablement is equivalent to
approval for every automation action.

## Clarify snapshot, fork, and subagent semantics

Replace the overloaded idea of `fork` with three explicit concepts:

1. `vm.snapshot` / `vm.eval-isolated`: clone VM-local state and evaluate synchronously. This is
   the behavior of the current Co-Forth `fork`; preserve it under an unambiguous name and keep
   `fork` as a compatibility alias temporarily.
2. `task.spawn` / `task.await`: run a pure or capability-bearing VM program concurrently from
   a declared snapshot. The child returns values and events; VM state is merged only through an
   explicit, conflict-checked commit operation.
3. `agent.spawn` / `agent.await`: start a child model loop with its own context and a scoped
   capability grant. The result is a typed task result, not only a string.

Child agents inherit no ambient authority. The parent supplies a subset of its grants and a
budget. All child capability requests go through the shared broker. The scheduler enforces a
configurable depth limit, total child count, parallelism limit, token budget, deadline, and
cancellation propagation.

Register agent spawning through a provider resolver or session model handle, not an
`Arc<dyn LlmProvider>` captured at startup. This prevents a model switch from leaving
`spawn_task` bound to a stale provider. Allow an explicit provider/model override only when
policy permits it.

## Fork, join, and orchestration protocol

Fork and join live in the runtime scheduler, not in a provider adapter and not in either
language evaluator. Forth and Lisp words are thin bindings that submit typed scheduler requests.
The first implementation uses Tokio tasks in the Finch brain process. If stronger isolation later
moves workers into separate OS processes or nodes, the task protocol and VM vocabulary stay the
same.

### Spawn request and child identity

`agent.spawn` accepts an `AgentTaskSpec` rather than an unstructured prompt:

```text
AgentTaskSpec
  task: string
  role: general | explore | research | code | custom
  context: ContextSpec
  provider: optional provider selector
  model: optional model selector
  capabilities: requested subset of parent grants
  budget: turns, tokens, deadline, output bytes, child count
  execution_mode: foreground | background
```

The scheduler validates the request, resolves a model, allocates a task ID, and creates an
`AgentIdentity`:

```text
agent_id and task_id
parent_agent_id and root_agent_id
depth and sibling index
session, turn, and execution IDs
provider and model actually selected
capability-grant ID
workspace/environment and VM revision IDs
```

This identity is installed in the child's `ExecutionContext` and summarized in its system
context. A child therefore knows it is a child, who its parent is, its depth, its task, its
limits, and how to return a result. It does not infer child status from prompt wording or inherit
credentials and authority through ambient globals.

### Starting context

Context transfer is explicit and bounded. `ContextSpec` may include:

- the task statement and optional parent-authored background;
- the compact boot capsule and current `VmManifest`;
- selected committed conversation event IDs, never an implicit copy of the entire transcript;
- program, file, memory, or artifact references with content hashes;
- a VM snapshot/revision for VM tasks, or registry/environment generations for model agents;
- the child's scoped capability grant and resource budget.

The scheduler materializes this into a deterministic child preamble plus referenced content.
Large artifacts remain addressable and are fetched through native capabilities on demand. The
spawn result records a hash of the effective starting context so the run can be audited or
reproduced. Secrets are passed only as opaque capability grants and are never inserted into the
prompt.

The default is intentionally small: task, parent summary, boot capsule, relevant manifest slice,
workspace identity, and grants. The parent must opt into conversation excerpts or additional
artifacts. This prevents context duplication from erasing the token savings of delegation.

### Model selection

Model selection occurs once, in `ProviderResolver`, when the scheduler accepts the task. The
precedence is:

1. an explicit provider/model requested by the parent and allowed by policy;
2. a configured model for the requested role;
3. the session's active provider/model if it supports the required features;
4. a configured fallback selected by capability, availability, cost, and context limits;
5. a structured `NoEligibleModel` error.

The resolved provider/model is pinned for the child run and recorded in every task event. A retry
may change models only under an explicit retry policy and emits a new attempt event. Children that
spawn children use the same resolver; they do not directly clone their provider object. This makes
Grok eligible for any role its adapter advertises, without Grok-specific scheduler code.

### Execution and communication

Each task owns a bounded mailbox and writes ordered events to the session event log:

```text
TaskStarted
TaskProgress
ChildMessage
CapabilityRequested / CapabilityCompleted
ArtifactProduced
TaskCompleted | TaskFailed | TaskCancelled
```

Parent/child messages use a typed envelope containing message ID, sender, recipient, sequence,
reply-to, payload kind, and optional artifact references. Messages are persisted before delivery.
Backpressure limits mailbox count and bytes; cancellation closes the mailbox after recording the
terminal event.

The child communicates its final information through `AgentTaskResult`:

```text
status and final_message
typed values
produced artifacts and proposed changesets
diagnostics and capability outcomes
descendant task IDs
provider/model and token/resource usage
event-log cursor
```

This avoids flattening a child run into one string. File changes are returned as artifacts or
hash-bound changesets and applied through the broker. Concurrent children cannot silently
overwrite each other; stale preimages produce an explicit conflict for the parent to resolve.

### Join operations

Spawning returns a `TaskHandle` immediately. Both languages bind these scheduler operations:

```text
task.poll(handle)                 non-blocking status and new events
task.await(handle)                suspend until one task reaches a terminal state
task.await-all(handles)           join all, preserving input order
task.select(handles)              return the next completed task
task.cancel(handle, reason)       propagate cancellation to descendants
agent.send(handle, message)       append a parent-to-child mailbox event
```

Awaiting suspends the VM continuation; it must not block the REPL thread or hold a VM mutex while
the model is running. Completion queues a resume event with the typed result. The continuation is
then scheduled against its recorded VM revision. Any attempted state commit is checked for a
revision conflict.

### Master-agent orchestration

The master is not a privileged alternate executor. It is the root agent using the same scheduler
API with a broader user-approved grant. It orchestrates by submitting a Forth or Lisp program that:

1. decomposes work and creates task specs;
2. starts independent children and retains their handles;
3. uses `select`, `poll`, or `await-all` according to dependencies;
4. sends follow-up context or cancellation through mailboxes;
5. validates structured results and resolves changeset conflicts;
6. invokes additional programs or agents when needed;
7. synthesizes the user-visible result and records terminal status.

A dependency-aware orchestration program can place handles in the existing poset so ready tasks
run concurrently and dependent tasks start only after prerequisites succeed. The scheduler owns
execution state; the poset describes dependencies. Neither the master model nor a Forth data stack
is used as the authoritative task queue.

For simple calls, provider-native `spawn_task` remains a compatibility adapter that builds one
`AgentTaskSpec` and awaits its handle. More capable models can express fan-out/fan-in directly in a
submitted program, reducing model round trips while preserving the same permissions and audit
trail.

## UI projection for child agents and tasks

Child agents are logical runtime tasks by default, not OS child processes. The UI should call them
agents or tasks. If one of them invokes an external process, that process appears as an operation
under the owning agent. This keeps provider work, VM concurrency, and operating-system processes
visually distinct.

The scheduler event log is authoritative. The TUI maintains a `TaskTreeProjection` keyed by stable
task IDs and renders that projection into shadow-buffer surfaces. UI widgets never inspect Tokio
join handles, provider objects, or mutable VM stacks. A reconnect or terminal resize rebuilds the
same view by replaying task events.

### Default conversation view

Represent one root agent turn as a `WorkUnit` with a compact, live task tree beneath it:

```text
◆ Building…  18s · Grok 4 · 3 agents
  ├─ ● inspect VM bindings       Grok 4 Mini   reading · 8s
  ├─ ◐ design scheduler          Grok 4        generating · 11s
  └─ ✓ audit permissions         Claude Sonnet 24s · 3 findings
```

Each row shows semantic state, not raw child output:

- hierarchy and task label;
- queued, running, waiting, approval-needed, completed, failed, or cancelled state;
- resolved provider/model, elapsed time, and bounded token/resource totals;
- current capability operation or latest progress summary;
- unread message, artifact, conflict, or approval badges.

Rows update in place while active. Completed children collapse to a stable one-line summary; their
full transcript does not flood the parent conversation. When the root turn commits, the final tree
snapshot and result summary enter immutable scrollback exactly once, following the existing
`WorkUnit` insertion invariant.

Initially, shallow child rows can reuse `WorkUnit` rendering. Nested agents need a dedicated
`TaskTreeMessage` rather than encoding hierarchy into preformatted `WorkRow.label` strings. The
message stores semantic nodes and styles; rendering writes styled cells/spans to `ShadowBuffer`.
Do not embed ANSI control sequences in labels or event payloads. The terminal backend applies
Crossterm colors and attributes when changed cells are blitted.

### Agent inspector

Selecting a task opens an agent inspector as an overlay or alternate shadow-buffer surface:

```text
┌ Agents ─────────────────┬ Child: audit permissions ─────────────────────┐
│ ◆ root                  │ status: waiting for parent                    │
│ ├─ ● VM bindings       │ model: claude-sonnet · depth 1 · 2.1k tokens │
│ ├─ ◐ scheduler         │                                               │
│ └─ ✓ permissions   [2] │ latest message / streamed response            │
│                         │ capability calls and compact outputs          │
│                         │ artifacts: findings.md, changeset #18         │
├─────────────────────────┴───────────────────────────────────────────────┤
│ Enter focus  Tab next  m message  c cancel  d diff  Esc close          │
└─────────────────────────────────────────────────────────────────────────┘
```

The left pane is the task tree; the right pane is a lens over the selected task's event stream.
Views can switch among transcript, capability calls, artifacts/diffs, VM values, and resource
usage without copying those streams into the main chat. Parent and child transcripts retain
independent scroll positions and ring-buffer limits.

Useful interactions:

- focus a child and follow or pause its live stream;
- send a typed parent message or additional artifact reference;
- cancel a task or a whole subtree with confirmation appropriate to policy;
- inspect the exact starting-context hash, grants, provider/model, and descendants;
- preview, approve, reject, or resolve a proposed changeset;
- answer a child-originated question without abandoning the root conversation;
- promote a useful child result into the main conversation or shared vocabulary.

Keyboard bindings are illustrative and must be integrated with the existing input/dialog priority
rules rather than intercepted globally.

### Questions, approvals, and attention

Child questions and capability approvals enter one ordered attention queue. The live task row gets
a badge and the status bar reports the count. Opening an item shows provenance such as
`root > scheduler > test-runner`, the requesting model, requested effect, relevant diff or command,
and the grant that caused escalation.

An approval response is a persisted scheduler event delivered to the waiting continuation. It is
not sent by mutating the child's prompt directly. Multiple independent approvals may remain open;
the tabbed dialog system can present them without blocking rendering or unrelated agents.

The root agent may continue orchestrating unrelated work while one child waits for the user. The
root turn is considered suspended, not silently complete, if all remaining work is waiting on
attention.

### Rendering and update flow

Use a one-way projection pipeline:

```text
scheduler events -> TaskTreeProjection -> semantic render nodes -> ShadowBuffer -> cell diff
```

High-frequency token and progress events are coalesced on the normal render tick. The shadow
buffer changes only affected cells, so several streaming children do not cause whole-screen
redraws. Event ingestion never waits for a terminal draw, and rendering never holds scheduler or
VM locks.

Use stable IDs for root work units, task rows, messages, artifacts, and approvals. This lets a row
move between queued/running/completed views without being duplicated in scrollback. Terminal size
changes reflow the projection; they do not alter task state.

The same projection should support other clients. The daemon/API emits task snapshots plus ordered
events, while the terminal, future GUI, and remote clients choose their own layout. UI-specific
collapse state, selection, and scroll offsets remain client-local.

### UI implementation slices

1. Add task lifecycle variants to the durable runtime event schema and bridge them into
   `ReplEvent` without embedding display strings.
2. Add `TaskTreeProjection` and unit-test event folding, ordering, reparenting, terminal states,
   unread badges, and reconnect replay.
3. Add a semantic `TaskTreeMessage` to the live area with one-level expansion and immutable final
   scrollback insertion.
4. Add the inspector surface, task focus/navigation, and independent transcript buffers.
5. Route child questions, approvals, messages, artifacts, and cancellation through the inspector.
6. Add nested-tree, resize, narrow-terminal, no-color, rapid-update, and reconnect snapshot tests.

The UI is intentionally a projection phase after the scheduler protocol. A minimal tree ships with
Phase 3; richer inspector views can evolve independently without changing agent semantics.

## Delivery plan

### Phase 0: Characterize and protect current behavior

- Add integration tests for Forth and Lisp arithmetic, current isolated Forth evaluation,
  provider tool-call round trips, and the existing `spawn_task` loop.
- Add a registry test that records exactly which tools each provider and subagent sees.
- Record baseline metrics: tool schema tokens, model round trips, shell invocations, execution
  latency, and permission prompts for representative coding tasks.
- Mark direct subagent execution without permission checks as unsupported; do not expose it at
  top level until Phase 2 supplies the shared broker.

Exit criteria: current behavior is reproducible and regressions are visible across Grok,
Claude, OpenAI-compatible, and one local provider fixture.

### Phase 1: Shared runtime service and structured execution

- Introduce `ProgramRuntime`, owning the Forth VM, Lisp environment, program registry,
  revisions, and execution limits behind session-safe synchronization.
- Add `ExecutionContext`, `ExecutionOutcome`, structured diagnostics, cancellation, and fuel.
- Implement direct Forth and Lisp invocation without routing through terminal input or the
  conversational stack.
- Add `submit_program`, `invoke_program`, and VM introspection tools to the main registry.
- Build tool definitions once from the registry and pass the same definitions through every
  provider adapter.

Exit criteria: Grok and the other provider fixtures can add numbers and invoke stored Forth and
Lisp definitions through the identical function-tool contract, with no shell or TUI mediation.

### Phase 2: Capability broker and native workspace operations

- Define a typed `Capability`/`EffectRequest` enum and a `CapabilityBroker` used by tools,
  Forth primitives, Lisp primitives, and subagents.
- Move existing native read, glob, search, write, edit, patch, hash, HTTP, dialog, memory, and
  plan implementations behind broker handlers instead of duplicating their logic.
- Move enabled GUI automation behind the broker, replace AppleScript inspection with native host
  APIs, and bind the supported operations into both VM languages.
- Split process execution into `process.run(executable, argv, ...)` and an explicitly named
  `shell.run(script, ...)` capability.
- Add changeset preview, content hashes, atomic application, and stale-file conflict results.
- Route every request through permission policy and append an auditable effect event.

Exit criteria: a model can inspect and modify a workspace using Forth or Lisp without invoking
a shell; permission decisions and diffs match equivalent legacy tool calls.

### Phase 3: Safe task and subagent scheduling

- Add a runtime scheduler with task handles, bounded concurrency, cancellation, deadlines, and
  structured results.
- Implement concurrent VM `task.spawn`/`await` on explicit snapshots and conflict-safe commit.
- Refactor `TaskTool` into an `AgentScheduler` client; remove its private tool construction and
  direct, unchecked execution path.
- Add native `agent.spawn`, `agent.poll`, `agent.await`, `agent.cancel`, and messaging
  capabilities to both languages.
- Register the compatibility `spawn_task` tool as a broker-backed adapter until models have
  migrated to program submission.
- Project scheduler events into a live `TaskTreeMessage`, and ship the first agent-inspector view
  for transcript, approval, artifact, message, and cancellation interaction.

Exit criteria: Grok can spawn bounded child agents and concurrent VM tasks; child operations
obey the same permissions, workspace root, audit, and cancellation rules as the parent.

### Phase 4: Vocabulary, discovery, and efficient reuse

- Complete the runtime manifest and refresh it on startup, model switch, context compaction,
  scope change, and registry-generation mismatch.
- Expose vocabulary search, inspection, testing, definition, and promotion equally in Forth
  and Lisp.
- Retrieve only relevant signatures and documentation for a task; load full source on demand.
- Cache compiled programs by source, dependency, runtime, and environment hashes.
- Let models invoke stable program IDs directly so repeated workflows avoid regeneration and
  extra model turns.

Exit criteria: common coding workflows reuse stored programs, and growing vocabulary does not
cause linear prompt growth.

### Phase 5: Program-first default and legacy retirement

- Translate legacy provider tool calls into capability requests and compare results in shadow
  mode.
- Run representative repositories through both paths and compare output, diffs, approvals,
  failures, latency, and token usage.
- Make the compact program/runtime tools the default model-facing API.
- Retain direct legacy tools behind a compatibility setting for one release, then remove their
  schemas after telemetry and tests meet the gates below.

Exit criteria: normal file/search/edit/agent workflows use the VM; process execution occurs only
for genuine external programs, and explicit shell use is rare and visible.

## Suggested code boundaries

Avoid putting more session state into `ToolContext`. Prefer explicit services with narrow
interfaces:

```text
src/runtime/mod.rs              ProgramRuntime and session binding
src/runtime/context.rs          ExecutionContext, authority, budgets
src/runtime/outcome.rs          values, diagnostics, suspension, usage
src/runtime/capabilities.rs     typed requests and effect derivation
src/runtime/broker.rs           permissions, dispatch, audit
src/runtime/scheduler.rs        VM tasks and child agents
src/runtime/providers.rs        dynamic provider/model resolver
src/runtime/changeset.rs        preview, hashes, atomic application
src/runtime/automation.rs       optional automation capability and availability
src/coforth/capabilities.rs     Forth bindings only
src/lisp/capabilities.rs        Lisp bindings only
src/tools/implementations/program.rs  thin provider-tool adapters
```

Keep provider adapters responsible only for request/response translation. Keep the REPL/TUI
responsible only for presentation, approvals, and user commands. Neither should own runtime
semantics.

## Test matrix

Every phase adds unit, integration, and provider-contract tests.

| Scenario | Required assertion |
|---|---|
| Arithmetic | Forth and Lisp return equivalent tagged numeric values |
| Provider parity | Identical runtime schemas and successful tool round trip per adapter |
| Native workspace task | Read/search/patch completes with zero shell invocations |
| Process task | Executable and argv remain distinct; no implicit shell parsing |
| Permission parity | Tool, VM, and child-agent requests receive the same decision |
| Snapshot isolation | Child mutation cannot silently alter parent VM state |
| Concurrent tasks | Results join deterministically; conflicts are explicit |
| Subagent recursion | Depth, count, tokens, deadline, and cancellation are enforced |
| UI event projection | Replay and reconnect produce the same task tree without duplicates |
| Concurrent rendering | Coalesced child updates do not block execution or redraw unchanged cells |
| Attention routing | Child questions and approvals show ancestry and resume the right continuation |
| Automation disabled | No automation vocabulary is advertised or callable |
| Automation consent | Missing/revoked OS permission returns structured availability changes |
| Native automation | Inspect/click/type paths invoke no shell or AppleScript process |
| Automation delegation | Children receive only explicitly delegated automation grants |
| Scheduled execution | Durable trigger invokes the pinned program once under stored policy |
| Model switch | New children use the active provider unless explicitly pinned |
| Stale manifest | Execution fails safely and returns the current generation |
| Changeset conflict | Modified files are not overwritten after hash mismatch |
| Resource exhaustion | Fuel/time/output limits return structured failures |

Run real-provider smoke tests only when credentials are available. Deterministic provider
fixtures must cover all protocol behavior in CI, including Grok-style streamed tool-call
fragments.

## Release gates

Program-first becomes the default only when all of these are true:

- 100% of native capability tests use the shared broker from top-level agents and subagents;
- Grok, Claude, OpenAI-compatible, and local-provider fixtures pass the same contract suite;
- representative read/search/edit tasks require zero shell invocations;
- enabled native automation requires zero implicit shell invocations;
- no workspace mutation occurs without a previewable, hash-bound changeset or explicit policy;
- cancellation and limits terminate runaway VM programs and recursive agents;
- stored-program reuse reduces median model round trips without increasing failure rate;
- every turn ends in a visible completed, suspended, failed, or cancelled event;
- compatibility mode can be disabled without losing required coding workflows.

## First implementation slice

Keep the first change deliberately narrow:

1. Add `ProgramRuntime`, `ExecutionContext`, and `ExecutionOutcome` for pure execution only.
2. Expose provider-neutral `submit_program` for Forth and Lisp with `Pure`, `VmRead`, and
   `VmWrite` effects; reject all other effects structurally.
3. Return tagged values and diagnostics without passing through `Push`, `Run`, terminal input,
   or string replacement protocols.
4. Register the same tool from the central registry for every provider.
5. Add fixture-based Grok/OpenAI, Claude, and local-provider contract tests.
6. Prove Forth addition, Lisp addition, isolated VM evaluation, stale-generation rejection,
   cancellation, and resource limits.

Do not register the current `TaskTool` in this slice. Its unchecked private executor would create
a second security model. Add subagents only after the capability broker is the sole execution
path.
