# Shared Program Runtime Plans

## Goal

Make Finch a reliable conversational agent backed by a shared, reflective programming
environment. Humans, agents, and remote peers can publish Forth or Lisp programs, inspect
and reuse them, and request their execution through the same durable session protocol.

Normal user text always starts an agent turn. Forth and Lisp are explicit executable
artifacts used through program submission or through `/forth` and `/exec`.

## Core decisions

### Persist vocabulary between sessions

Yes. Cross-session persistence is necessary for learned programs to reduce tokens and model
round trips. Persistence must be scoped and promoted deliberately.

| Scope | Lifetime | Intended contents | Publication policy |
|---|---|---|---|
| Built-in | Finch release | Core words, effects, adapters | Maintainer-reviewed |
| Session | Current session | Experiments and temporary helpers | Automatic, ephemeral |
| Project | Sessions in one repository | Tested project workflows | Explicit approval; version-controlled |
| Personal | Sessions across projects | General personal workflows | Explicit approval |
| Imported | Until reviewed | Programs received from peers | Quarantined by default |

Session definitions begin as candidates. Finch may propose promotion after a definition
compiles, passes its tests, and is used successfully. Project and personal definitions use
ordinary `.forth` and `.lisp` files under a `vocabulary/programs` directory. Programs authored
through the local VM are immediately visible in `~/.finch/vocabulary/programs/generated/`;
they must never exist only as opaque database rows. SQLite is a rebuildable search index,
usage ledger, and recovery cache—not the canonical source. Compiled artifacts are also caches.
Source files, metadata, tests, and provenance are canonical.

### Discover vocabulary from runtime state

No LLM should be expected to remember which definitions exist. A model may change during a
session, a session may restart, and remote peers may have different environments.

Every LLM worker receives a runtime handshake containing a compact `VmManifest`:

```text
runtime and protocol versions
active project and environment hash
available source languages
core effects and syntax
relevant program names, signatures, and one-line descriptions
registry generation number
```

The handshake begins with the versioned, human-readable kernel in `vocabulary/BOOT.md`.
That boot capsule must remain small enough to send on every new model, restart, or context
compaction. It defines the wire ABI and safety/effect rules, not the complete Forth/Lisp
standard or the accumulated vocabulary; those are retrieved through introspection.

The manifest is a summary, not a dump of all source. Models can call:

```text
search_vocabulary(query, language?, capability?, limit?)
inspect_program(program_id, include_source?, include_tests?)
list_capabilities()
get_vm_state(detail)
```

The registry generation changes whenever definitions or active scopes change. A model switch,
context compaction, peer environment change, or stale generation triggers a new handshake.
Execution requests include the generation/environment hash against which they were composed;
Finch rejects or recompiles stale requests instead of invoking a different definition by name.

Prompt assembly should retrieve only programs relevant to the current user request. Full
source is loaded on demand. This makes vocabulary growth reduce token use instead of making
the system prompt grow without bound.

### Keep conversation transport separate from execution

The durable session is an append-only event log. The visible conversation stack is a
projection of its active branch. Rewind or pop moves the active head or appends a revert
event; it does not delete shared history.

Every requested agent turn must finish with at least one of:

- visible assistant message;
- visible VM output;
- pending dialog or approval;
- explicit failure or cancellation.

Successful execution with no visible effect still produces a status event. This removes the
current class of silent turns.

Assistant output has a provisional phase and a committed phase. Streaming draft updates may
replace earlier draft text under the same message ID. Commitment appends an immutable event;
later corrections append `supersedes` or `retracts` events and move the active projection rather
than deleting history. Programs and mutations may be submitted only from committed output.

### Rewind VM state without replaying external effects

A brain has one authoritative event log, but two different kinds of derived history:

- VM state is reducible state: data/return stacks, definitions, variables, heap, and other
  language-runtime state.
- Host effects are execute-once facts: files changed, processes started, network messages sent,
  dialogs answered, and other observations outside the VM.

Each committed program records its input VM revision, resulting VM revision, declared and derived
effect, emitted effect intents, and effect outcomes. Finch persists a serializable VM checkpoint
or reversible VM delta at every committed program boundary. Popping a program moves the active
head to the preceding VM revision and updates every attached client's projection. It must not
re-run source merely to reconstruct state: replay could repeat a file write, shell command, network
request, or other external action.

External effects are never silently reversed by a stack pop. An effect may optionally record a
typed compensating action. File changes should retain the preimage hash, postimage hash, and reverse
changeset; compensation applies only when the current content still matches the expected postimage,
otherwise Finch proposes a conflict-aware diff. Process execution and network sends are normally
irreversible and are reported as such. Running a compensating action is a new audited event and uses
the same approval policy as the forward action.

Until transactional effect capture and persistent VM checkpoints exist, named-brain programs that
can produce workspace, external, destructive, or unclassified effects must not participate in
automatic replay. `remote_mode` only suppresses interaction; it is not an effect sandbox.

### Make programs the model-facing action interface

The model may answer normally or submit a program. Individual filesystem, shell, dialog,
network, and editing tools are not exposed directly. They are capabilities available inside
Forth and Lisp programs.

Provider-native tool calling remains the reliable envelope:

```text
submit_program(intent, language, source, expected_result)
search_vocabulary(query, ...)
inspect_program(program_id, ...)
```

Conventional model tool calls can be translated during migration and reported back to the
model. Direct legacy tools should be removed after compatibility tests pass.

### Share an ABI and effect model before building a universal IR

Forth and Lisp initially keep their own evaluators. They share:

- a callable program registry;
- tagged values;
- signatures and Forth stack effects;
- immutable program IDs and versioned dependencies;
- a capability/effect broker;
- execution results and suspension semantics.

Cross-language invocation uses a language-neutral boundary:

```text
invoke(program, arguments, execution_context) -> values | effect | error
```

External behavior is normalized into typed effects such as `Say`, `ShowDialog`, `ReadFile`,
`WriteFile`, `ExecuteProcess`, `InvokeProgram`, and `SendToPeer`. This effect IR is required
for permissions, approval, audit, replay, and suspension.

Effect declarations are part of the language contract. Primitive VM words declare their effects;
definitions derive the least-safe join of the words they can call. Pure, VM-local, workspace-read,
and external-read programs run autonomously while remaining visible in the audit log. Unknown or
dynamic calls are `unclassified`. Workspace writes produce a `ChangeSet`/minimal diff first;
applying that changeset is a separate approved effect.

A full computational Finch IR is optional and comes later. Definitions that fit a portable
subset may include compiled IR. Dynamic Lisp macros, non-serializable closures, and Forth
words dependent on VM-specific state can remain native.

## Program registry

Each stored definition includes:

```text
stable ID and immutable version
qualified name and optional sense
language and original source
documentation and examples
signature or stack effect
declared and derived capabilities
versioned dependencies
tests or proofs
provenance, trust state, and scope
source and environment hashes
optional portable IR
usage and success statistics
```

Names are convenient aliases; execution and replay resolve immutable IDs and versions.
Approvals bind to the source and environment hashes shown to the user.

Both languages expose equivalent reflection operations: search, describe, source,
dependencies, capabilities, tests, define, publish, revise, and deprecate.

## Plan A: Conservative shared registry

This plan proves persistence and interoperation without a common computational IR.

1. Add stable identity, version, language, source, documentation, signature, scope, trust,
   provenance, and environment generation to a shared registry.
2. Project existing Forth `WordEntry` definitions and persisted Lisp definitions into registry
   views without changing their execution behavior.
3. Add tagged values and adapters for calling Lisp from Forth and Forth from Lisp.
4. Add the VM manifest, startup/model-switch handshake, and reflection operations.
5. Add session candidates and explicit promotion to project or personal scope.
6. Replace full vocabulary prompt injection with task-relevant retrieval.

This is the lowest-risk route and a useful stopping point if interoperation and token savings
are the main goals.

## Plan B: Program-first agent runtime (recommended)

This plan builds on Plan A and changes how agent actions are expressed.

1. Introduce an append-only event envelope with event ID, turn ID, author, parent, branch,
   ordering, payload, and terminal status.
2. Guarantee that every normal user message schedules an LLM turn. Remove implicit routing of
   ordinary text into silent Forth execution.
3. Define `/forth <source>` as direct execution in the session VM and
   `/exec <event-or-program>` as execution of a stored or shared artifact.
4. Expose only program submission and registry introspection to models. Move legacy tool
   implementations behind VM capabilities.
5. Implement typed effects and continuations. Dialogs and approvals suspend a program and
   resume it with a typed result.
6. Add `REQUEST_EXEC`: English intent plus Forth/Lisp source, capability derivation,
   exact-hash approval, execution, and result events.
7. Translate legacy tool calls during migration, then remove direct action tools from the
   model-visible schema.
8. Evaluate silent-turn rate, compilation success, intent/effect agreement, vocabulary reuse,
   stale-manifest recovery, and model round trips.

This is the recommended product direction because it preserves chat reliability while making
programs the unit of agency.

## Plan C: Portable Finch IR and distributed execution

This plan follows Plan B after stored programs reveal which semantics need portability.

1. Design a small explicit IR for constants, arguments, locals, calls, branches, collections,
   capability requests, and returns.
2. Compile the portable subset of Forth and Lisp into it while retaining original source.
3. Validate signatures, stack effects, capabilities, dependency versions, fuel, and resource
   budgets before execution.
4. Package programs with manifests and content hashes for peer exchange.
5. Execute remote requests transactionally against explicit state snapshots rather than a
   concurrently mutable global data stack.
6. Add idempotent request IDs, retries, environment negotiation, missing-dependency exchange,
   and signed provenance.
7. Add suspended-continuation migration only after local suspension is stable.

This enables portable distributed programs but should not block the registry or program-first
interface.

## Recommended delivery sequence

### Milestone 1: Restore the response invariant

- Normal text always reaches the LLM.
- Explicit commands select Forth/Lisp execution.
- Every turn has a visible terminal or suspended state.
- Transport lag and failures become visible events.

### Milestone 2: Reflective persistent vocabulary

- Shared registry and tagged values.
- VM manifest and model/session handshake.
- Introspection and cross-language calls.
- Session candidates plus project/personal promotion.
- Retrieval-based prompt context.

### Milestone 3: Program submission

- `submit_program` and `REQUEST_EXEC`.
- Capability/effect broker.
- Dialog and approval continuations.
- Legacy tool-call translation, followed by removal of direct model-facing tools.

### Milestone 4: Collaboration and hardening

- Append-only shared log and branch/rewind semantics.
- Quarantined peer imports and versioned dependency exchange.
- Content-bound approval and deterministic replay.
- Concurrency, retry, security, and resource-limit tests.

### Milestone 5: Optional portable IR

- Derive the IR from observed reusable programs.
- Compile only the portable subset.
- Add remote execution and continuation migration incrementally.

## Persistence policy

1. Definitions created during a turn are session-scoped candidates.
2. Candidates use a recovery journal so crashes do not lose work, but are not automatically
   loaded into unrelated sessions.
3. Passing tests/proofs and successful execution make a candidate eligible for promotion.
4. Project promotion requires approval and writes a reviewable source file under
   `vocabulary/programs` in the repository.
5. Personal promotion requires approval and writes a source file under Finch's personal
   `vocabulary/programs` directory.
6. Imported programs remain quarantined until analysis, tests, and approval pass.
7. Revisions create immutable versions rather than mutating definitions used by old logs.
8. Unused candidates may be archived; approved versions remain addressable for replay.

## First implementation slice

1. Define `ProgramDefinition`, `ProgramRef`, `ProgramScope`, `TrustState`, and `VmManifest`.
2. Add registry tables and a migration path from the current `lisp_env` replay model.
3. Project existing `WordEntry` records into the registry without breaking TOML compatibility.
4. Add registry generation tracking and manifest refresh on startup and model switch.
5. Add `vocab search`, `vocab describe`, `vocab source`, and model-facing equivalents.
6. Add one pure cross-language bridge in each direction.
7. Test that stored Forth and Lisp functions can call each other, survive a new session, and
   are rediscovered after switching models.

Do not move tools behind the VM or build portable IR in this first slice. Those changes become
safer after registry identity, persistence, discovery, and interoperation are stable.
