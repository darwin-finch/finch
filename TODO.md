# Finch TODO

This is the short, discoverable work queue. Detailed rationale and protocol sketches live in
[`docs/TYPED_LISP_FORTH_CAPABILITY_JIT_PLAN.md`](docs/TYPED_LISP_FORTH_CAPABILITY_JIT_PLAN.md),
[`docs/SHARED_PROGRAM_RUNTIME_PLAN.md`](docs/SHARED_PROGRAM_RUNTIME_PLAN.md), and
[`docs/BRAIN_CONVERGENCE_PLAN.md`](docs/BRAIN_CONVERGENCE_PLAN.md).

## Typed Lisp/Co-Forth VM — prerequisite for Brain convergence

- [ ] Finish both source frontends and their shared typed IR semantics: definitions/signatures,
  conditionals, metered loops, locals, quotations, closures, collections, bounded macros, and
  structured error/result forms.
- [ ] Generate every production word/function from one typed signature, effect, documentation, and
  host-implementation registry.
- [ ] Finish the capability broker: bounded argument templates, availability, grants, attenuation,
  revocation, audit, approval dialogs, runtime guards, and typed suspend/resume.
- [ ] Bind files, native tools, processes, network, automation, MemTree, schedules, response output,
  and agent fork/join/model selection through typed VM primitives.
- [ ] Make `ProgramRuntime` and VM inspection use one persistent typed stack, dictionary, heap,
  transaction manager, and revision history for Lisp and Co-Forth.
- [ ] Remove the Lisp-to-Forth text compiler, native Lisp fallback, source-text effect inference,
  and duplicate direct model-tool paths after conformance parity.
- [ ] Complete provider language packages, structured shadow-buffer outcomes, rollback/security
  tests, concurrency tests, and provider conformance tests. Do not require the later Cranelift JIT
  optimization tier to begin Brain convergence.

## Shared brains and environments

- [ ] After every VM prerequisite above passes, execute
  [`docs/BRAIN_CONVERGENCE_PLAN.md`](docs/BRAIN_CONVERGENCE_PLAN.md): consolidate the three current
  Brain concepts into one daemon-authoritative event log, VM history, environment, and authority
  boundary; model interactive turns, speculative helpers, schedules, and subagents as `BrainRun`s;
  and make local, embedded, IPC, HTTP/WebSocket, and remote clients projections of one service.
- [ ] Persist complete VM checkpoints or reversible VM deltas at committed program boundaries.
- [ ] Split reducible VM state from the execute-once host-effect journal. Never replay file,
  process, dialog, or network effects while restoring VM state.
- [ ] Add typed compensating actions for reversible effects. File undo must use preimage and
  postimage hashes plus a conflict-aware reverse changeset.
- [ ] Finish remote named-brain attach/detach, live scrollback replacement, status display, and
  prompt/Forth/Lisp routing through the daemon-owned event stream.
- [ ] Add per-brain control ownership/leases and participant roles. Only the bound environment
  may execute workspace effects or reveal/rotate its credential.
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
