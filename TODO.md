# Finch TODO

This is the short, discoverable work queue. Detailed rationale and protocol sketches live in
[`docs/TYPED_LISP_FORTH_CAPABILITY_JIT_PLAN.md`](docs/TYPED_LISP_FORTH_CAPABILITY_JIT_PLAN.md),
[`docs/SHARED_PROGRAM_RUNTIME_PLAN.md`](docs/SHARED_PROGRAM_RUNTIME_PLAN.md), and
[`docs/BRAIN_CONVERGENCE_PLAN.md`](docs/BRAIN_CONVERGENCE_PLAN.md).

## Typed Lisp/Co-Forth VM — prerequisite for Brain convergence

- [x] Fix startup rendering ownership: the live event loop projects its header through
  `OutputManager`, puts notices in the status bar, silently loads vocabulary, and never executes
  legacy boot scripts at interactive startup. Startup diagnostics and suggestions must continue to
  use that UI/event path or remain opt-in commands; never interleave direct terminal writes with
  shadow-buffer redraws.
- [ ] Finish both source frontends and their shared typed IR semantics: definitions/signatures,
  conditionals, metered loops, locals, quotations, closures, collections, bounded macros, and
  structured error/result forms. Maintain a hard parity invariant: every executable shared-IR
  operation must have one canonical Co-Forth spelling and conformance test; Lisp may add readable
  syntax but must lower directly to that same IR rather than introduce Lisp-only execution
  semantics. Preserve source spans directly—never satisfy parity by reparsing generated Forth text.
  Captured anonymous Co-Forth quotations now lower to the same closure IR as Lisp lambdas, retain
  exact body spans/effect inference, may escape their defining frame, and survive checkpoints;
  finish the remaining bounded compile-time and structured-pattern surface before closing this gate.
- [ ] Enforce single-pass source parsing for every Lisp and Co-Forth module. Consume each source
  byte stream once into span-carrying syntax/lowering events; macro expansion, name resolution,
  optimization, linking, and the independent security verifier may operate on those retained
  structures but must never rescan source, reparse generated text, or require C++-style iterative
  parsing. Use declaration-before-use and explicit typed module interfaces where necessary;
  mutually recursive definitions may be declared in an interface before their bodies. Keep the
  verifier as an independent semantic pass over IR—security verification is not source parsing.
- [ ] Add a dependency-driven semantic scheduler over retained AST/module interfaces. Parse and
  register every declaration once, then advance each symbol or generic instantiation through
  monotonic phases such as `Declared → SignatureReady → BodyTyped → Lowered`. A semantic job that
  needs another symbol/phase yields an explicit compiler continuation (`Needs(symbol, phase)`), and
  the scheduler resumes it when that dependency is ready. Memoize generic instantiations by
  immutable definition/module identity plus type/value arguments. Detect phase-specific dependency
  cycles with a complete source trace; permit declared mutual recursion where signatures break the
  cycle, but reject compile-time value/layout cycles. Bound jobs and expansion fuel. Keep these
  compiler continuations distinct from runtime `fiber<Y,R>`, and prefer explicit resumable state
  machines over native stackful fibers unless measurement proves the latter materially simpler.
  This design is informed by SDC's small fiber-based semantic scheduler, without inheriting its
  unresolved-forward-reference or per-fiber native-stack limitations.
- [ ] Put an explicit span-preserving AST boundary between both source readers and typed stack IR.
  Formalize Lisp's existing `Val`/`SpannedVal` tree as its frontend AST. Co-Forth now tokenizes a
  module once into a span-preserving `ForthModuleAst` containing definitions and retained body
  nodes; definition and anonymous-quotation bodies lower from those nodes without source
  copying, masking, or re-tokenization, and nested diagnostics retain original module coordinates.
  Anonymous quotations and their signatures/bodies are now recursive nodes directly in each body
  sequence—there is no byte-offset quotation side table, and lowering does not rediscover or skip
  their delimiters. Integers, booleans, symbols, strings, and pasted JSON are likewise classified
  as typed literal nodes during that source pass rather than reinterpreted during IR emission.
  All other body elements are explicit unresolved-word nodes rather than generic token atoms.
  Finish elaborating those words into structured Co-Forth AST nodes for control flow and resolved
  local/call references. Then perform one post-order semantic lowering into
  the shared IR; lower syntactic sugar and bounded macro rewrites over AST nodes without serializing
  or reparsing source. Source-defined generics and compile-time templates are now the concrete case
  that may require one small shared parametric HIR (or equivalently elaborated AST): retain generic
  parameters, constraints, unresolved bodies, module-interface references, and expansion origins
  until instantiation, then emit verified stack IR. Current `Type::Variable`/`StackSignature`
  substitution is sufficient for polymorphic core-word calls but does not by itself define a
  general user template system. Specify and test this boundary in both frontends before claiming
  generics/templates complete. Do not turn it into a chain of incidental HIR passes. Preserve every
  original and expansion span through the final IR.
- [ ] Generalize the managed JSON boundary into first-class typed records/maps. Both frontends now
  share immutable typed-map construction, key lookup/update, keys/length, serialization across the
  public runtime boundary, map-type unification, immutable heterogeneous record construction, and
  statically named option-valued field projection, immutable static field update, and
  insertion-ordered `map-entries` iteration through typed key/value records, homogeneous typed-list
  construction/immutable append in both frontends, direct pasted Co-Forth JSON-object literals,
  explicit `json-as-map -> option<map<string,json>>` normalization, and total statically checked
  Lisp record-subset and exhaustive list destructuring lowered to shared public words/branch/local
  IR. Closed `variant{...}` types, payload-checked constructors, safe tag projection, and exhaustive
  Lisp variant patterns now lower to shared operations with Co-Forth conformance coverage; still
  add row-polymorphic record rules. The deliberate typed brace-record syntax is the only record
  source surface; `json-parse`/`json-get`/scalar option projections remain the safe boundary for
  arbitrary JSON objects rather than exposing the internal `TypedValue::Record` ABI ad hoc.
- [ ] Add named structural schema declarations and bounded compile-time derivation after record-row
  rules are stable. Today record types are anonymous inline shapes such as
  `record{name:string,age:int}`; define one shared declaration/alias representation with equivalent
  Lisp and Co-Forth spellings that erases to the same structural IR type. A pure deterministic
  `derive` facility may consume immutable schema metadata and generate ordinary inspectable typed
  words (for example JSON/YAML encoders and `result<Record,DecodeError>` decoders), but must not run
  host effects or hide privileged compiler behavior. If general definition reflection is later
  exposed, specify a versioned hygienic syntax/typed-IR value and bounded read-only compile-time
  API; `inspect_word` model/application access to source is not itself a macro capability. Keep
  arbitrary external object keys—including names with spaces—at the `json` or `map<string,json>`
  boundary unless an explicit schema maps them to typed fields.
- [ ] Extend the implemented typed option/result branches, type-directed Lisp `match`, and
  no-fallthrough integer Co-Forth `case` to richer structured patterns plus expression-valued
  named `break` after the loop verifier is generalized to check each target's declared result
  stack row. Total record subset patterns now bind statically present fields through ordinary
  shared projection/local operations. Exhaustive Lisp `empty`/`cons` patterns lower through the
  public polymorphic `list-uncons`, option branches, record projection, and locals that Co-Forth can
  compose directly. Simple named stack-preserving `break`/`continue` already lower to verified loop
  edges; do not add unrestricted jumps or C-style fallthrough switches.
- [x] Add explicit typed result propagation after branch forms are stable: Lisp `try` and
  Co-Forth `?` early-return an `err` only from a function whose sole declared output is a
  compatible `result<T,E>`. The verifier checks the cold return edge, the interpreter unwinds the
  current frame without replaying effects, and the normal edge retains the unwrapped `ok` value.
  Keep `match-result`/`if-ok` for recovery and `unwrap` as a deliberate diagnostic trap; do not
  introduce dynamically catchable language exceptions or silently replay host effects.
- [ ] After typed result propagation is established, add lexical scope guards (`on-exit`, `on-ok`,
  `on-err`) as compiler-generated once-only cleanup edges for normal return, result propagation,
  cancellation, and resumed continuations. They must not imply rollback of journaled external
  effects or become a dynamically catchable exception system.
- [x] Specify and implement the provider wire discriminator: leading `(` selects Lisp and all other
  valid program starts select Co-Forth; make the receiver incrementally tokenize Co-Forth while
  retaining complete-program verification and clear malformed-wire diagnostics.
- [ ] Specify and test an optional mixed top-level wire stream now that Lisp and Co-Forth share one
  IR. Outside an open string/record/definition/control construct, `(` may begin one complete Lisp
  form while a Co-Forth token begins one complete concatenative unit; neither frontend may parse
  through the other's owned lexical region. Define the transaction boundary explicitly: a closed,
  independently verified unit may execute/commit and emit progressive effects, and a later malformed
  unit cannot pretend those commits or effects rolled back. Incomplete Lisp forms and incomplete
  Co-Forth definitions/control regions remain visible source only and never execute. Preserve an
  explicit single-language mode for self-contained scripts, conformance fixtures, and diagnostics.
- [x] Add one bounded provider-wire repair turn for reader/type/capability diagnostics. Preserve the
  rejected source and its diagnostic as journaled program/output WorkUnits, send the structured
  error plus exact source back to the same provider, and render any replacement as a new program
  rather than silently overwriting history. Never auto-retry a host effect, approval, cancellation,
  timeout, or externally partial operation.
- [x] Publish the canonical source surface in the generated language package: `s"..."` strings,
  exact escaping/comment rules, `say` composition, Lisp form examples, and no-free-prose protocol
  rule. Include common stack-error corrections rather than only error codes.
- [x] Record provider wire adherence by provider/model and failure class: first-pass valid
  `ProgramSubmission`, raw prose, Markdown fence, invented word, stack/type error, wrong language
  dispatch, missing output effect, capability error, repaired successfully, and terminal failure.
  Interactive, one-shot, and named-Brain receivers append source-free JSONL metrics, while `/metrics`
  groups results by provider/model; the visible program/error history remains independent and never
  hides a rejected source merely because bounded repair succeeds.
- [ ] Run the fixed protocol-conformance workload across every supported configured provider/model,
  publish sample size and recovery rates, and keep regressions as replayable fixtures. A live
  configured-Grok first-pass Lisp result proves the recorder path, not cross-provider conformance.
  The ignored `live_parity_finch_wire_programs` suite now executes three fixed response/arithmetic/
  definition tasks through the real typed runtime, performs at most one source-only repair, and
  prints source-free first-pass/repaired/terminal counts for every configured provider. On
  2026-08-24 the configured Grok profile completed all 3 tasks first-pass (0 repaired, 0 terminal),
  including a recursive factorial definition; other provider/model profiles remain unmeasured.
  Expand the fixed matrix to ordinary and quoted/multiline responses, calculation, introspection,
  bounded file effects, approval/denial, loops, closures, fibers, malformed-wire repair, and
  unknown-word recovery. Preserve provider/model and runtime/language-package versions, sample
  size, first-pass wire/parse/verification rates, repair success and attempts, invented-word/raw-
  prose/Markdown-fence rates, selected frontend, tokens, and latency in a reproducible aggregate
  artifact suitable for the eventual Finch protocol write-up; never infer a broad provider claim
  from an ad hoc transcript.
- [ ] Consider an opt-in typed unresolved-word handler for a module/run. Ordinary unbound bare
  words must remain linking diagnostics; an explicitly installed handler may receive the unknown
  token as `symbol`/`string` and has one declared signature/effect contract, allowing controlled
  lookup/delegation without silently turning leaked prose or misspelled capabilities into data.
- [ ] Later language evolution: typed records already carry ordinary closure values; add explicit
  method-call sugar that passes `self` and returns a replacement record (never hidden ambient
  mutation). Design concepts/constraints and coherent overload resolution only after the core
  vocabulary, diagnostics, capability inference, and closure serialization are stable. Concepts
  must be structural requirements over explicitly imported, visible typed words—not nominal
  Rust-style trait implementations—and resolution must select one coherent candidate or diagnose
  ambiguity rather than escape through `dynamic`. No surface construct may receive privileged
  overload resolution or optimization unavailable to a normal typed definition: compiler-owned
  `for`/`foreach` syntax may select an indexed, range, fiber, or collection-specific loop lowering,
  but selection must use public structural contracts and resolved word IDs so user-defined types
  and traversal functions can receive the same specialization/inlining as built-ins. Keep one
  `foreach` surface: a compile-time range plus pure compile-time body executes through bounded CTFE,
  while a runtime range lowers to the verified loop protocol; do not add a separate `static foreach`.
- [x] Generate every production core word from one typed signature, effect, documentation, and
  host-implementation registry. `CoreWordSpec` is the immutable source consumed by frontend
  discovery, the verifier, interpreter dispatch, and host-binding validation; user definitions
  retain their own versioned typed contracts rather than pretending to be host primitives.
- [x] Finish Lisp source maps: the reader retains structural spans and typed lowering preserves
  exact nested named-call operators/arguments, `begin`/`if`, definitions, `let`, lambdas,
  closure-call targets, typed matches, loops, deferred tasks, macro call-site → template
  ancestry, and exact caller-argument spans through macro substitution. Do not regress to
  whole-submission spans or invent locations by source-text searching.
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
  redispatches it. Every outcome now retains the complete statically inferred capability envelope,
  separately from its currently missing `required_capabilities`, so completed and untaken branches
  remain previewable/auditable. The portable-host submission policy can now suspend every approved awaited
  capability (not just editor proposals) for an external implementation; typed scheduled callbacks
  now persist a versioned creation-time grant ceiling and cannot acquire approvals granted later.
  Global runtime grants now receive stable IDs in a serializable host-owned `CapabilityLedger`;
  grant/revoke audit events are ordered, revocation and expiry rebuild the VM's active authority at
  submission/resume boundaries, and VM checkpoints still contain no authority. The ledger now
  validates issuance for every declared scope, records source-free authorization decisions in the
  same total order, and atomically consumes a matching `once` grant after its first successful
  decision. Each private ProgramRun now derives reusable authority from its exact task, session,
  project, and policy identity; exact grants never enter ambient runtime authority. Approval prompts
  have deterministic execution/effect-bound IDs and preserve child ancestry. The atomic
  `resolve_typed_approval` path revalidates the retained prompt, issues the selected scope, audits
  allow/deny, consumes `once`, resumes only that pending boundary, and rejects stale or forged UI
  actions before they can create reusable authority. Interactive provider-wire runs now route that
  exact prompt through a structured application dialog and resume the retained frame without source
  replay. Session/project identity plus the ledger have a separate atomic, integrity-checked
  `ProgramRuntimeAuthorityStore`; a missing or policy-incompatible record fails closed and VM
  archives remain authority-free. Provider wire, explicitly entered Lisp/Co-Forth, and legacy
  provider-tool submissions now share that approval controller; repeated capability boundaries
  continue through their exact saved frames. Host availability is now a selector-aware property
  independent from grants and is visible in approval dialogs. Child agents retain a creation-time
  inherited grant ceiling, so later session/project/global grants cannot silently widen them;
  explicit task-scoped approval is the escalation path. Named Brain compatibility storage now
  persists and restores that separate authority record beside its content-addressed VM checkpoints;
  a missing record restores no grants, a tampered record fails closed, and archiving the Brain moves
  both lifecycles together. Every non-intrinsic awaited host boundary now re-resolves its concrete
  request against the ledger immediately before local dispatch or portable deferral, records the
  stable grant ID with execution/effect sequence once, and reuses that execute-once authorization
  fact when an external result resumes. Named Brain runtimes now install an application-owned
  authority sink: grant, revoke, denial, and host-authorization mutations atomically replace the
  separate integrity-checked authority record immediately, even when no VM revision commits or the
  surrounding run later rolls back. Persistence failure restores the prior in-memory ledger and
  fails closed; archiving detaches the sink so retained runtime references cannot recreate the old
  policy path. Typed scheduling now binds create/read/manage through opaque `schedule` resources:
  `schedule-get` returns redacted structured metadata, `schedule-cancel` preserves the durable row,
  and the queue atomically arbitrates `Pending → Running|Cancelled` so cancellation cannot report
  success after a worker has claimed the callback. Host-owned `CapabilityPolicy` revisions and
  capability-wide denials now persist in that authority record: a new immutable policy identity
  atomically revokes every active grant from the prior revision, denial prevents reissuance, every
  host boundary re-reads the live policy, and sink failure restores both policy and ledger. Reusing
  one policy identity for different contents fails closed; old integrity-signed authority records
  without an explicit policy migrate to the original default without losing their checksum proof.
  Still complete the remaining host adapters and the approval/history UI for policy changes.
- [ ] Bind files, native tools, processes, network, automation, MemTree, schedules, response output,
  and agent fork/join/model selection through typed VM primitives. `agent-spawn-with` now carries
  one exact typed role/background/provider/model/budget record through both frontends into the
  configured-profile resolver; unavailable models and invalid budgets fail before task creation.
  Poll and await now return exact typed snapshot/result records rather than scheduler JSON or a
  bare final-message string. Child schedulers no longer expose direct Read/Glob/Grep or direct
  agent-control tools: they retain typed `submit_program` plus read-only discovery, and use the
  bounded structural `tree-list`/`file-*`/`agent-*` vocabulary under the child grant ceiling.
  Still add explicit starting-context references/hashes and capability-subset requests, and audit
  remaining root/provider legacy model-selection/tool entry points before closing this gate.
- [ ] Define a compact, discoverable data-work vocabulary before asking models to synthesize their
  own large-file loops: workspace tree metadata, bounded file hash, a bounded host-computed
  directory Merkle root, and bounded host-computed CSV header/per-column summaries now exist; add workbook
  metadata/range cursors. Build security/integrity inspection from these explicit bounded facts
  (inventory, metadata, hashes, rules/signatures, provenance), with any remediation remaining a
  separately authorized proposal/effect. Each contract must advertise result shape and byte/work
  bounds; bulk materialization into a model-visible value must remain explicit so the VM can prefer
  aggregate or streaming work without trusting a provider to make the economical choice unaided.
- [ ] Extend bounded `file-slice`/`file-size` and host-issued cursors with workbook cursors so large
  Excel workbooks can be processed incrementally without whole-file/string loading. Line cursors
  and bounded `csv-open`/`csv-next`/`csv-close` record cursors now cover UTF-8 CSV quoted-record
  framing; workbook-specific opaque resources remain.
- [ ] Replace the compatibility output adapter with portable output-handle effects: default response
  port append (`say`), append/replace/status/progress/complete/fail operations, per-handle ownership
  and generation checks, and journal-first projection into concurrent shadow-buffer WorkUnits. Provider
  wire effects and the provider `submit_program` tool now cross the client's ordered REPL event bus
  before any WorkUnit mutation, and explicitly entered typed programs run as background ProgramRuns
  whose completion returns through that same loop. The process-wide string-only output callback has
  been removed; live output is now exclusively a per-run typed `(execution_id, sequence)` envelope.
  Client projections reject duplicates and retain a gapped in-memory suffix until its prefix arrives,
  but this is only a live reconnect guard. The application-side `VmEffectDeliveryLog` now provides
  durable idempotent JSONL append, per-client prefix acknowledgement, ordered suffix replay, and
  fail-closed corruption/gap checks; bind it to the converged Brain/client identity and projection
  lifecycle before treating reconnect replay as complete.
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
- [ ] Add real compilation modules after self-contained scripts and task/session/project/user
  vocabulary lifetimes are reliable: typed namespaces, explicit imports/exports, immutable module
  identities, and separately linkable interfaces/IR. Never implement imports as C-style textual
  inclusion or source concatenation, and never make an import implicitly execute initialization
  effects. Keep promotion to project/user/published vocabulary a separate authority-bearing,
  reviewable operation.
- [ ] Later, add deterministic decentralized dependency resolution. An import or module manifest
  must identify a package source and exact version or immutable content hash; a checked-in lockfile
  pins the complete transitive graph. Support local paths plus Git and HTTPS/content-addressed
  sources so Finch does not require a centrally operated registry. A future registry may provide
  discovery, metadata, mirrors, and caching, but is never the root of identity or authority.
  Fetch/cache and verify packages before compilation; prevent dependency confusion, reject
  undeclared mutable resolution, do not run ambient install scripts, and do not grant runtime
  capabilities merely because a module was imported.
- [ ] Adapt discovered MCP client tools into versioned namespaced typed VM bindings with schema
  validation, managed JSON fallback, parameter-bounded `mcp.call` capability grants, and normal
  suspension/resume; keep MCP transport lifecycle host-owned rather than a VM subagent protocol.
- [ ] Finish the persistent `ProgramRuntime` state model. Lisp and Co-Forth already share one
  persistent typed stack and dictionary, exposed by one inspection/revision boundary. Successful
  revisions now retain a serializable, reverified stack-and-definition checkpoint whenever they
  contain no host-owned handles; authority is intentionally not serialized. Named-Brain
  compatibility execution now journals content-addressed typed checkpoints and restores them after
  daemon restart without replaying source or effects. A versioned `ProgramRuntimeArchive` now
  validates and restores the complete reducible revision lineage while excluding grants, pending
  calls, and execute-once effects. `ProgramRuntimeArchiveStore` now atomically persists that archive
  with a SHA-256 integrity envelope and restores it only after current-verifier validation, while
  still refusing to persist authority or live handles. Bind the store to the converged application
  lifecycle, then add the managed heap and explicit host-handle restoration before treating every
  Brain run as restartable.
- [ ] Specify and test VM memory ownership and retention before introducing a managed heap. Current
  immutable `TypedValue` trees, call-frame windows, failed working snapshots, and continuations are
  acyclic Rust-owned values and are reclaimed when their owning root is dropped; preserve that cheap
  region/ownership path. Inventory every durable root (persistent stack and definitions, closure
  captures, suspended runs, producer fibers, host registries, revision archives, and future CTFE
  memo tables), give each an explicit release/retention/compaction policy, and add long-running
  bounded-memory tests. Completed/cancelled fiber tombstones, abandoned suspensions, and complete
  revision checkpoints must not grow forever. If shared or cyclic language objects are later
  admitted, put them behind generation-checked managed handles and choose a checkpoint-aware tracing
  scheme from measured workloads; do not add a global collector merely for acyclic temporary values.
- [ ] Complete revisioned private working snapshots and conflict-aware commits. A `ProgramRuntime`
  now executes each ProgramRun on a cloned stack/dictionary snapshot and gates only snapshot/commit;
  stale resume checks and losing post-resume commits return structured failed outcomes without
  overwriting the winner, retaining their emitted-effect journals rather than replaying or
  compensating them. Still add durable revision history, structured outcomes for host-effect
  correlation/revocation races, and reviewed merge rules where a commutative delta can safely be
  accepted.
- [x] Remove the native Lisp evaluator, effectful Lisp standard library, Lisp-to-Forth text
  compiler/fallback, source-text effect inference, and selectable legacy backend labels. Lisp now
  retains only its neutral reader/types and lowers directly into the shared typed IR; named-Brain
  Program/Prompt compatibility endpoints and `submit_program` execute the same `ProgramRuntime`.
  The outer tool gate no longer trusts the submitted coarse `ExecutionEffect`, optional exact
  `declared_capabilities` is only a checked upper-bound assertion, and `ExecutionOutcome` reports
  only `typed_vm`. This is the completed one-semantic-runtime milestone; do not reintroduce a native
  evaluator as a compatibility fallback.
- [ ] Finish legacy-runtime cleanup after migration evidence is acceptable. Project older persisted
  `lisp_env` rows into the typed program registry and remove that obsolete table/API. Migrate or
  retire the remaining legacy Co-Forth proof/library, grammar, channel, POSIX/IPC, and stack-console
  call sites identified by `finch library audit-typed`; keep them explicitly quarantined from the
  provider/runtime ABI until then. Remove compatibility aliases and duplicate entry points only
  after their useful behavior has a typed equivalent and replayable conformance coverage.
- [ ] Complete provider language packages, structured shadow-buffer outcomes, rollback/security
  tests, concurrency tests, and provider conformance tests. Manual configured-cloud smoke checks on
  2026-08-23 successfully executed provider-emitted Lisp `say`, Lisp arithmetic, and Co-Forth
  response programs through the raw wire receiver. A 2026-08-24 named-Brain smoke check attached
  two live consoles, shared a Lisp definition between them, restored it across daemon restart, and
  had configured Grok emit Co-Forth that invoked the restored word. This is useful integration
  evidence, not a substitute for fixed multi-provider conformance fixtures or recovery-rate
  measurements. A 2026-08-24 gate audit passes the complete current `cargo test` target set (the
  library alone is 2,395 passed, 0 failed, 7 ignored after captured Co-Forth quotation parity), and a rebuilt configured-Grok one-shot
  `hello finch` smoke test produced and executed raw Lisp after the VM contract was made persistent
  across tool-result continuations. Keep the unchecked gate items unchecked until their missing
  semantics and fixed cross-provider measurements exist. Do not require the later Cranelift JIT
  optimization tier to begin Brain convergence.
- [ ] After interpreter semantics, parametric HIR, CTFE, monomorphization, and Cranelift differential
  gates are stable, add a separate `finchc` AOT target. It should emit either a pure standalone
  executable with a minimal runtime or a capability-hosted executable linked to the portable
  `VmSideEffect`/`VmResume` ABI. Reuse the same frontends, semantic scheduler, verifier, source maps,
  and compile-time type/syntax reflection; do not create a second metaprogramming language or accept
  model-authored CLIF/native artifacts as trusted input. Make host selection explicit: a `none`
  profile rejects unsupported effects, a terminal wrapper may project `session.emit` to stdout, and
  portable/library output exposes or links the structured effect/resume shims. `say` must retain one
  semantic meaning rather than becoming a different primitive in compiled programs. Add a standard
  async host profile for executable applications: the linked runtime owns its poller/event loop and
  maps opaque generation-checked listener/socket/file resources to OS descriptors internally. A
  compiled web client/server uses typed connect/listen/accept/read/write/close effects and suspends
  through the same continuation ABI; ordinary Finch code never receives or forges a raw descriptor.
  Deliberate low-level descriptor/FFI work remains an unsafe native extension with a separately
  declared capability and ABI, not an implicit privilege of AOT compilation. Define the linked
  scheduler as a replaceable reactor interface: Finch-owned service loops and host-owned Cocoa,
  Win32, GTK, game, or C loops provide the same timer/readiness/wakeup contract, and native callbacks
  enqueue typed resumptions rather than re-entering VM frames. Add versioned typed C `extern`
  declarations and generated interpreter/Cranelift shims with explicit layout, ownership, callback
  lifetime, thread-affinity, and effect metadata; raw pointers, variadics, and unchecked descriptor
  access require a distinct unsafe-FFI capability.
- [ ] Make native performance and application expressiveness measurable release gates. For
  monomorphized effect-free compute, parsing, collection, and control-flow kernels, compare optimized
  Finch against checked-in equivalent optimized Rust/C++ using wall time, allocations, peak memory,
  code size, compile latency, and boundary overhead; keep missing vectorization, escape/alias
  analysis, bounds-check elimination, and range fusion visible rather than declaring victory when
  Cranelift emits code. Maintain application-sized Lisp/Co-Forth fixtures demonstrating the same
  structural records, variants, closures, parametric functions, concepts, modules, derivation, async
  resources, and range composition expected from a TypeScript-class application language, without
  relying routinely on `dynamic` or giving either frontend privileged semantics.
- [ ] After the parametric compiler and AOT service ABI stabilize, make the frontend and semantic
  scheduler self-hosting candidates. Preserve a small trusted Rust stage 0 for framing, verified
  artifact loading, IR verification, effects, and execution; expose the Finch compiler as a
  versioned typed service and optionally generated C-compatible `libfinch_compiler` facade. Pin a
  content-addressed bootstrap artifact and require stage-0→stage-1→stage-2 reproducibility over
  normalized IR/native output with complete source/module/runtime/target hashes. Compiler modules
  must use the same public syntax/type/CTFE APIs as other Finch code rather than gaining a hidden
  metaprogramming path.
- [ ] Freeze and test the Runtime/Application boundary: the embedder-neutral typed VM exposes only
  verified execution, diagnostics, capability requests, and idempotent side-effect/resume records;
  the Finch application supplies Brain, UI, approval, provider, MCP, scheduler, and OS adapters.
  When the later Brain gate opens, make the normal local frontend/daemon path a versioned Cap'n
  Proto projection of those structured records (including attachment cursors and effect/resume
  correlation), not a free-form JSON event bus; HTTP/WebSocket remain remote adapters.
- [x] Complete the fiber/task split: `(defer :cpu (lambda () ...))` / `defer-cpu` has private-stack,
  immutable-capture CPU work with typed `task<T>` poll/join/cancel operations. A running join
  suspends the parent VM continuation rather than blocking the event loop; CPU fibers reject
  effects and never share their parent stack. Repeatedly-yielding fibers remain separate from
  subagents, and bidirectional resume remains deferred until a concrete need exists.
- [x] Implement a typed lazy sequence protocol: host-backed `stream<T>` handles now provide
  bounded `stream-next -> option<T>` and `stream-close`, with
  ProgramRun ownership/generation checks, path-scoped capability propagation, concrete polymorphic
  host-result rows, and shared Lisp/Co-Forth lowering. File-line and CSV streams are the first
  backends.
- [x] Complete first-class producer fibers on the shared typed `yield` control effect. `yield` now
  publishes one typed value beside the exact serializable continuation, pending-run inspection
  exposes it, and unit yields retain automatic cooperative-timeslice behavior. Callable and closure
  signatures now retain the inferred `yields<Y,unit>` contract, the independent verifier rejects a
  hidden or inconsistent suspension, and published words preserve it in vocabulary introspection.
  `defer` now exposes a pure, zero-argument yielding closure as `fiber<Y,R>` through ordinary
  registry-backed `fiber-next`, `fiber-join`, and `fiber-cancel` words. `fiber-next` returns
  `result<Y,variant{end(R)}>` (`ok(Y)` for a yield, `err(end(R))` for terminal return); the same
  types and operations are available from Lisp and Co-Forth. Producer continuations and handles
  survive VM checkpoints, participate in ProgramRun rollback, and retain deterministic completed,
  failed, and cancelled states. `stream<T>` stays the range abstraction for
  cursor-backed data, while producer fibers supply user-defined ranges. No special multi-return,
  hidden iterator protocol, compiler-only map lookup rule, or untyped resumed value is permitted;
  all scheduler operations must come from the same typed registry/templates available to user
  definitions. This first version resumes producers with `unit` and permits only pure closures;
  effectful autonomous work remains a separate task/agent protocol. Legacy Co-Forth generators
  remain outside typed-runtime vocabulary.
- [ ] Make resource roots first-class capability objects. Workspace/project paths remain safely
  relative; an intentional full-machine grant is a separate audited host root, never ambient
  authority inferred from an absolute path string. `host-path` and distinct
  `host-file-read`/`host-file-write` now require an explicitly installed host binding and recheck
  canonical containment at every call; keep workspace `path`/`file-read` structurally separate.
  Still add project/task-output bindings, persisted approval/revocation lifecycle, and the host UI
  for deliberately binding `/` as whole-machine scope.
- [ ] Phase 0: route existing provider streaming through the portable VM side-effect journal and
  per-ProgramRun output-handle bindings; test replay/reconnect and concurrent WorkUnit projection,
  then publish the incompatibilities found by the non-executing `finch library audit-typed`
  report before requiring typed output. The audit command and diagnostic-code grouping are now in
  place, and the first machine-local library baseline is recorded in
  `docs/TYPED_VM_MIGRATION_AUDIT.md`; still run it against representative project/session corpora
  and retain those reports as migration evidence.
- [ ] Later: define a signed, content-addressed vocabulary package protocol for pushing reviewed
  `published` definitions between Finches; verify source/IR, dependencies, certificates, provenance,
  and local capability policy before installation.

## TUI and tool presentation

- [ ] Batch model-authored edits into one explicit multi-file changeset/proposal and request one
  approval before applying it to the real workspace. Keep the familiar model-facing edit/write
  tools, but make them target a per-run proposal overlay by default: later reads, searches, and test
  processes in that run see the overlay, while the user's workspace remains unchanged. Multiple
  edit calls are internal proposal events: update one live summary WorkUnit instead of committing a
  stream of intermediate per-file diffs to scrollback, then present the aggregate multi-file diff at
  the review boundary. User comments and requested changes resume the proposal's agent/run against
  the same executable overlay and create a new proposal revision; accept applies the final complete
  changeset atomically, reject discards it, and `$EDITOR` co-editing is optional rather than the only
  review UI. Use a Finch-owned `GIT_EXTERNAL_DIFF` adapter to turn each tracked-file
  comparison from one bracketed `git diff` invocation into typed diff records associated with the
  same proposal ID; explicitly include untracked creations because Git does not send them through
  the external-diff hook. The proposal coordinator—not the environment variable—owns atomic
  apply/reject/request-changes, stale-base/three-way-conflict handling, and final aggregate counts.
  Immediate real-workspace writes require an explicit task/session autonomous-write policy grant,
  so users can deliberately choose delegation without making disengagement the default UX.
- [ ] Render edit/write tool results as structured diffs rather than unstyled text: show a stable
  `Edited path (+added -removed)` title, retained context and line numbers, green backgrounds for
  additions, red backgrounds for removals, and neutral styling for unchanged context. Feed typed
  styled lines through `OutputManager`/shadow-buffer projection so live redraw, scrollback, copy,
  resize, and concurrent WorkUnits remain stable; never print raw ANSI directly from a tool.
- [ ] Add syntax highlighting to structured code and diff bodies as a separate layer after diff
  semantics are correct. Select the grammar from the path/language metadata, compose token color
  with added/removed backgrounds legibly across themes, and fall back to plain code without
  changing layout when the language is unknown.

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
- [x] Finish remote named-brain attach/detach, live scrollback replacement, status display, and
  prompt/Forth/Lisp routing through the daemon-owned event stream. The WebSocket now begins with an
  atomic snapshot/live subscription, attached clients render typed source and output distinctly,
  and one per-Brain daemon turn lane serializes concurrent consoles against the authoritative VM
  revision. A two-console test on 2026-08-24 also passed restart/checkpoint restoration and a real
  configured-Grok prompt. A subsequent live Grok test preserved an invalid first Lisp program and
  its `E-TYPE-006` result, accepted exactly one source-only correction, produced the expected value,
  and restored the shared definition after another daemon restart. Durable per-client
  acknowledgement cursors and scoped participant roles remain separate items below.
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
- [ ] Revisit shared channels only after the Brain event log, runner lease, and participant-role
  model are complete. The eventual channel should be a threaded/multi-participant projection of a
  Brain: people and models share one durable conversation, while programs run only on the remote
  environment-owning runner. Adding/removing VM definitions and borrowing CPU require explicit,
  attenuated participant grants. Until then, quarantine the aspirational IRC/room/peer/gas command
  surfaces (`/join`, `/part`, `/say`, `/room`, `/connect`, and related commands) rather than
  presenting them as a second collaboration protocol.

## Client and model integration

- [ ] Keep the Brain event log and validated VM checkpoints authoritative while optionally retaining
  an expendable remote provider-continuation cursor keyed by Brain, provider identity, model, and
  language-package hash. When an API supports incremental continuation, send only new user/tool
  events; rebuild a fresh remote chain from the local checkpoint/log after expiry, rejection,
  provider/model change, or compaction. Never treat provider-side application state or prompt-cache
  residency as durable Brain state. BOOT is an invocation contract rather than remembered dialogue:
  attach the current BOOT/instruction capsule on every inference for APIs whose continuation cursor
  does not inherit instructions, and start a new chain whenever its package hash changes.
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
