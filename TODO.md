# Finch TODO

This is the short, discoverable work queue. Detailed rationale and protocol sketches live in
[`docs/TYPED_LISP_FORTH_CAPABILITY_JIT_PLAN.md`](docs/TYPED_LISP_FORTH_CAPABILITY_JIT_PLAN.md),
[`docs/SHARED_PROGRAM_RUNTIME_PLAN.md`](docs/SHARED_PROGRAM_RUNTIME_PLAN.md), and
[`docs/BRAIN_CONVERGENCE_PLAN.md`](docs/BRAIN_CONVERGENCE_PLAN.md).

## Typed Lisp/Co-Forth VM — prerequisite for Brain convergence

- [ ] Fix startup rendering ownership: direct stdout from the legacy banner/proof demo can overlap
  the shadow-buffer live area and leave stale glyphs (for example a corrupted `258/258` proof
  count). Startup diagnostics and suggestions must be projected through the same UI/event path or
  remain opt-in commands; never interleave direct terminal writes with shadow-buffer redraws.
- [ ] Finish both source frontends and their shared typed IR semantics: definitions/signatures,
  conditionals, metered loops, locals, quotations, closures, collections, bounded macros, and
  structured error/result forms.
- [ ] Generalize the managed JSON boundary into first-class typed records/maps only after both
  frontends share construction, field/key lookup, pattern matching, serialization, and row-type
  rules. `json-parse`/`json-get`/scalar option projections now provide safe object-field access;
  do not expose the existing internal `TypedValue::Record` ABI representation as an ad-hoc source
  language feature first.
- [ ] Extend the implemented typed option/result branches, type-directed Lisp `match`, and
  no-fallthrough integer Co-Forth `case` to richer structured patterns plus expression-valued
  named `break` after the loop verifier is generalized to check each target's declared result
  stack row. Simple named stack-preserving `break`/`continue` already lower to verified loop
  edges; do not add unrestricted jumps or C-style fallthrough switches.
- [x] Specify and implement the provider wire discriminator: leading `(` selects Lisp and all other
  valid program starts select Co-Forth; make the receiver incrementally tokenize Co-Forth while
  retaining complete-program verification and clear malformed-wire diagnostics.
- [x] Add one bounded provider-wire repair turn for reader/type/capability diagnostics. Preserve the
  rejected source and its diagnostic as journaled program/output WorkUnits, send the structured
  error plus exact source back to the same provider, and render any replacement as a new program
  rather than silently overwriting history. Never auto-retry a host effect, approval, cancellation,
  timeout, or externally partial operation.
- [x] Publish the canonical source surface in the generated language package: `s"..."` strings,
  exact escaping/comment rules, `say` composition, Lisp form examples, and no-free-prose protocol
  rule. Include common stack-error corrections rather than only error codes.
- [ ] Generate every production word/function from one typed signature, effect, documentation, and
  host-implementation registry.
- [ ] Refine Lisp source maps from the implemented exact enclosing top-level-form spans to exact
  nested expression/token spans and macro-expansion ancestry. Do not regress to whole-submission
  spans or invent locations by source-text searching.
- [x] Make documentation a first-class field of typed Lisp/Co-Forth definitions and promotion
  records, not merely leading source comments. Preserve `; finch-doc:` / `\ finch-doc:` as a
  self-contained-script spelling, retain exact immutable version metadata, and converge provider
  discovery on `search_word` / `inspect_word`: compact search across core and persisted words,
  followed by full contract/source inspection where source exists. Generated provider manifests now
  advertise only `search_word` / `inspect_word`; keep `search_vm_vocabulary`, `inspect_vm_word`,
  `search_vocabulary`, and `inspect_program` as dispatch-only compatibility aliases until external
  clients migrate.
- [ ] Finish the capability broker: bounded argument templates, availability, grants, attenuation,
  revocation, audit, approval dialogs, runtime guards, and typed suspend/resume. The base
  serializable `VmResume { execution_id, sequence, response }` path now validates the host's
  result row, records result/denial/cancellation against the exact journaled effect, and never
  redispatches it. The portable-host submission policy can now suspend every approved awaited
  capability (not just editor proposals) for an external implementation; typed scheduled callbacks
  now persist a versioned creation-time grant ceiling and cannot acquire approvals granted later.
  Durable approvals, revocation policy, and complete host adapters remain.
- [ ] Bind files, native tools, processes, network, automation, MemTree, schedules, response output,
  and agent fork/join/model selection through typed VM primitives.
- [ ] Extend bounded `file-slice`/`file-size` and host-issued cursors with workbook cursors so large
  Excel workbooks can be processed incrementally without whole-file/string loading. Line cursors
  and bounded `csv-open`/`csv-next`/`csv-close` record cursors now cover UTF-8 CSV quoted-record
  framing; workbook-specific opaque resources remain.
- [ ] Replace the compatibility output adapter with portable output-handle effects: default response
  port append (`say`), append/replace/status/progress/complete/fail operations, per-handle ownership
  and generation checks, and journal-first projection into concurrent shadow-buffer WorkUnits. Provider
  wire effects and the provider `submit_program` tool now cross the client's ordered REPL event bus
  before any WorkUnit mutation, and explicitly entered typed programs run as background ProgramRuns
  whose completion returns through that same loop. Client projections reject duplicate or gapped
  `(execution_id, sequence)` envelopes, but this is only a live reconnect guard; still replace the
  remaining direct compatibility projections with durable application-journal/replay support.
- [ ] Complete the application-owned policy for cooperative typed-VM `yield`: the interactive
  provider-wire runner now yields its Tokio task and automatically resumes only an exact
  `PendingTypedReason::Yielded` continuation. General daemon/frontend scheduling still needs
  fairness, timer/I/O/message wakeups, cancellation, and durable replay before treating `yield` as
  autonomous background execution.
- [ ] Reimplement the existing model-facing `TodoRead`/`TodoWrite` tools over a typed, journaled
  task-list projection owned by the Brain/runtime. Keep their useful visible-plan UX and stable tool
  surface, but make task creation, status changes, hierarchy, progress, cancellation, and durable
  recovery ordinary typed task events—not a second session-local JSON source of truth beside VM
  `task<T>` handles.
- [ ] Normalize model-facing naming and manifests. `todo_read`/`todo_write`, `enter_plan_mode`,
  `present_plan`, and `ask_user_question` now advertise canonical snake_case names while
  dispatch-only PascalCase aliases preserve compatibility; extend that audited migration to every
  remaining provider/host tool. Typed Co-Forth words retain
  language-native hyphenated spelling (for example `output-append`); Lisp maps to the shared typed
  vocabulary. Remove PascalCase legacy names from generated manifests only after explicit aliases
  and tests establish that no duplicate semantic operation is advertised.
- [ ] Finish typed `proposal.open` as the explicit durable editor/proposal boundary. The current
  `proposal-open` capability can now suspend an event-loop-bound ProgramRun on its portable
  `(execution_id, sequence)` request and the frontend controller opens the language-aware editor,
  resumes that exact handle with accepted/chat/cancel data, and emits only the final tool result.
  Replace the compatibility projection with durable application-journaled
  `created → awaiting-edit → accepted|chat|cancelled → submitted` events and reconnectable
  proposal views. It must support Finch, Bash, Python, and other source artifacts without forcing
  an editor for ordinary individually authorized VM calls.
- [ ] Separate tool execution budgets from human/editor waits. The legacy universal 30-second
  timeout currently wraps `$EDITOR` proposal review and reports “try restarting” while waiting for
  a person. Model the lifecycle explicitly as `awaiting-approval|awaiting-edit → executing`;
  apply a bounded timeout only to the actual subprocess/host execution, retain live output, and
  report the true phase in the UI.
- [ ] After the broker and durable task protocol pass their gate, expose opt-in local operator
  bindings for accessibility, browser, mail, messaging, and credential-backed services as
  parameterized capabilities with audit/event-journal projection; never treat broad shell access
  as the integration contract.
- [ ] Extend the existing typed executable-script command with explicit isolated-versus-named-Brain
  state. `finch --exec` already consumes the tested Finch shebang envelope through `ProgramRuntime`
  (never the legacy interpreters), returns structured `--json` output, and uses the ordinary
  capability broker; it currently creates only an isolated runtime. Do not add named-Brain script
  state until the Brain convergence gate is open.
- [ ] Add package/import namespaces only after self-contained scripts and task/session/project/user
  vocabulary lifetimes are reliable. Keep promotion to project/user/published vocabulary an
  authority-bearing, reviewable operation.
- [ ] Adapt discovered MCP client tools into versioned namespaced typed VM bindings with schema
  validation, managed JSON fallback, parameter-bounded `mcp.call` capability grants, and normal
  suspension/resume; keep MCP transport lifecycle host-owned rather than a VM subagent protocol.
- [ ] Finish the persistent `ProgramRuntime` state model. Lisp and Co-Forth already share one
  persistent typed stack and dictionary, exposed by one inspection/revision boundary. Successful
  revisions now retain a serializable, reverified stack-and-definition checkpoint whenever they
  contain no host-owned handles; authority is intentionally not serialized. Add the managed heap,
  durable storage, host-handle restoration, and transaction manager before treating it as a
  restartable Brain state.
- [ ] Complete revisioned private working snapshots and conflict-aware commits. A `ProgramRuntime`
  now executes each ProgramRun on a cloned stack/dictionary snapshot and gates only snapshot/commit;
  stale resume checks and losing post-resume commits return structured failed outcomes without
  overwriting the winner, retaining their emitted-effect journals rather than replaying or
  compensating them. Still add durable revision history, structured outcomes for host-effect
  correlation/revocation races, and reviewed merge rules where a commutative delta can safely be
  accepted.
- [ ] Remove the Lisp-to-Forth text compiler, native Lisp fallback, source-text effect inference,
  and duplicate direct model-tool paths after conformance parity.
- [ ] Complete provider language packages, structured shadow-buffer outcomes, rollback/security
  tests, concurrency tests, and provider conformance tests. Manual configured-cloud smoke checks on
  2026-08-23 successfully executed provider-emitted Lisp `say`, Lisp arithmetic, and Co-Forth
  response programs through the raw wire receiver; this is useful integration evidence, not a
  substitute for fixed multi-provider conformance fixtures or recovery-rate measurements. Do not
  require the later Cranelift JIT optimization tier to begin Brain convergence.
- [ ] Freeze and test the Runtime/Application boundary: the embedder-neutral typed VM exposes only
  verified execution, diagnostics, capability requests, and idempotent side-effect/resume records;
  the Finch application supplies Brain, UI, approval, provider, MCP, scheduler, and OS adapters.
- [x] Complete the fiber/task split: `(defer :cpu (lambda () ...))` / `defer-cpu` has private-stack,
  immutable-capture CPU work with typed `task<T>` poll/join/cancel operations. A running join
  suspends the parent VM continuation rather than blocking the event loop; CPU fibers reject
  effects and never share their parent stack. Repeatedly-yielding fibers remain separate from
  subagents, and bidirectional resume remains deferred until a concrete need exists.
- [x] Implement a typed lazy sequence protocol separately from scheduler `yield`: host-backed
  `stream<T>` handles now provide bounded `stream-next -> option<T>` and `stream-close`, with
  ProgramRun ownership/generation checks, path-scoped capability propagation, concrete polymorphic
  host-result rows, and shared Lisp/Co-Forth lowering. File-line and CSV streams are the first
  backends; producer-backed repeated-yield fibers remain a distinct later extension. Legacy
  Co-Forth generators remain outside typed-runtime vocabulary until their state, effects, and
  suspension semantics are verified.
- [ ] Make resource roots first-class capability objects. Workspace/project paths remain safely
  relative; an intentional full-machine grant is a separate audited host root, never ambient
  authority inferred from an absolute path string. `host-path` and distinct
  `host-file-read`/`host-file-write` now require an explicitly installed host binding and recheck
  canonical containment at every call; keep workspace `path`/`file-read` structurally separate.
  Still add project/task-output bindings, persisted approval/revocation lifecycle, and the host UI
  for deliberately binding `/` as whole-machine scope.
- [ ] Phase 0: route existing provider streaming through the portable VM side-effect journal and
  per-ProgramRun output-handle bindings; test replay/reconnect and concurrent WorkUnit projection,
  then replay the existing Co-Forth corpus in report-only verifier mode before requiring typed output.
- [ ] Later: define a signed, content-addressed vocabulary package protocol for pushing reviewed
  `published` definitions between Finches; verify source/IR, dependencies, certificates, provenance,
  and local capability policy before installation.

## Shared brains and environments

- [ ] After every VM prerequisite above passes, execute
  [`docs/BRAIN_CONVERGENCE_PLAN.md`](docs/BRAIN_CONVERGENCE_PLAN.md): consolidate the three current
  Brain concepts into one daemon-authoritative event log, VM history, environment, and authority
  boundary; model interactive turns, speculative helpers, schedules, and subagents as `BrainRun`s;
  and make local, embedded, IPC, HTTP/WebSocket, and remote clients projections of one service.
- [ ] Persist complete VM checkpoints or reversible VM deltas at committed program boundaries.
- [ ] Make the daemon own schedule definitions/due-time delivery only. Coalesce missed ticks into one
  pending event per schedule while the environment-owning frontend is unavailable; require explicit
  bounded catch-up and idempotency policy before delivering every missed occurrence.
- [ ] Split reducible VM state from the execute-once host-effect journal. Never replay file,
  process, dialog, or network effects while restoring VM state.
- [ ] Add typed compensating actions for reversible effects. File undo must use preimage and
  postimage hashes plus a conflict-aware reverse changeset.
- [ ] Finish remote named-brain attach/detach, live scrollback replacement, status display, and
  prompt/Forth/Lisp routing through the daemon-owned event stream.
- [ ] After the VM gate, launch each local active Brain runner in a named `tmux` session by default
  on Unix. Keep the daemon as a durable event-log/coordinator with no workspace, accessibility, or
  credential handles; the master frontend runner is the only environment authority. Recover only
  from validated checkpoints on that environment and never replay recorded external effects after
  runner or `tmux` failure.
- [ ] Define Brain initialization as a reviewed typed program/module with an explicit capability
  budget and journaled effects. Deterministic VM vocabulary/module loading may occur before a
  runner accepts turns; proofs, poetry, provider calls, and other observable initialization work
  must be separately scheduled/approved BrainRuns. Do not revive the legacy mutable
  `boot = true` Co-Forth registry as an ambient startup hook.
- [ ] After the VM gate, design an optional reviewed **semantic-convergence corpus** for a Brain:
  versioned examples, equivalences, claims, and executable proofs that document how human/LLM
  symbols acquire shared meaning. It may inform a provider manifest and be refined through
  explicit proposals, but it must never silently redefine core words or execute as ambient boot
  poetry.
- [ ] Add per-brain control ownership/leases and participant roles. Make the current attachment
  role explicit in every console status bar and permission/proposal view: `runner` (the one
  environment-owning executor), `driver` (may queue prompts/programs), `consultant` (may push
  bounded context/reviews), or `observer` (read-only). Approval/control scopes remain separate
  from these roles. Only the bound environment may execute workspace effects or reveal/rotate its
  credential.
- [ ] Add remote brain creation while preserving the invariant that one environment is an
  indivisible machine/workspace authority boundary.
- [ ] Treat the global brain password as a local/bootstrap credential only. Mint scoped, revocable,
  expiring participant credentials containing subject, audience, brain, environment generation,
  and permitted roles. Initial scopes: `brain:read`, `brain:submit`, `brain:approve`,
  `brain:control`, `environment:execute`, `environment:admin`, and `compute:submit`.
- [ ] Enforce least privilege independently for event visibility, prompt/program submission,
  approval, control-lease ownership, workspace effects, environment changes, credential minting,
  and distributed inference. Never advertise credentials in mDNS discovery records.

## Client and model integration

- [ ] Keep the ordinary OpenAI-compatible API client-managed and stateless with respect to named
  brains: Cline/Roo own their conversation and tool loop while Finch supplies model inference.
- [ ] Optionally add an explicit brain-scoped OpenAI mode (for example a `finch/brain/<name>` model
  ID) for clients that deliberately want to join a Finch event log. Never infer that attachment
  from an ordinary `/v1/chat/completions` request.
- [ ] Run the complete coding-agent/tool loop on the brain environment. Remote clients submit
  prompts and approvals; file/process actions execute only in that environment.
- [ ] Make OpenAI tool-call behavior respect control ownership: a participant client must not
  accidentally execute workspace tools on its own machine.

## Distributed inference

- [ ] Extend mDNS node advertisements with a versioned compute manifest: CPU/GPU/TPU kind,
  device count, memory capacity/availability, runtimes, loaded models, queue depth, and approximate
  throughput. Confirm current values through an authenticated node API.
- [ ] Schedule bounded, content-addressed inference jobs across discovered compute nodes without
  granting those nodes workspace or execution-environment authority.
- [ ] Record remote inference provenance: node, model, input hash, resource budget, timing, and
  brain environment generation.
