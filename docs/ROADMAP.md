# Finch development roadmap

**Last updated:** 2026-09-02

This is a forward-looking guide to Finch's intended direction. It is not a release schedule or
proof that a feature works. The [GitHub issue tracker](https://github.com/darwin-finch/finch/issues)
is authoritative for work status and dependencies; the README, current source, and tests describe
the checked-out revision. Historical release details belong in the changelog and archive.

## Product direction

Finch is intended to become one user-controlled agent runtime spanning two workloads that are
usually separate:

1. A terminal coding agent in the problem space of Codex, Claude, Grok, Muse, and OpenCode.
2. A persistent personal assistant in the problem space of OpenClaw-style systems, capable of
   running on a spare computer with attached terminal, remote, voice, and automation frontends.

The differentiating goal is not a longer provider list. Finch should preserve one durable,
auditable workflow while changing models, frontends, and execution environments. Cloud APIs,
supported subscription accounts, local models, skills, MCP tools, desktop applications, and remote
Brains must all remain subject to explicit capability and privacy boundaries.

## Current foundation

The current source includes an interactive TUI and raw REPL, provider-backed chat, a bounded HTTP
daemon, named Brain persistence, a typed Lisp/Co-Forth runtime, approval-aware tools, an MCP client,
explicit feedback storage, and experimental ONNX Runtime and Candle model loaders.

Those components have uneven end-to-end maturity. In particular, configuration variants do not
prove provider conformance, configured local models do not prove local routing, and implemented
Brain primitives do not make unattended personal automation release-ready. See the README's
current limitations and the issues below.

## Immediate: reliable daily dogfooding

- Finish direct provider and supported subscription authentication without borrowing another
  application's credentials ([#51](https://github.com/darwin-finch/finch/issues/51)).
- Publish repeatable provider/model wire conformance rather than provider-family claims
  ([#98](https://github.com/darwin-finch/finch/issues/98)).
- Make provider/model selection durable per Brain and cheap to change
  ([#217](https://github.com/darwin-finch/finch/issues/217)).
- Complete accessible file-context selection
  ([#310](https://github.com/darwin-finch/finch/issues/310)).
- Replace legacy UUID-session output with named-Brain attach and resume UX
  ([#314](https://github.com/darwin-finch/finch/issues/314)).
- Stream truthful Brain progress and converge work on typed task handles
  ([#57](https://github.com/darwin-finch/finch/issues/57),
  [#60](https://github.com/darwin-finch/finch/issues/60)).

## Next: one extensible agent environment

### Skills and tools

Finch should discover and invoke repository Agent Skills with progressive disclosure, provenance,
and no implicit authority escalation ([#213](https://github.com/darwin-finch/finch/issues/213)).
Skills and MCP tools should use the same typed capability broker and appear in the same event and
approval history.

### Local models and explicit fallback

Refresh the model catalog using dated artifact/runtime/hardware evidence
([#74](https://github.com/darwin-finch/finch/issues/74)). Provider selection and fallback must be
visible, preserve the actual model identity, and never silently turn a requested local operation
into a cloud request. Hosted Muse support and its announced open-weight path are tracked separately
([#317](https://github.com/darwin-finch/finch/issues/317)).

### Multimodal input and local perception

Complete typed image attachment transport without replacing dropped media with text markers
([#135](https://github.com/darwin-finch/finch/issues/135)). Investigate local speech recognition,
OCR, image description, and bounded media-to-text summaries for both users and authorized agents
([#318](https://github.com/darwin-finch/finch/issues/318)). Derived text must retain provenance and
remain untrusted input.

### Durable work and collaboration

Finish background BrainRuns and make their state reconnectable
([#106](https://github.com/darwin-finch/finch/issues/106)). Add authenticated threaded channels and
durable Brain-to-Brain messaging
([#112](https://github.com/darwin-finch/finch/issues/112),
[#175](https://github.com/darwin-finch/finch/issues/175)). Schedule only bounded,
content-addressed work with explicit policy, ownership, retry, and cancellation semantics
([#125](https://github.com/darwin-finch/finch/issues/125)).

## Later: an accessible always-on personal assistant

The intended personal-agent deployment is a Finch daemon and named Brain running continuously on a
user-controlled machine, with one or more lightweight frontends attached. It may receive explicit
messages or scheduled work, use skills and local/cloud models, and interact with applications
through semantic accessibility elements.

The implementation must follow the
[VM-native agent runtime plan](VM_NATIVE_AGENT_RUNTIME_PLAN.md): inspect and act by application,
role, label, and stable domain identifiers wherever possible. Raw screen coordinates are an
internal last resort, not a public user or model interface. Background, remote, and scheduled work
starts without desktop mutation authority and requires a narrow unattended-action policy.

This phase also includes reproducible frontend self-hosting
([#102](https://github.com/darwin-finch/finch/issues/102)), resource-aware work distribution, remote
presence, emergency stop, and clear notification/audit UX. It is complete only when restart,
replay, cancellation, revocation, and hostile-input cases pass at production boundaries.

## Non-goals and evidence rules

- Do not reproduce another product's private authentication or credential store.
- Do not describe configuration, compilation, or a model download as end-to-end support.
- Do not silently fall back across provider, account, local/cloud, or capability boundaries.
- Do not grant a skill, model, remote peer, or scheduled run ambient machine authority.
- Do not make automatic training or data contribution a consequence of feedback.
- Do not call Finch production-ready until the relevant release and live acceptance gates pass.

## Contributing

Check the issue tracker and [contribution guide](../CONTRIBUTING.md) before starting. Every bug fix
requires a regression test at the production boundary where the failure occurred. Design documents
describe intent; dated tests and live acceptance evidence establish behavior.
