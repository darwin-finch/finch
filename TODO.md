# Finch TODO

This is the short, discoverable work queue. Detailed rationale and protocol sketches live in
[`docs/SHARED_PROGRAM_RUNTIME_PLAN.md`](docs/SHARED_PROGRAM_RUNTIME_PLAN.md).

## Shared brains and environments

- [ ] Consolidate the three current "brain" concepts. A brain is one authoritative context,
  event log, program stack, and VM state; background agents become worker jobs attached to it,
  and legacy shared-context strings become a projection/migration path.
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
- [ ] Replace shared passwords with scoped, revocable credentials; do not advertise credentials
  in mDNS discovery records.

## Client and model integration

- [ ] Add an OpenAI-compatible named-brain adapter so Cline/Roo requests participate in the same
  event log instead of using Finch as a stateless model proxy.
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

## Verification and maintenance

- [ ] Repair the local Apple Command Line Tools installation providing `clang_rt.osx`, then run
  the full linked test suite. `cargo check --tests` currently succeeds, but test linking is blocked.
