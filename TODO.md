# Finch TODO

This is the short, discoverable work queue. Detailed rationale and protocol sketches live in
[`docs/TYPED_LISP_FORTH_CAPABILITY_JIT_PLAN.md`](docs/TYPED_LISP_FORTH_CAPABILITY_JIT_PLAN.md),
[`docs/SHARED_PROGRAM_RUNTIME_PLAN.md`](docs/SHARED_PROGRAM_RUNTIME_PLAN.md), and
[`docs/BRAIN_CONVERGENCE_PLAN.md`](docs/BRAIN_CONVERGENCE_PLAN.md).

## Typed Lisp/Co-Forth VM — working substrate and ongoing language evolution

The product-critical Brain entry gate passed on 2026-08-24: both frontends lower directly to the
shared verified runtime, production words come from the typed registry, provider wire programs and
bounded repair execute end to end, reducible checkpoints restore across daemon restart, two
consoles share committed definitions, and the complete current test target set passes. Unchecked
items below remain real VM/language work, but advanced CTFE, generics, mixed syntax, richer records,
JIT/AOT, and future coroutine policy no longer prevent testing and converging Brains on the working
runtime. Any change that would create a second execution, authority, checkpoint, or event lifecycle
still blocks the corresponding Brain phase until it is unified.

- [ ] Make "statically safe, scripting-language feel" a conformance requirement for both source
  languages. Infer literal, local, parameter, return, stack-row, effect, yield, and generic
  instantiation types wherever the program determines them; private definitions should normally
  need no annotations, while publication freezes an inferred or explicitly declared stable
  interface. Use concepts, overload resolution, parameter packs, ranges, and bounded CTFE to make
  ordinary collection/data/application code feel as direct as Python or JavaScript without
  introducing routine `dynamic`, implicit unsafe coercions, or a slower alternate execution path.
  Require annotations only at genuinely ambiguous, recursive-interface, module/API, refinement,
  capability-selector, and FFI boundaries. Track annotation density, first-pass model success,
  compile latency, and diagnostic locality in the cross-provider corpus. Inference must be
  directional and bounded: an initializer establishes a binding's type, then later uses are checked
  against it. Never propagate an expected callee/result type backward to reinterpret an established
  binding or blame its initializer; generic substitution flows forward from supplied arguments.
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
  parsing. Source order must not become a user-facing consequence of this rule: register every
  top-level declaration skeleton during that one parse, then let the semantic scheduler resolve
  forward references on demand. Require explicit signatures only for exported interfaces,
  genuinely ambiguous inference, or dependency cycles that cannot otherwise become
  `SignatureReady`; measure those cases in corpus replay. Keep the
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
- [ ] Unify macros, templates, derivation, compile-time `if`/`foreach`, concepts, and reflection as
  ordinary pure bounded Finch CTFE over first-class `syntax`, `type`, constraint-evidence, and
  module/symbol-reference values. `syntax` must retain origin/expansion ancestry, lexical scope
  marks, and stable symbol identity so transformation functions remain hygienic and diagnostic-rich;
  do not add a privileged macro evaluator, string mixins, or a parallel template language.
  `define-syntax` may be sugar for registering a `syntax -> syntax` CTFE function. Keep classic
  S-expressions as the canonical structural Lisp reader, while allowing a later lighter
  expression/indentation reader that immediately produces the identical syntax tree. Reader sugar
  must disappear before expansion/elaboration and must not create a second semantic path. Add
  full nested `quote`, `quasiquote`, `unquote`, and splice support over first-class `syntax` values;
  replace the current symbol-only `quote` restriction, and test that quoted calls remain data while
  unquoted calls execute. Preserve spans and hygiene through every nested quoted form. Add
  normalization conformance fixtures proving that each sugared program and its canonical
  S-expression produce structurally identical `syntax` modulo spelling-specific source origins,
  then identical elaborated HIR/IR. Do not brand or implement the notation as a third language.
  Treat every statically known signature, effect/yield row, schema, parameter pack, symbol/module
  reference, and structural-concept result as an ordinary immutable CTFE value. Resolve a concept
  once against the concrete type's resolved interface and cache `ConceptEvidence` containing its
  derived type bindings and concrete operation word IDs; do not repeatedly probe by speculatively
  compiling expressions. Structural matching may bind explicit output variables such as
  `T : Map<K,V>, infer K, infer V` or
  `F : fn(Args...) -> R ! E, infer Args, infer R, infer E`. Resolution remains bounded and
  directional: explicit generic arguments, ordinary argument types, concept evidence and derived
  outputs, remaining validation, then memoized specialization. Ambiguous evidence is a diagnostic,
  never an import-order-dependent winner.
- [ ] Make ordered type/value parameter packs a normal part of source-defined generics. Pure bounded
  CTFE must be able to query pack length, index/destructure it, inspect each type/value pair, and use
  ordinary compile-time `foreach` to generate a concrete fixed-arity specialization or an explicit
  homogeneous typed range. Lisp and Co-Forth must expose identical pack semantics, and user-defined
  functions must have the same type-safe variadic power as core/library words. Keep raw C ABI `...`
  and `va_list` behind the separately authorized unsafe-FFI boundary; safe wrappers may use packs but
  must never turn Finch values into implicit `void*` variadics.
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
  Keep `match-result`/`if-ok` for expected recovery and `unwrap` as a deliberate diagnostic trap;
  do not silently replay host effects.
- [ ] Add statically declared/inferred exception effects for failures whose intermediate callers
  have no recovery policy. `throws<E>` participates in the one canonical effect row, propagates
  transitively through ordinary calls, and typed handlers subtract only variants they handle;
  unchecked Java-style exceptions are not permitted. Keep `option<T>` for ordinary absence and
  `result<T,E>` for expected alternatives. Cancellation is an unwind reason but is not ordinarily
  catchable, and verifier traps remain diagnostics rather than source-level exceptions.
- [ ] Add lexical scope guards (`on-exit`, `on-success`, `on-failure`) as serialized once-only guard
  records owned by a lexical frame/scope. They run in reverse registration order on actual scope
  exit, exception propagation, cancellation, or trap as appropriate, but never merely because the
  execution yields or awaits. Permit explicit dismissal/commit. Guards may perform explicit
  compensating actions, but must not imply rollback of journaled external effects or conceal the
  original unwind reason.
- [ ] Standardize declaration attributes as ordinary namespaced compile-time metadata and bounded
  `syntax -> syntax` transforms. Built-in and user-defined attributes use the same `@name(...)`
  lookup, reflection value, hygiene, and wrapper contract; no D-style mixture of magic bare
  attributes and second-class user annotations. Inputs/outputs and the canonical `! EffectRow`
  remain callable type structure rather than attributes. Omitted `!` means an inferred empty row.
  Derive `pure`, `total`, `deterministic`, and `non-suspending` independently from the resolved row
  and body: a deterministic `throws<E>` function may be pure but not total, while yielding remains
  a scheduling barrier. Expose these predicates as ordinary concept constraints, not attributes or
  effect members.
- [ ] Generalize concrete host capability enum cases into versioned namespaced capability
  descriptors with stable unforgeable identity, typed selector/schema metadata, containment, and
  host binding. User modules may define abstract effects, attributes, wrappers, handlers, and
  attenuation, but cannot mint host authority by declaring a matching name. Normalize capability,
  exception, suspension, mutation, and other observable effects into one reflectable row while the
  broker selects only capability-bearing members for authorization. Preserve the existing broker
  and generated registry during migration. Spell source rows as `! fs.read<R> | throws<IoError> |
  yields<Y,Resume>` and canonicalize member order; these become typed effect data rather than
  privileged parser syntax.
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
  The ignored `live_parity_finch_wire_programs` suite now executes eight fixed response, multiline-
  quoting, arithmetic, recursive-definition, closure, producer-fiber, loop, and typed-record tasks
  through the real typed runtime, performs at most one source-only repair with a 60-second per-call
  deadline, and
  prints source-free first-pass/repaired/terminal counts for every configured provider. On
  2026-08-24 the configured Grok profile completed all 8 language-package tasks first-pass
  (0 repaired, 0 terminal); other provider/model profiles remain unmeasured. A BOOT-only trial
  passed the four basic tasks but emitted textual introspection requests for closures and fibers
  because that source-only fixture exposed neither tools nor the language definitions. Supplying
  the canonical VM/Lisp/Forth package made both advanced cases pass; preserve that distinction
  rather than misclassifying a missing test affordance as a model or VM failure.
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
  `agent-spawn-with` now accepts bounded typed `{kind,id,sha256}` context references and poll/await
  expose a deterministic starting-context hash bound to task/background/budgets, ancestry, selected
  provider/model, VM revision/generation, references, and inherited grants. Explicit capability
  subsets now use opaque host-issued `resource<capability-grant>` values discovered within the
  current ProgramRun ceiling; child creation rechecks live policy/scope/revocation and parent
  containment, while printed grant IDs and requirement JSON remain non-authoritative metadata.
  Child context references now resolve through an injectable immutable store before task creation,
  with per-item/aggregate bounds, UTF-8 validation, byte-for-digest verification, identity-rebinding
  rejection, and verified content materialization. Persist that store with the owning Brain and wire
  artifact-producing host paths to register content instead of accepting caller-invented references;
  also audit remaining root/provider legacy model-selection/tool entry points before closing this gate.
- [ ] Define a compact, discoverable data-work vocabulary before asking models to synthesize their
  own large-file loops: workspace tree metadata, bounded file hash, a bounded host-computed
  directory Merkle root, bounded host-computed CSV header/per-column summaries, workbook sheet-name
  discovery, typed workbook row cursors, 10,000-cell rectangular workbook slices, and bounded
  workbook header/per-column summaries now exist. Build security/integrity inspection from these
  explicit bounded facts
  (inventory, metadata, hashes, rules/signatures, provenance), with any remediation remaining a
  separately authorized proposal/effect. Each contract must advertise result shape and byte/work
  bounds; bulk materialization into a model-visible value must remain explicit so the VM can prefer
  aggregate or streaming work without trusting a provider to make the economical choice unaided.
- [ ] Extend bounded `file-slice`/`file-size` and host-issued cursors with workbook cursors so large
  Excel workbooks can be processed incrementally without whole-file/string loading. Line cursors
  and bounded `csv-open`/`csv-next`/`csv-close` record cursors now cover UTF-8 CSV quoted-record
  framing. `workbook-open` and `workbook-sheet-open` now return the same opaque
  `stream<list<string>>` shape, `workbook-sheets` returns structured names, only one row crosses into
  the VM per `stream-next`, and ordinary ownership/generation/close checks apply. The current
  Calamine adapter retains an owned decoded worksheet behind the host cursor with a 10-million-cell
  bound; replace it with a genuinely streaming or bounded-range decoder before closing this gate.
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
- [ ] Preserve the useful poset idea as a typed ephemeral execution-plan artifact, independent of
  the deleted semiotic stack language. A plan contains immutable node IDs, typed Finch
  `ProgramSubmission` or host-operation specs, declared input/output rows, dependency edges,
  inferred capability envelopes, budgets, and source origins. Present the whole DAG for human
  review/edit/approval before scheduling; hash the approved form so execution cannot substitute
  nodes afterward. Ready nodes may run concurrently as ordinary ProgramRuns/fibers, and their typed
  results satisfy successor inputs without an LLM-only `Call` convention. The current executor now
  runs reviewed Lisp/Co-Forth nodes only through `ProgramRuntime` and no longer receives the legacy
  shared stack; replace its remaining string labels/results and direct tool registry with this typed
  plan contract. Keep plans ephemeral unless an explicit promotion persists a reusable procedure.
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
- [ ] Make every proposal review expose explicit `accept`, `reject`, and `request changes` actions
  in both keyboard navigation and the typed decision result. Reject must resume the exact suspended
  proposal with a cancellation/denial value, never execute accepted-looking source, and remain
  available even when `$EDITOR` is unset, exits unsuccessfully, or the provider proposed Bash.
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
- [x] Adapt discovered MCP client tools into versioned namespaced typed VM bindings with schema
  validation, managed JSON fallback, parameter-bounded `mcp.call` capability grants, and normal
  suspension/resume; keep MCP transport lifecycle host-owned rather than a VM subagent protocol.
  The generic Lisp/Co-Forth `mcp-call` boundary and generated `mcp.<server>.<tool>` words derive and
  recheck exact server/tool selectors, dispatch through the application-owned MCP client, and
  return managed raw MCP JSON. Supported input schemas become typed records; unsupported valid
  schemas fall back conservatively to `json`; admitted output schemas validate
  `structuredContent`. Schema hashes version each binding, unsafe or excessive names/schema shapes
  are rejected, and third-party descriptions remain explicitly bounded untrusted metadata.
  Startup, `/mcp refresh`, `/mcp reload`, and daemon-owned named-Brain runtimes all install the same
  discovered vocabulary without manufacturing grants. A deterministic stdio fixture covers generic
  Lisp, typed namespaced Lisp, typed namespaced Co-Forth, raw results, and concrete authority.
- [ ] Finish the persistent `ProgramRuntime` state model. Lisp and Co-Forth already share one
  persistent typed stack and dictionary, exposed by one inspection/revision boundary. Successful
  revisions now retain a serializable, reverified stack-and-definition checkpoint whenever they
  contain no host-owned handles; authority is intentionally not serialized. Named-Brain
  compatibility execution now journals content-addressed typed checkpoints and restores them after
  daemon restart without replaying source or effects. A versioned `ProgramRuntimeArchive` now
  validates and restores a bounded recent reducible revision window while excluding grants, pending
  calls, and execute-once effects. The window records its base revision, retains at most 256 full
  checkpoints, and continues monotonic revision identity after restoration; older history belongs
  to the application event/checkpoint store. `ProgramRuntimeArchiveStore` atomically persists that archive
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
  bounded-memory tests. Cooperative producer records are now traced from every nested typed value
  on the committed stack and transitively through live continuation stacks, locals, captures, and
  terminal results at each successful commit: an unreferenced terminal record is reclaimed, while a
  duplicate or captured handle keeps its deterministic tombstone alive. CPU-task records now use
  per-runtime-snapshot owner leases: cloning a private transaction retains its reachable nested task
  handles, consuming the handle releases only that snapshot, failed transactions reclaim tasks they
  alone spawned, and the final release removes the tombstone or cooperatively cancels an unobserved
  worker. Private host suspensions are now hard-capped at 256 per `ProgramRuntime`; existing human
  approvals are never silently evicted, while a newly suspending run at capacity is cancelled with
  its complete structured outcome and resource cleanup. Reducible revision history is likewise
  bounded to the most recent 256 full checkpoints with an explicit archive base revision. Still
  bind an explicit participant/time expiry policy and any longer historical retention to the
  application event/checkpoint log.
  If shared or cyclic language objects are later admitted, put them behind generation-checked
  managed handles and choose a checkpoint-aware tracing scheme from measured workloads; do not add
  a global collector merely for acyclic temporary values.
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
- [ ] Finish legacy-runtime deletion after migration evidence is acceptable. Public `: ... ;` and
  `/forth` source execution now enter only `ProgramRuntime`, and the old dictionary undo shortcut is
  gone. Project older persisted `lisp_env` rows into the typed program registry and remove that
  obsolete table/API. Port useful proof/library behavior to typed words, then delete the remaining
  semiotic Co-Forth grammar, channel, POSIX/IPC, peer-demo, and stack-console interpreter call sites
  identified by `finch library audit-typed`. Until deletion, keep that implementation explicitly
  internal to those named subsystems: it is not a supported language, compatibility runtime, or
  fallback, and no new public aliases may enter it. The 2026-08-24 report-only baseline found
  3,225 stored entries: 160 accepted, 29 missing, and 3,036 rejected (2,988 `E-LINK-002`, 31
  `E-TYPE-002`, 12 `E-STACK-001`, and five other reader/signature failures). Preserve this baseline
  for comparison; it is migration evidence, not a reason to make legacy execution public again.
- [ ] Complete provider language packages, structured shadow-buffer outcomes, rollback/security
  tests, concurrency tests, and provider conformance tests. Manual configured-cloud smoke checks on
  2026-08-23 successfully executed provider-emitted Lisp `say`, Lisp arithmetic, and Co-Forth
  response programs through the raw wire receiver. A 2026-08-24 named-Brain smoke check attached
  two live consoles, shared a Lisp definition between them, restored it across daemon restart, and
  had configured Grok emit Co-Forth that invoked the restored word. This is useful integration
  evidence, not a substitute for fixed multi-provider conformance fixtures or recovery-rate
  measurements. A 2026-08-24 gate audit passes the complete current
  `cargo test --all-targets --no-fail-fast` target set after public legacy execution-path deletion
  (the library alone is 2,327 passed, 0 failed, 7 ignored), and a rebuilt configured-Grok one-shot
  `hello finch` smoke test produced and executed raw Lisp after the VM contract was made persistent
  across tool-result continuations. Keep the unchecked gate items unchecked until their missing
  semantics and fixed cross-provider measurements exist. Do not require the later Cranelift JIT
  optimization tier to begin Brain convergence.
- [ ] Run report-only replay over retained real provider/model outputs before tightening source
  rules further. Publish first-pass parse/verify success, repair success/attempts, raw-prose and
  Markdown leakage, invented words, annotation needs, forward-reference/source-order patterns,
  inferred capability mismatches, language choice, and tokens per successful ProgramRun by
  provider/model. Keep the corpus immutable and versioned so syntax and prompt changes can be
  compared rather than judged from interactive anecdotes. Opt-in capture is now implemented for
  interactive, one-shot, and named-Brain provider responses: setting `FINCH_WIRE_CORPUS_PATH`
  appends locked, source-hashed version-1 JSONL records for both first-pass and repair attempts,
  separately from the source-free aggregate metrics. `finch wire-corpus audit <file> [--json]`
  validates record hashes and compiles/verifies every retained Lisp/Co-Forth source without
  creating a `ProgramRuntime` or executing effects. Captures now retain the exact provider/model
  and a reducible compiler context containing promoted typed functions, while excluding operand
  stacks, grants, pending effects, and host resources; a regression test verifies replay of a
  program that calls a previously promoted word. Still collect and freeze a representative
  multi-provider corpus, extend that compiler context to future module/import/package identities,
  add token/latency metadata, and expand the reproducible reports. The first source-free checked-in
  smoke artifact is `docs/conformance/2026-08-24-grok-code-fast-1.json`: its private captured
  corpus replayed 11/11 programs successfully under manifest 1 / VM type system 5. That includes
  eight isolated language tasks, two turns sharing a committed typed word, and one deterministic
  repair from raw prose plus the real structured diagnostic. Its eleven submissions and one
  provider are deliberately documented as insufficient to close this gate.

### Separate language/compiler research track — not a Brain convergence prerequisite

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
  monomorphized effect-free compute, parsing, collection, and control-flow kernels, compare Finch
  against checked-in interpreter, Cranelift, optimized Rust, and C++ baselines using wall time,
  allocations, peak memory, code size, compile latency, and boundary overhead. Cranelift is the
  fast baseline backend, not a promise of Rust-class loop/vector/alias optimization. Treat
  Rust/C++-class output as a separate long-term optimizing-backend/compiler-research target and do
  not put it on the Finch Runtime or Brain roadmap until a backend capable of it is selected.
  Maintain application-sized Lisp/Co-Forth fixtures demonstrating the same
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

### Runtime completion gate continues

- [ ] Freeze and test the Runtime/Application boundary: the embedder-neutral typed VM exposes only
  verified execution, diagnostics, capability requests, and idempotent side-effect/resume records;
  the Finch application supplies Brain, UI, approval, provider, MCP, scheduler, and OS adapters.
  When the later Brain gate opens, make the normal local frontend/daemon path a versioned Cap'n
  Proto projection of those structured records (including attachment cursors and effect/resume
  correlation), not a free-form JSON event bus. Carry the same schema in binary WebSocket frames for
  remote clients so local and remote paths reuse message definitions and conformance fixtures.
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
- [ ] Converge producer fibers, future bidirectional coroutines, compiler semantic waits, and
  scheduler-owned green tasks on one private `ResumableExecution<Y,Resume,R>` state machine. The
  primitive owns verified frames, a private operand stack, locals/captures, PC, transaction/effect
  prefix, and single-resumer lifecycle; suspension propagates through ordinary calls. Generalize
  callable metadata from `yields<Y,unit>` to `yields<Y,Resume>` and `yield : Y -> Resume` without
  making source programs routinely dynamic. `defer` must reify/transfer ownership of that same
  execution record into a handle rather than implementing another continuation format. Keep
  generator pull, explicit fiber call/yield, green scheduling, async event handling, actors, and
  compiler `Needs(symbol,phase)` as inspectable library/host policies over the primitive. New
  resumable executions get explicit arguments plus immutable captures and a private stack; shared
  `cell`/atomic/mutex/channel resources remain an orthogonal, explicitly typed feature.
- [ ] Make resource roots first-class capability objects. Workspace/project paths remain safely
  relative; an intentional full-machine grant is a separate audited host root, never ambient
  authority inferred from an absolute path string. `host-path` and distinct
  `host-file-read`/`host-file-write` now require an explicitly installed host binding and recheck
  canonical containment at every call; keep workspace `path`/`file-read` structurally separate.
  Project and task-output bindings now have distinct typed path constructors and read/write words
  over application-installed roots. Still persist the root-binding approval/revocation lifecycle
  and add host UI for deliberately binding `/` as whole-machine scope.
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

- [ ] Add theme-aware author/channel backgrounds so user input, model-authored VM source, tool
  activity, and VM/user-visible output are distinguishable without relying on low-contrast cyan
  foreground text alone. Keep output backgrounds subtle, preserve WCAG-like foreground contrast in
  light and dark themes, emit plain text when copied, and make the styling part of structured
  `OutputManager` rows so redraw, resize, scrollback, and concurrent WorkUnits remain stable. In a
  shared Brain, assign stable distinguishable participant background accents from participant
  identity and label the author; do not make every human console look like the same local user.
- [ ] Give `$VISUAL`/`$EDITOR` a correct terminal-protocol handoff. Leave Finch raw/live rendering,
  enter a clean alternate screen for Vim and similar full-screen editors, restore terminal modes on
  every exit path, discard the editor screen instead of retaining its `~` rows in scrollback, then
  invalidate and redraw Finch's live region exactly once.
- [ ] Preserve visible turn ownership while input queues behind an active provider/repair/tool
  turn. A later user prompt must remain a distinct queued WorkUnit and cannot appear inside the
  earlier repair's source or tool block; program source, diagnostics, bounded repair, tool activity,
  and VM output must stay grouped under one correlated Brain turn until its terminal event.
- [x] Recompute the owned live-region geometry after terminal reflow before erasing on resize, so
  shrinking a terminal no longer leaves one historical separator row per resize event.
- [x] Queue VM-output completion behind every projected `say` event, so a WorkUnit cannot enter
  scrollback after its first chunk and silently lose later chunks from the same program.
- [ ] Batch model-authored edits into one explicit multi-file changeset/proposal and request one
  approval before applying it to the real workspace. Keep the familiar model-facing edit/write
  tools, but make them target a per-run proposal overlay by default: later reads, searches, and test
  processes in that run see the overlay, while the user's workspace remains unchanged. Treat this
  as a real materialized proposal workspace rather than only an in-memory interception layer:
  model-authored shell commands, editors, build scripts, and arbitrary subprocesses must receive the
  proposal tree as their working tree and must not receive the real workspace as a writable root.
  Back the platform-neutral snapshot interface with APFS `clonefile`/reflinks on macOS, OverlayFS or
  reflinks on Linux, ReFS block cloning where available on Windows, and a correct copy fallback.
  Isolate Git metadata/index state, seed the snapshot from the exact visible dirty and untracked
  workspace state, and enforce capability/OS-sandbox boundaries against absolute-path and `..`
  escapes. Multiple
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

## Typed stream/range library

- [ ] Build ordinary typed range combinators over `stream<T>` and producer fibers: `map`, `filter`,
  `take`, `drop`, `first`, `last`, `fold`, `collect`, `any`, and `all`, with bounded/fuel-aware
  variants for model-authored analysis. Reuse the same contracts for lists and host-backed streams
  through concepts/evidence rather than interpreter special cases. Make `tree-list` plus these
  words the discoverable typed replacement for routine `find | head`/`tail` shell pipelines, and
  add provider fixtures proving models choose the bounded native path for large trees and files.

## Shared brains and environments

- [x] Open the Brain convergence gate on the tested shared runtime without declaring the remaining
  language roadmap complete. The 2026-08-24 evidence is recorded above and in the runtime plan;
  future language features must extend the same runtime rather than creating a parallel Brain path.
- [ ] Execute
  [`docs/BRAIN_CONVERGENCE_PLAN.md`](docs/BRAIN_CONVERGENCE_PLAN.md): consolidate the three current
  Brain concepts into one daemon-authoritative event log, VM history, environment, and authority
  boundary; model interactive turns, speculative helpers, schedules, and subagents as `BrainRun`s;
  and make local, embedded, IPC, HTTP/WebSocket, and remote clients projections of one service.
- [x] Persist complete VM checkpoints or reversible VM deltas at committed program boundaries.
  Every committed local or leased-runner revision writes one content-addressed, schema-native
  Cap'n Proto `TypedRuntimeCheckpoint` beside the Brain event log. Restart restores and reverifies
  that closed graph rather than replaying source. Existing content-addressed `.json` checkpoints
  remain readable as migration input, but all new durable blobs use the exact runner-transport
  codec. Tests cover native and frontend-runner restart, legacy recovery, closure/fiber state, and
  rejection of ambiguous trailing data.
- [ ] Make the daemon own schedule definitions/due-time delivery only. Coalesce missed ticks into one
  pending event per schedule while the environment-owning frontend is unavailable; require explicit
  bounded catch-up and idempotency policy before delivering every missed occurrence.
- [x] Split reducible VM state from the execute-once host-effect journal. Runner callbacks now
  carry exact schema-native effect/state records independently of checkpoints on success, failure,
  and cooperative cancellation. The daemon persists each `(execution_id, sequence)` once before
  publishing a reducible checkpoint or final result, rejects conflicting identity reuse, and
  restores checkpoints without replaying file, process, dialog, network, or output effects.
- [ ] Add typed compensating actions for reversible effects. File undo must use preimage and
  postimage hashes plus a conflict-aware reverse changeset.
- [x] Finish remote named-brain attach/detach, live scrollback replacement, status display, and
  prompt/Forth/Lisp routing through the daemon-owned event stream. The WebSocket now begins with an
  atomic snapshot/live subscription, attached clients render typed source and output distinctly,
  and one per-Brain daemon turn lane serializes concurrent consoles against the authoritative VM
  revision. A two-console test on 2026-08-24 also passed restart/checkpoint restoration and a real
  configured-Grok prompt. A subsequent live Grok test preserved an invalid first Lisp program and
  its `E-TYPE-006` result, accepted exactly one source-only correction, produced the expected value,
  and restored the shared definition after another daemon restart. Attachments now also have
  durable daemon-owned acknowledgement cursors, role-bound submission, per-connection identity,
  conflict-safe reconnect/detach, and a status display derived from the authoritative role. A live
  driver/consultant test shared a Lisp definition, rejected consultant program submission, retained
  its cursor across reconnect, and observed a WebSocket close as a durable detach event.
- [ ] Replace the transitional JSON named-Brain lifecycle protocol with one versioned Cap'n Proto
  `BrainService` schema. Use Cap'n Proto RPC over the local Unix socket and ordinary Cap'n Proto
  messages in ordered binary WebSocket frames remotely; retain HTTP only for authenticated
  discovery/bootstrap. Prefer the zero-copy-friendly word-aligned encoding, use packed encoding
  only when measured bandwidth savings justify unpacking, and keep large blobs content-addressed or
  separately streamed. The versioned schema, closed participant-submission union, full local RPC
  implementation, typed IPC client, snapshot-first watch, and shared transport-neutral submission
  operation now exist. The home TUI now uses that capability for snapshot, persistent attachment,
  watch, acknowledgement, submit, runner-lease renewal/release, and detach; an ignored live test
  exercises the full local lifecycle against a daemon. Remote watches now use one correlated,
  bidirectional Cap'n Proto envelope for snapshot/event projections and submit, acknowledge, and
  detach commands. The server binds commands to the authenticated socket attachment, revalidates
  scoped credentials on every command and while idle, and preserves event delivery while a long
  runner request is in flight. A loopback fixture and ignored live-daemon test cover the remote
  command lifecycle, including detach before an explicit watch. JSON submit, acknowledge, detach,
  and runner-lease routes have been removed; HTTP now remains only for authenticated
  discovery/credential/attachment bootstrap and explicit administrative archive. An ignored live
  conformance fixture now drives local RPC and remote binary adapters through the same lifecycle and
  compares their normalized events, submission outcomes, and queued run state. A cloneable
  in-process `BrainLifecycleService` now owns attachment reservation/expiry, atomic watch activation,
  acknowledgement, detach cleanup, participant submission, queued-run resumption, and runner-lease
  lifetime; embedded hosts can call it directly, while both transport adapters contain only
  authentication, encoding, socket, and callback mechanics. Hermetic service tests and the live
  cross-transport fixture exercise that same boundary. Tool inputs, approval details, and approval
  decisions now use a recursive schema-native `JsonValue` union across durable events, runner turn
  callbacks, and reverse approval RPC rather than opaque JSON byte strings, with exact signed,
  unsigned, float, string, list, and object preservation. VM effects resume against exact
  `(execution_id, sequence)` identity, and Brain approvals now use exact
  `(brain_id, request_seq, approval_id)` keys so stale decisions cannot consume live continuations.
  Provider context now crosses the runner callback as typed `List(Message)`, and tool inputs in both
  Brain context and ordinary query/stream IPC use `JsonValue`. Runner registration, program results,
  and full-turn results now carry a closed, schema-native `TypedRuntimeCheckpoint`: all typed values,
  types, effect selectors, IR instructions, verified modules, continuations, diagnostics, and
  producer fibers round-trip without an opaque JSON envelope. A live addressed-handoff test proves
  native checkpoint bootstrap/result transport through the replacement frontend callback. Durable
  checkpoint blobs now use that same Cap'n Proto graph and content hash; JSON is read only to
  migrate already-journaled historical hashes.
- [ ] Define Brain initialization as a reviewed typed program/module with an explicit capability
  budget and journaled effects. Deterministic VM vocabulary/module loading may occur before a
  runner accepts turns; proofs, poetry, provider calls, and other observable initialization work
  must be separately scheduled/approved BrainRuns. Do not revive the legacy mutable
  `boot = true` Co-Forth registry as an ambient startup hook.
- [x] Add the first per-Brain runner lease and participant-role substrate. Attachments persist as
  `runner`, `driver`, `consultant`, or `observer`; remote attachment creation cannot mint runners;
  roles constrain submission; and the status/list projections report the authoritative role and
  live runner. The exclusive runner lease expires, renews without event-log heartbeat spam, is
  bound to an exact environment generation, and emits a durable release on expiry or graceful
  frontend shutdown. `/quit` follows the ordinary cleanup path, detaches both home/selected
  participants, and removes an otherwise-unused provisional Brain.
- [x] Dispatch named-Brain ProgramRuns to the leased environment frontend instead of executing
  them in the daemon. The frontend registers a lease-bound Cap'n Proto callback, re-registers it on
  every renewal, hydrates newer durable reducible VM state without importing daemon authority, and
  returns a correlated output/revision/checkpoint. The daemon validates and content-addresses that
  checkpoint; missing and stale callbacks fail closed. Request-boundary, replacement-callback,
  authority-separation, pending-continuation, and daemon-restart tests cover the path. Interactive
  prompts/programs now receive canonical event-sourced `RunId`s and lifecycle state: missing
  callbacks leave never-started work `queued_for_environment`, registration drains that queue under
  the Brain turn lane, approval suspension is visible on the same run, runner failures become
  correlated failed runs, and daemon restart marks already-started work interrupted without
  replaying it. Runs may now name a validated nonterminal parent; ancestry survives event-log
  reconstruction and is inspectable through the transport-neutral lifecycle service. Exact run IDs
  cross the leased-runner callback. The initiating driver can cancel its own queued, interrupted,
  running, or approval-suspended run; running cancellation overtakes the outstanding Cap'n Proto
  execution RPC, is acknowledged by the exact frontend callback, and records `cancelled` rather
  than misclassifying the callback error as `failed`. Local Cap'n Proto and authenticated remote
  binary transports expose the same inspect/cancel operation, `/brain runs` and `/brain cancel
  <id-prefix>` use that shared client surface, and live-daemon tests cover both queued cancellation
  and cancellation of a still-running callback.
- [ ] Route the ordinary home-console conversation and its complete coding-agent/tool loop through
  the same named-Brain event log. Ordinary home prompts now use a durable driver attachment and the
  daemon turn lane; the leased frontend runs the complete provider/tool/VM callback, while both home
  and remote consoles replay its canonical prompt/program/result events. A live two-console test
  submitted a follow-up from the remote driver, executed it on the home runner, and displayed the
  same Lisp source/result in both projections. Frontend tool calls and results now cross the
  runner callback as typed Cap'n Proto lifecycle entries, persist before the final program in the
  canonical log, reconstruct provider-native tool messages after restart, and replay as grouped
  tool rows on attached consoles without duplicating the home runner's live view. Approval
  requests and the exact selected decision now share that ordered typed callback transcript,
  persist as versioned canonical events attributed to the runner participant, and replay as
  approval rows without being injected into the provider protocol. A failed or cancelled callback
  returns and persists its partial lifecycle before the terminal Brain error; keep workspace
  execution and provider tools in the frontend runner. The daemon now also exposes a per-turn
  reverse Cap'n Proto approval capability: it publishes the triggering call and addressed request
  immediately, accepts a decision only from that attachment, persists the decision before resuming
  the runner, and deduplicates the final lifecycle flush. The obsolete client-local `BrainSession`,
  separate typing-time provider, hidden context injection, and ambient question/action shell path
  have been deleted; any future speculative helper must be a visible cancellable `BrainRun` here.
- [ ] Complete Brain control and approval ownership above that substrate. Put the role and approval
  audience on every permission/proposal view. ProgramRuns
  now execute on the leased frontend. Each approval request now carries the daemon-selected
  initiating attachment ID, subject, actual participant role, Brain identity, and environment
  generation through Cap'n Proto into tool, VM-capability, and editor-backed proposal views; the
  daemon rejects a runner that substitutes that audience before journaling it. The exact addressed
  console now presents the ordinary tool/VM dialog and returns its structured decision through the
  canonical log; disconnect/cancellation fails the suspended continuation closed. Remote clients
  now bootstrap into signed, expiring, revocable credentials bound to Brain ID, environment
  generation, subject, role, and independent scopes; every ordinary route rechecks that live
  audience and the exact attachment identity. Attachment bootstrap now attenuates that credential
  into a signed child bound to the exact attachment/connection pair, removes attachment-creation
  authority (`brain:attach`), and retains the separate `brain:detach` authority needed to close that
  exact connection;
  sibling replay fails even for the same subject, ancestor revocation reaches the child, and a
  pending authenticated reservation prevents another detach from deleting the provisional Brain.
  Signed invitation bootstrap and addressed handoff now exist; finish the remaining
  scope-specific hostile/replay cases before closing this ownership milestone.
- [x] Persist a frontend attachment identity across frontend process restarts, not merely reconnects
  in one process, while keeping the daemon cursor authoritative. The client stores only the opaque
  attachment ID, keyed by durable Brain ID plus console slot/subject/role; the daemon still owns the
  role, connection generation, and acknowledged cursor and rejects concurrent rebinding.
- [x] Expire or clean up an attachment that REST-attaches but crashes before its WebSocket activates,
  without advancing its cursor or allowing a stale connection to detach a later reconnect. REST
  attachment now creates a 15-second pending reservation; only the exact WebSocket activation emits
  `ClientAttached`, concurrent identity reuse fails closed, and expiry clears only the matching
  pending connection without moving its durable acknowledgement cursor or Brain revision.
- [x] Add explicit remote Brain creation while preserving the invariant that one environment is an
  indivisible machine/workspace authority boundary. `/brain create <name>[@machine]` calls an
  authenticated bootstrap/admin endpoint whose request contains only the alias; the owning daemon
  supplies its fixed canonical machine, workspace, and generation and rejects alias reuse. The same
  operation lives on `BrainLifecycleService`, and hermetic plus live-daemon tests cover environment
  ownership, conflict behavior, and scoped archive cleanup.
- [x] Treat the global brain password as a local/bootstrap credential only. Mint scoped, revocable,
  expiring participant credentials containing subject, audience, brain, environment generation,
  and permitted roles. Initial scopes: `brain:read`, `brain:attach`, `brain:detach`, `brain:submit`,
  `brain:approve`, `brain:control`, `environment:execute`, `environment:admin`, and
  `compute:submit`. A daemon-owned
  random signing secret and revocation ledger survive restart; the password is accepted only by
  authenticated discovery/bootstrap/administration routes, and the client refreshes its scoped
  credential without reverting ordinary operations to the password.
- [x] Replace password sharing with signed Brain invitations for collaboration bootstrap. The owner
  issues a short-lived single-participant token fixing Brain ID, environment generation, role,
  scopes, delegation ancestry, and expiry; runner authority is forbidden. `/brain join` redeems it
  into the existing scoped credential and attachment path. Redemption persists a subject binding
  before responding, returns the exact same credential to same-subject retries after response loss
  or daemon restart, rejects a different subject, and inherits ancestor revocation. This completes
  invitation bootstrap. The persistent Ed25519 node identity now also signs a deterministic
  self-signed TLS certificate; each signed invitation carries that exact DER trust root. Opt-in LAN
  collaboration uses a restricted TLS-only listener on port 11436, advertised through mDNS without
  authority, and invitation clients pin HTTPS and WSS to the signed certificate. The plaintext
  daemon/admin listener remains loopback-only and remote password bootstrap is rejected. B6's
  hostile-LAN, certificate-substitution, invitation replay, confused-deputy, cross-Brain audience,
  sibling-connection replay, and approval-audience substitution matrix now passes. mDNS carries
  only stable authority-free identity/reachability metadata; dynamic node/model availability is
  queried with `brain:read` and checked against the credential and invited node identity.
- [ ] Enforce least privilege independently for event visibility, prompt/program submission,
  approval, control-lease ownership, workspace effects, environment changes, credential minting,
  and distributed inference. mDNS advertisement and discovery now use an authority-free metadata
  allowlist; the reusable legacy peer token is neither broadcast nor copied into discovered peer
  state. Self attachment creation uses `brain:attach` and exact bound-connection teardown uses
  `brain:detach`, rather than either operation requiring `brain:control`; default consultant
  credentials no longer approve, default observer credentials are read/attach/detach only,
  elevated participant scopes must be requested explicitly within a role-specific ceiling, and
  ordinary HTTP/WebSocket operations require scoped credentials even over loopback. Keep all future
  discovery records credential-free. A `brain:control` credential may now mint a bounded descendant
  whose scopes are a subset of both the delegator and target role, whose expiry cannot exceed its
  parent, and whose signed ancestry makes ancestor revocation invalidate the whole descendant chain.
  Runner control now uses a durable, addressed handoff reservation bound to the exact source lease,
  target runner subject, and environment generation. Remote `brain:control` holders may request or
  cancel it, while only the environment-owning local Cap'n Proto service may accept it; acceptance
  atomically replaces the lease and makes the previous callback stale. Frontends now expose an
  ephemeral per-process runner identity plus request/accept/cancel commands, restore their previous
  runner if a transfer fails, and accept only through the local Unix-socket service when normalized
  hostname and canonical workspace exactly match the Brain environment. A live daemon test transfers
  a registered runner, revokes the requesting controller before its next command, proves only a
  freshly authorized driver and the target callback receive the next ProgramRun, and observes the
  correlated durable result. The transport-neutral submission gate now also rejects consultant
  prompts before they can append an event or create a run; consultants contribute relay-only
  `ParticipantMessage` context unless separately granted approval authority. Close the remaining
  scope-specific hostile/replay cases before completing this least-privilege milestone. Local
  runner identities are now first-claim bound to one Cap'n Proto connection; acquisition, renewal,
  handoff acceptance, release, and callback registration require that connection's lease authority,
  and disconnect atomically removes its callback. A live two-connection test proves a client that
  merely learns the public lease ID cannot claim, renew, register, or release it. Local participant
  attachments now use the same connection-bound rule: watch, submit, acknowledge, run cancellation,
  and detach require the Cap'n Proto connection that created the attachment connection identity.
  A second live two-connection test proves snapshot-visible attachment IDs cannot be replayed while
  the owner continues to submit and acknowledge normally.
- [ ] Revisit shared channels only after the Brain event log, runner lease, and participant-role
  model are complete. The eventual channel should be a threaded/multi-participant projection of a
  Brain: people and models share one durable conversation, while programs run only on the remote
  environment-owning runner. Adding/removing VM definitions and borrowing CPU require explicit,
  attenuated participant grants. The base conversation boundary now exists: canonical
  `ParticipantMessage` events are relay/store-only, enter later prompt context, and never schedule
  an LLM turn; drivers and consultants can submit them while observers and runners cannot. `/say`
  exposes that path without losing terminal punctuation, `/who` projects connected authenticated
  attachments, and `/whois <subject>` reports public role/presence/cursor metadata but never
  credentials. A live two-frontend check on 2026-08-25 attached a second driver, projected the same
  relay to both consoles, and confirmed that it created no `BrainRun`; presence rows now include a
  short durable attachment ID so two consoles owned by the same subject remain distinguishable.
  `@finch <prompt>` now explicitly schedules a model turn without persisting the addressee marker,
  while `/say` remains relay-only; projected prompts and relays use distinct markers and stable
  participant-derived backgrounds. Still add threads/channels and the corresponding collaboration
  authorization tests. Until then,
  quarantine the remaining aspirational IRC/room/peer/gas command surfaces (`/join`, `/part`,
  `/room`, `/connect`, and related commands) rather than presenting them as a second collaboration
  protocol.

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
- [ ] Make memory persistence independent of the provider's completion path: store successful turns
  that used tools as well as no-tool turns, correlate them to Brain/run/event identity, and test that
  failures and retries do not duplicate memories. Separate always-applied user/project policy from
  best-effort semantic recall; a preference that must govern behavior cannot depend on reaching the
  top-k TF-IDF/neural results. Give recalled summaries stable provenance/IDs and an explicit path to
  inspect their fuller source instead of injecting anonymous truncated text alone.
- [ ] Make OpenAI tool-call behavior respect control ownership: a participant client must not
  accidentally execute workspace tools on its own machine.

## Distributed inference

- [ ] Extend the authenticated node-capability response with a versioned compute manifest:
  CPU/GPU/TPU kind, device count, memory capacity/availability, runtimes, loaded models, queue depth,
  and approximate throughput. Keep mDNS limited to stable identity and transport reachability;
  dynamic compute state is disclosed only after authentication.
- [ ] Schedule bounded, content-addressed inference jobs across discovered compute nodes without
  granting those nodes workspace or execution-environment authority.
- [ ] Record remote inference provenance: node, model, input hash, resource budget, timing, and
  brain environment generation.
