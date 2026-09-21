# Finch design index

This is the root map of Finch's architecture: how the binary is composed today, which subsystem
owns which code, where each subsystem's authoritative documentation lives, and where the design is
meant to go. It links rather than repeats. `AGENTS.md` (a symlink to [`CLAUDE.md`](CLAUDE.md))
remains the single source for invariants and working rules; co-located module documents, source,
and tests remain authoritative for detail.

**Status legend.** Unlabelled sections describe `main` and cite source by path and symbol. The
[intended direction](#intended-direction) and [open questions](#open-questions) sections are design
intent from the subsystem program ([#541](https://github.com/darwin-finch/finch/issues/541)) and
are not evidence that anything is implemented. Numbers appear only in the dated
[snapshot](#snapshot). History lives behind the [documentation map](docs/README.md); this index
does not cite archived or historical documents. Subsystem-level `DESIGN.md` files, when they
arrive, hold intent only; this root index is the deliberate exception that maps current and
intended structure side by side.

`scripts/check_docs.py` checks this file's links, anchors, shell fences, and known stale claims,
that it links every capsule under `src/`, that it cites no historical or archived
document, and that design documents appear only under intended direction or open questions. It does
not verify the prose or the cited symbols.

## Why subsystems exist

**The goal is to bound how much an agent must read.** Finch is worked on mostly by LLM agents, and
the binding constraint is context, not compilation. An agent changing a subsystem should need that
subsystem's code, its capsule, and the *interfaces* of the subsystems it depends on. It should
never need their implementations, and it should never have to read the tree to find out what it is
allowed to touch.

Everything else in this section follows from that:

- **A subsystem's interface is a short, enumerable list.** Implementation modules are private, so
  what a caller can reach is what the facade re-exports. `vm` is the worked example: about 60
  exported items stand in front of roughly 23,000 lines.
- **A module states its own limits.** Its capsule says what it may depend on and what it must not,
  beside the code rather than in a central file that drifts from the tree.
- **Instructions are local.** A capsule beside the code states scope, limits, and focused tests, so
  an agent starting there inherits the root rules plus one page, not the whole project narrative.
- **Facades are the entry points.** A caller reads the module's short README and agent contract
  for meaning and rules, then its `mod.rs` or `src/lib.rs` for exports and facade-local signatures.
  Rustdoc renders methods on re-exported types for exact signatures without opening private
  implementations. Generated signature catalogs are not architectural authority.
- **Crate extraction is optional.** A crate makes a boundary unforgeable and shrinks the dependency
  surface a change pays for, but the facade is what delivers the context win. Extract only where
  measurement justifies it.

**Measure the goal, not its proxies.** The primary number is the context a representative task
requires: the capsule, the files being edited, and the facades of what they depend on.
Build and test latency are secondary evidence; they say how fast the loop runs, not how much an
agent must understand to be correct.

## Composition

Finch builds as one package that produces the `finch` binary ([`src/main.rs`](src/main.rs)) and a
test-only isolation supervisor ([`src/bin/finch-test-supervisor.rs`](src/bin/finch-test-supervisor.rs)).
`main.rs` dispatches to four main composition paths:

| Path | Entry in `src/main.rs` | What it wires |
|------|------------------------|---------------|
| Typed programs (`--exec`, `--forth`, `--lisp`) | calls to `ProgramRuntime::submit_typed_only` | A fresh `ProgramRuntime` granted only session output; no provider or config |
| Pipe or `finch query` | `run_query` | Program-shaped input runs directly (`is_clearly_forth`); otherwise `build_query_tool_executor` with `DaemonClient`, or teacher-only |
| Interactive REPL | `Repl::new`, then `Repl::run_event_loop` | Provider graph (`create_provider_graph_from_config`), HTTP `DaemonClient`, `ipc::IpcClient`; the REPL builds its provider profile again inside `run_event_loop` (`src/cli/repl.rs`) |
| Daemon (`finch daemon`, `daemon-start`) | `run_daemon` | `DaemonLifecycle::acquire_instance`, provider graph, `BootstrapLoader` background model loading, `AgentServer` over HTTP, `ipc::start_ipc_server` |

`finch agent` (`run_agent`) is a smaller fifth path that drives `agent::AgentLoop`; the remaining
subcommands are setup, authentication, and maintenance utilities.

Named Brains run on per-Brain runtimes from `BrainStore::program_runtime`
([`crates/finch-brain/src/store.rs`](crates/finch-brain/src/store.rs)). HTTP routes are defined in
[`src/server/handlers.rs`](src/server/handlers.rs); the IPC wire schema is
[`crates/finch-ipc/schema/finch_ipc.capnp`](crates/finch-ipc/schema/finch_ipc.capnp).

## Subsystems

The tree is the ownership record: a directory that carries an `AGENTS.md` is a module, and the
files under it are its own. This table summarizes the current shape. Layers are the intended
direction (lower is more foundational); see [dependencies](#dependencies). "None yet"
marks a subsystem without local documentation; the subsystem program adds one before code moves.
Documents with known stale claims are flagged in [documentation status](#documentation-status).

A subsystem may be declared on a path inside another's — that is all a *sub-subsystem* is, and
`tools-mcp` inside `tools` is the first. Ownership takes the longest matching path, a `crate::a::b`
reference resolves to the deepest owner it names, and the layer ratchet governs the parent and
child like any other pair. The point is the same one as for a top-level subsystem: an agent sent to
the MCP client should not have to load tool execution and permissions to get there.

| Subsystem (layer): responsibility | Owns | Local documentation |
|-----------------------------------|------|---------------------|
| **`config`** (0): configuration, instruction loading, licensing, metrics | `src/config`, `context`, `license`, `metrics`, `monitoring`, `errors.rs`; `data/` personas | Capsule [`src/config/AGENTS.md`](src/config/AGENTS.md), interface [`src/config/INTERFACE.md`](src/config/INTERFACE.md); context capsule [`src/context/AGENTS.md`](src/context/AGENTS.md), interface [`src/context/INTERFACE.md`](src/context/INTERFACE.md); [configuration](src/config/CONFIGURATION.md), [context assembly](src/context/ASSEMBLY.md), [licensing](src/license/LICENSING.md) |
| **`theme`** (0): shared saved color vocabulary and terminal color mapping | `crates/finch-theme`; root `src/theme.rs` is a compatibility re-export | [README](crates/finch-theme/README.md), [agent contract](crates/finch-theme/AGENTS.md), [facade](crates/finch-theme/src/lib.rs) |
| **`vm-core`** (0): shared typed IR, verifier, types, effects, capabilities, diagnostics, and vocabulary contracts | `crates/finch-vm-core` | [README](crates/finch-vm-core/README.md), [agent contract](crates/finch-vm-core/AGENTS.md), [facade](crates/finch-vm-core/src/lib.rs) |
| **`colisp`** (0): CoLisp reader and translation into the shared semantic-construction protocol | `crates/finch-colisp`; `vocabulary/language/FINCH_LISP.md` | [README](crates/finch-colisp/README.md), [agent contract](crates/finch-colisp/AGENTS.md), [facade](crates/finch-colisp/src/lib.rs) |
| **`coforth`** (0): Co-Forth reader and translation into the shared semantic-construction protocol | `crates/finch-coforth`; `vocabulary/language/FINCH_FORTH.md` | [README](crates/finch-coforth/README.md), [agent contract](crates/finch-coforth/AGENTS.md), [facade](crates/finch-coforth/src/lib.rs) |
| **`language`** (0): compilation facade that selects a frontend and returns `ModuleVerified` | `crates/finch-language` | [README](crates/finch-language/README.md), [agent contract](crates/finch-language/AGENTS.md), [facade](crates/finch-language/src/lib.rs) |
| **`vm`** (0): execute verified modules, classify compiler-boundary wire failures, and preserve the execution compatibility facade | `crates/finch-vm`; `vocabulary/language/FINCH_VM.md`, `examples/finch/` | [README](crates/finch-vm/README.md), [agent contract](crates/finch-vm/AGENTS.md), [facade](crates/finch-vm/src/lib.rs); language contracts [`FINCH_VM.md`](vocabulary/language/FINCH_VM.md), [`FINCH_FORTH.md`](vocabulary/language/FINCH_FORTH.md), [`FINCH_LISP.md`](vocabulary/language/FINCH_LISP.md); [typed VM migration audit](docs/TYPED_VM_MIGRATION_AUDIT.md) |
| **`programs`** (1): program identity, discovery models, vocabulary manifest, and optional source-only corpus capture; no eager program loading | `crates/finch-programs`; root [`src/program_registry.rs`](src/program_registry.rs) composes authored source and memory indexing | [Programs README](crates/finch-programs/README.md), [agent contract](crates/finch-programs/AGENTS.md), [facade](crates/finch-programs/src/lib.rs) |
| **`memory`** (0): SQLite-backed MemTree storage, retrieval coverage, and opaque program index rows | `crates/finch-memory`; root [`src/program_registry.rs`](src/program_registry.rs) maps program identity to memory rows | [Memory README](crates/finch-memory/README.md), [agent contract](crates/finch-memory/AGENTS.md), [facade](crates/finch-memory/src/lib.rs) |
| **`tools-api`** (0): the dependency-free tool surface — `Tool` trait, registry, typed requests/results, permission and approval policy, declared effects, tool-round protocol | `crates/finch-tools-api` | [README](crates/finch-tools-api/README.md), [agent contract](crates/finch-tools-api/AGENTS.md), [facade](crates/finch-tools-api/src/lib.rs) |
| **`tools-mcp`** (0): the client for external Model Context Protocol servers | `src/tools/mcp` | Capsule [`src/tools/mcp/AGENTS.md`](src/tools/mcp/AGENTS.md), interface [`src/tools/mcp/INTERFACE.md`](src/tools/mcp/INTERFACE.md), [user guide](docs/MCP_USER_GUIDE.md) |
| **`tools`** (1): tool execution and GUI automation — the executor, concrete tool implementations, and MCP wiring over the `tools-api` surface | `src/tools` except `mcp`; re-export shims `src/tools/types.rs`, `src/tools/permissions.rs` | Capsule [`src/tools/AGENTS.md`](src/tools/AGENTS.md), interface [`src/tools/INTERFACE.md`](src/tools/INTERFACE.md); [Tool execution and permissions](src/tools/EXECUTION.md), [macOS GUI automation](docs/MACOS_GUI_AUTOMATION.md) |
| **`runtime`** (2): typed program execution, capability authority, host effects, and delivery ABI | `crates/finch-runtime`; root `poset` and [`src/program_registry.rs`](src/program_registry.rs) are application composition, not runtime-crate internals | [Runtime README](crates/finch-runtime/README.md), [agent contract](crates/finch-runtime/AGENTS.md), [facade](crates/finch-runtime/src/lib.rs) |
| **`models`** (2): local model loading, routing, training, feedback | `src/models`, `local`, `generators`, `training`, `feedback`, `router`, `logging` | Capsule [`src/models/AGENTS.md`](src/models/AGENTS.md), interface [`src/models/INTERFACE.md`](src/models/INTERFACE.md); local-generation facade [`src/local/AGENTS.md`](src/local/AGENTS.md); generators compatibility facade [`src/generators/AGENTS.md`](src/generators/AGENTS.md), interface [`src/generators/INTERFACE.md`](src/generators/INTERFACE.md); [Local model loader](src/models/unified_loader.rs), [ONNX loader](src/models/ONNX.md), [bootstrap loading](src/models/BOOTSTRAP.md), [deferred LoRA path](src/models/LORA.md), [router](src/router/ROUTING.md), [automatic-training status](docs/AUTOMATIC_TRAINING.md) |
| **`finch-providers`** (0): reusable provider transports, OAuth, catalogs, and credential ports | `crates/finch-providers` | [README](crates/finch-providers/README.md), [capsule](crates/finch-providers/AGENTS.md), [facade](crates/finch-providers/src/lib.rs); OAuth [README](crates/finch-providers/src/oauth/README.md), [capsule](crates/finch-providers/src/oauth/AGENTS.md), [facade](crates/finch-providers/src/oauth/mod.rs) |
| **`finch-generation`** (1): development generation contract, lifecycle, strategies, and identity; production REPL still uses older generator path | `crates/finch-generation` | [README](crates/finch-generation/README.md), [capsule](crates/finch-generation/AGENTS.md), [facade](crates/finch-generation/src/lib.rs) |
| **`providers`** (3): Finch Config mapping onto finch-providers, planning prompts | `src/providers`, `claude`, `oauth`, `llms`, `planning` | Providers compatibility facade [`src/providers/AGENTS.md`](src/providers/AGENTS.md), interface [`src/providers/INTERFACE.md`](src/providers/INTERFACE.md); OAuth compatibility facade [`src/oauth/AGENTS.md`](src/oauth/AGENTS.md), interface [`src/oauth/INTERFACE.md`](src/oauth/INTERFACE.md); planning capsule [`src/planning/AGENTS.md`](src/planning/AGENTS.md), interface [`src/planning/INTERFACE.md`](src/planning/INTERFACE.md); [Claude client](src/claude/CLIENT.md), [OAuth boundary](docs/OAUTH.md), [ChatGPT subscription transport](docs/CHATGPT_SUBSCRIPTION_TRANSPORT.md), [OpenAI transport](docs/OPENAI_TRANSPORT.md) |
| **`transport`** (3): domain-neutral Cap'n Proto schema/protocol/socket core, node identity, service discovery | `crates/finch-ipc`, `crates/finch-node`, `network`, `service` | IPC [README](crates/finch-ipc/README.md), [capsule](crates/finch-ipc/AGENTS.md), [facade](crates/finch-ipc/src/lib.rs); application client adapter in `src/client`, daemon RPC adapter in `src/server`, Brain codec in `crates/finch-brain/src/ipc_codec.rs`, runtime checkpoint codec in `crates/finch-runtime/src/ipc_codec.rs`; Node [README](crates/finch-node/README.md), [capsule](crates/finch-node/AGENTS.md), [facade](crates/finch-node/src/lib.rs), root compatibility facade [`src/node/AGENTS.md`](src/node/AGENTS.md); wire schema in [`crates/finch-ipc/schema/finch_ipc.capnp`](crates/finch-ipc/schema/finch_ipc.capnp) |
| **`brain`** (4): durable named Brains, Brain-specific clients and credentials | `crates/finch-brain`; root `server`, `daemon`, `client`, `agent`, `review`, `registry` (migration only), and `graph` are application composition | [Brain README](crates/finch-brain/README.md), [agent contract](crates/finch-brain/AGENTS.md), [facade](crates/finch-brain/src/lib.rs); nested persistence capsules [`attachment`](crates/finch-brain/src/attachment/AGENTS.md), [`journal`](crates/finch-brain/src/journal/AGENTS.md), [`projection`](crates/finch-brain/src/projection/AGENTS.md), [`run`](crates/finch-brain/src/run/AGENTS.md), and [`schedule`](crates/finch-brain/src/schedule/AGENTS.md); server facade [`src/server/AGENTS.md`](src/server/AGENTS.md); daemon capsule [`src/daemon/AGENTS.md`](src/daemon/AGENTS.md); [Brain test inventory](tests/BRAIN_TEST_INVENTORY.md) |
| **`frontend`** (5): commands, the interactive REPL, rendering, setup | `src/cli`, `startup.rs`, `samples.rs` | CLI facade [`src/cli/AGENTS.md`](src/cli/AGENTS.md); capsule [`src/cli/repl_event/AGENTS.md`](src/cli/repl_event/AGENTS.md) for the event loop; capsule [`src/cli/messages/AGENTS.md`](src/cli/messages/AGENTS.md) for typed messages and WorkUnit domain snapshots; application presentation capsule [`crates/finch-ui-model/AGENTS.md`](crates/finch-ui-model/AGENTS.md) for component-owned ViewModels and pure projection ([design](docs/TUI_DESIGN.md)); project-mention composition adapter [`src/cli/mention_session.rs`](src/cli/mention_session.rs); TUI [README](src/cli/tui/README.md), [agent contract](src/cli/tui/AGENTS.md), and [facade](src/cli/tui/mod.rs); [TUI renderer](src/cli/tui/ARCHITECTURE.md), [atomic history](src/cli/repl_event/ATOMIC_HISTORY.md) |
| **`tests`**: integration tests | `tests/` | [Test guide](tests/README.md), [Brain test inventory](tests/BRAIN_TEST_INVENTORY.md) |
| **`ci`**: repository checks, agent skills, installer | `.github/` (except workflows), `scripts/`, `.agents/`, `.claude/`, `install.sh` | [Repository hygiene](docs/REPOSITORY_HYGIENE.md), [Rust toolchain](docs/RUST_TOOLCHAIN.md) |
| **`docs`**: project documentation | `docs/` (except the archive), root narrative files | [Documentation map](docs/README.md) |
| **`website`**: website and license checkout | `web/` | None yet |

**Global** files select the full gates whenever they change: the Cargo manifests and toolchain,
`src/lib.rs`, [`src/main.rs`](src/main.rs), `src/bin/` (the test supervisor), the root instruction
files, every workflow under `.github/workflows/` (including [`release.yml`](.github/workflows/release.yml);
see the [release process](CONTRIBUTING.md#release-process) and open work on signed packages and rollback,
[#119](https://github.com/darwin-finch/finch/issues/119), and newer-release notification,
[#144](https://github.com/darwin-finch/finch/issues/144)), the Brain test launchers and isolation
harness, the [Cargo slot wrapper](.agents/skills/finch-backlog/scripts/with-cargo-slot), and the
[retired-target reclaimer](.agents/skills/finch-backlog/scripts/reclaim-cargo-targets) that deletes
a worktree's generated Cargo output once Git no longer registers it.

**Excluded**: `docs/archive/` (history).

## Dependencies

Module visibility is enforced where a facade exists: child modules are private, so the `pub use`
list is the whole surface. Direction is not mechanically enforced — `scripts/seam_cost.py` reports
the edges a directory actually has, and each capsule states which of them are intended. The edges
below are measured from production `crate::` paths, either as an intended edge or as debt against
the intended
direction. The intended `scripts/check_subsystems.py` enforcement does not yet exist in this
repository, so these declarations are currently reviewed against `scripts/seam_cost.py` evidence
instead of a subsystem CI gate. At the module level, nearly every top-level module still belongs
to one strongly connected component (see the [snapshot](#snapshot)).

Two-way edges that block the next extractions, one import each way. `vm` has no outgoing edge
since [#584](https://github.com/darwin-finch/finch/issues/584). The former `runtime` ↔ `brain`
loop is gone: production `crates/finch-runtime` cannot import `finch-brain` (there is no
runtime scheduler module and no `RunId` import). `crates/finch-brain/src/store.rs` imports `finch-runtime`,
which is the intended direction (runtime layer 2, brain layer 4).

Runtime's remaining outgoing edges are now only extracted-crate dependencies. MCP transport and
editor-backed artifact proposals are application-injected ports, and the worksheet allocation
bound used by runtime host I/O and the CLI preview is owned behind runtime's flat facade. Runtime
does not import `cli`, `theme`, `tools`, or another root-package implementation module.

Memory has no two-way edge and no production `crate::` import. Callers inject
`EmbeddingEngine`; `models::neural_embedding` owns ONNX Runtime (`ort`), `tokenizers`, and
`hf_hub` download/load. Program-definition mapping lives in the composition adapter
[`src/program_registry.rs`](src/program_registry.rs).

The former `tools` knot is broken (issue #872): the tool surface the whole application layer
shares lives in the dependency-free `crates/finch-tools-api` crate — `Tool`/`ToolRegistry`, typed
requests and results, the permission/approval policy, `ExecutionEffect` (re-exported by
`programs`), `VmEffectEnvelope` (re-exported by `runtime`), and the tool-round protocol — while the
executor, concrete tools, MCP, todo, and diagnostics stay with the composition root.
`src/tools/types.rs` and `src/tools/permissions.rs` are re-export shims with zero `crate::`
imports; application-bound per-call state (session mode, daemon effect-audit authority) reaches
tools through injected ports. `ipc` ↔ `server` and `claude` ↔ `providers` remain loops. Brain,
runtime, server, and IPC extraction is no longer blocked by `tools`.

## Runtime reference

Descriptive material moved from the root instructions. It describes `main`; it is not evidence that
every configuration or provider combination has passed conformance.

### Composition sketch

```
CLI / query client
    ↓
generation boundary (finch-generation) via src/generators adapters
    ↓
provider transport (finch-providers) and/or experimental local generator
    ↓
typed runtime + capability broker for program effects
```

#### Module docs

| Component | Module Doc |
|-----------|-----------|
| Local model loader | `src/models/unified_loader.rs` · `src/models/ONNX.md` |
| Deferred LoRA path | `docs/AUTOMATIC_TRAINING.md` · `src/models/LORA.md` |
| Router | `src/router/ROUTING.md` |
| TUI Renderer | `src/cli/tui/ARCHITECTURE.md` |
| Tool Execution & Permissions | `src/tools/EXECUTION.md` |
| Claude Client | `src/claude/CLIENT.md` |
| Context Assembly | `src/context/ASSEMBLY.md` |
| Configuration | `src/config/CONFIGURATION.md` |
| License System | `src/license/LICENSING.md` |

### Weighted feedback

Three historical weight tiers are retained for explicit feedback: high (10x), medium (3x), normal (1x). `Ctrl+G` = good, `Ctrl+B` = bad. Feedback is private durable data; it does not trigger training.

### Local backend investigation

The source contains ONNX Runtime and Candle loaders. Historical backend experiments are recorded in
`docs/MODEL_BACKEND_STATUS.md`, but that document is not end-to-end routing or conformance evidence.

### Storage layout

```
~/.finch/
├── config.toml          # User config
├── adapters/            # Preserved legacy adapters; not loaded automatically
├── feedback.jsonl       # Private explicit feedback; never a training trigger
├── training_queue.jsonl # Preserved legacy queue; not processed automatically
├── metrics/             # Usage metrics
├── usage/               # Per-Brain session token burn checkpoint (status-line readout)
├── notice_state.toml    # Licence-notice bookkeeping; kept out of config.toml (#76)
├── tool_patterns.json   # Approved tool patterns
├── sessions/            # Leftover legacy UUID transcripts; not the current resume model
└── brains/              # Named Brain event logs and state (resume with `finch attach`)

~/.cache/huggingface/hub/  # Base models (HF standard)
```

### Operating modes

- **Interactive REPL:** `finch` or `finch attach <brain-name>`
- **Single query / pipe:** `finch query "..."` or `echo "..." | finch`
- **Foreground HTTP server:** `finch daemon` (default `127.0.0.1:8000`)
- **Managed background daemon:** `finch daemon-start` (default `127.0.0.1:11435`)
- **Restricted remote Brain TLS listener:** configured default `0.0.0.0:11436`; opened only when
  service advertisement is enabled
- **Direct typed programs:** `finch --forth`, `finch --lisp`, and `finch --exec`

Brain and daemon tests must use the isolated launchers and kernel-assigned endpoints.

### Technology stack

- **Language:** Rust (memory safety, performance, Apple Silicon support)
- **ML frameworks in source:** ONNX Runtime (`ort` crate) and Candle
- **Async:** Tokio
- **HTTP server:** Axum (`/v1/chat/completions`, `/v1/models`, `/v1/messages`, and Finch-specific
  routes; not the full OpenAI API and not the Responses API)
- **TUI:** Ratatui + crossterm
- **Key deps:** `hf-hub`, `tokenizers`, `indicatif`, `sysinfo`

Provider profile variants and local model repositories are defined in source. Treat the model
catalog, setup choices, and loaders as configuration surfaces—not claims that each combination has
passed conformance.

## Invariants

Invariants are stated once, in [`AGENTS.md`](CLAUDE.md#invariants) (via its `CLAUDE.md` target),
next to the tests that prove them. This index only maps them to owners:

| Invariant group | Owning subsystem |
|-----------------|------------------|
| [Security](CLAUDE.md#security): peer permissions, read-only bash, constitutional constraints | Tools and authority |
| [Security](CLAUDE.md#security): license key parsing | Config, context, license |
| [Routing](CLAUDE.md#routing) | Memory and local models |
| [TUI](CLAUDE.md#tui) | Frontend |
| [Context](CLAUDE.md#context) | Config, context, license |
| [GUI accessibility](CLAUDE.md#gui-accessibility) | Tools and authority |
| [Test isolation](CLAUDE.md#testing-mandatory) | Tests; Brain and backend |

Some invariants cite tests that no longer exist or no longer exercise the current path
([#571](https://github.com/darwin-finch/finch/issues/571)); the GUI accessibility gaps are
[#453](https://github.com/darwin-finch/finch/issues/453) and
[#454](https://github.com/darwin-finch/finch/issues/454). An invariant moves into a subsystem's
local instructions only with proof that every agent working there still receives it.

## Documentation status

The [documentation map](docs/README.md) classifies project-wide documents as current, developer
reference, design and planning, or historical. When any document conflicts with source or tests,
source and tests win. Documents the map does not classify, including most co-located
`src/**/*.md` (it lists only the configuration, tool execution, context assembly, and TUI
documents), [OpenAI transport](docs/OPENAI_TRANSPORT.md), and
[Rust toolchain](docs/RUST_TOOLCHAIN.md), are qualified by hand here.

Known stale or unsupported claims in current-looking documents, awaiting repair in
[#572](https://github.com/darwin-finch/finch/issues/572) unless noted:

- [Rust toolchain](docs/RUST_TOOLCHAIN.md) contradicts the `rust-version` declared in `Cargo.toml`.
- [Bootstrap loading](src/models/BOOTSTRAP.md) and [ONNX loader](src/models/ONNX.md) state
  unmeasured startup timing and treat loader variants as backend support.
- [Local model/backend status](docs/MODEL_BACKEND_STATUS.md) is a dated investigation and names a
  Cargo feature that does not exist.
- [Test guide](tests/README.md) has two build commands that bypass the Cargo slot and cites a
  workflow that does not exist.
- [Claude client](src/claude/CLIENT.md) describes a Claude-only client; it now wraps any default
  provider.
- [MCP client guide](docs/MCP_USER_GUIDE.md) makes compatibility and permission claims without
  production-boundary evidence.
- [TUI renderer](src/cli/tui/ARCHITECTURE.md) describes the removed inline-viewport renderer
  ([#442](https://github.com/darwin-finch/finch/issues/442)).

## Intended direction

Design intent, not current fact. The program, its phases, and its measurable gates are in
[#541](https://github.com/darwin-finch/finch/issues/541).

- **Facades before crates.** Done for `vm` ([#586](https://github.com/darwin-finch/finch/issues/586)): its
  child modules are private behind `pub use` re-exports. Each subsystem gets a narrow public interface with private
  implementation children, co-located instructions and documentation, and boundary tests.
  Reverse and cross-subsystem implementation imports are removed before code moves between
  crates.
- **Extraction order.** The shared typed contract is currently `finch-vm-core`; the coarse
  `finch-colisp` and `finch-coforth` frontends depend only on that Finch crate; `finch-language`
  selects a frontend and returns `ModuleVerified`; `finch-vm` executes verified modules without
  depending on either source reader. Remaining language work is staged in the
  [language implementation roadmap](docs/language/IMPLEMENTATION_ROADMAP.md).
  `finch-programs` now depends on the language facade for compilation, `finch-vm` for execution
  contracts, and `finch-tools-api` for shared effect vocabulary. `finch-programs` and
  `finch-memory` (MemTree, retrieval, TF-IDF fallback, and an embedding port, without ONNX, Candle,
  tokenizer, Hugging Face, HTTP, or TUI stacks) are extracted workspace crates.
  The [application UI-model capsule](crates/finch-ui-model/AGENTS.md) is extracted as
  `finch-ui-model` and remains available through the root `ui_model` compatibility facade:
  stable message/row identity, WorkUnit and component snapshots, semantic widget data, pure
  transcript projection (including bounded assistant-prose markdown), line measurement, and pure
  claiming layout have no outgoing subsystem edges. Terminal painting and the thin root
  `Message`/colour adapter remain above that facade.
  Application subsystems follow only after their cycles are removed and the VM and memory
  measurements justify continuing.
- **Target dependency direction**, refined during facade work:

  ```text
  language-core → colisp ┐
         ├──────→ coforth ├→ language compiler
         └───────────────┘          ↓
                    language-core → vm → programs → brain

  memory          tools-api
     ↓                ↓
     └──── runtime ───┘
              ↓
           transport ← provider
              ↓
  ui-model → tui     finch composition root
  ```

- **One binary.** The root package stays the only composition root and produces the single
  `finch` binary, with one workspace lockfile and shared build caching.
- **One language contract.** CoLisp and CoForth expose the same static type, ownership, concept,
  effect, and metaprogramming facilities and lower them to the same verified typed IR. Ordinary
  parameters borrow; a taking parameter accepts an ownership carrier, automatically moving a
  unique value or retaining a shared value. Unique/shared/weak heap policies remain library-defined
  types over compiler-enforced move, copy, borrow, and deterministic-drop hooks. Static and dynamic
  ownership/concept dispatch stay explicit so compilation remains bounded and native lowering does
  not depend on source-language inference. `Result`/`Option` remain ordinary library variants;
  thrown values propagate through compiler-inferred exceptional edges, while an explicit `nothrow`
  guarantee requires handlers to consume every edge that could escape.
- **No junk drawer.** Each port or data type belongs to its consuming domain; there is no generic
  contracts crate, and not every directory becomes a crate.
- **Mechanical moves.** Code moves never carry behavior, wire, persistence, checkpoint, or schema
  changes.

Related design documents, intent rather than evidence:
[shared program runtime](docs/SHARED_PROGRAM_RUNTIME_PLAN.md),
[Brain convergence](docs/BRAIN_CONVERGENCE_PLAN.md),
[VM-native agent runtime](docs/VM_NATIVE_AGENT_RUNTIME_PLAN.md),
[Finch language design](docs/language/FINCH_LANGUAGE_DESIGN.md),
[ticketing plugins](docs/TICKETING_PLUGINS.md),
[model cost, routing, and review loops](docs/MODEL_COST.md).

## Open questions

- Where the provider graph should be built once; the REPL path builds it twice today.
- Whether `poset` belongs with programs or with Brain planning.
- Which subsystem owns `planning`, `agent`, and `review`, which sit between providers and Brain.
- Whether implement vs review vs `/plan` personas bind to named provider profiles now, or wait for a specialized review harness with a frozen cache prefix.

## Snapshot

Measured at commit `cb39ea0f`; re-derive before relying on these numbers.

- Tracked Rust under `src/`: 275 files, about 268,000 lines. Largest modules by lines: `cli`
  (47 files, 63,731), `brain` (7, 24,773), `vm` (15, 23,356), `runtime` (10, 23,121),
  `providers` (14, 22,178), `server` (9, 18,987), `tools` (38, 16,322), `ipc` (8, 13,631).
- Strongly connected component: 23 modules (`brain`, `claude`, `cli`, `client`, `config`,
  `daemon`, `generators`, `ipc`, `llms`, `local`, `memory`, `models`, `node`, `oauth`, `poset`,
  `programs`, `providers`, `router`, `runtime`, `server`, `tools`, `training`, `vm`). Method:
  top-level-module edges from production `crate::` paths, with `#[cfg(test)] mod tests` blocks
  removed. A different test-exclusion heuristic gives 25; treat the size as approximate.
- Subsystem level, at commit `74866238`: 61 cross-subsystem edges, 35 allowed (`depends_on`)
  and 26 recorded as debt at the time.
- After [#584](https://github.com/darwin-finch/finch/issues/584): `vm` has no outgoing edge (59 edges,
  35 allowed, 24 debt) and is no longer part of the module-level component.
