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
that it links every module document named in `AGENTS.md`, that it cites no historical or archived
document, and that design documents appear only under intended direction or open questions. It does
not verify the prose or the cited symbols.

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
([`src/brain/store.rs`](src/brain/store.rs)). HTTP routes are defined in
[`src/server/handlers.rs`](src/server/handlers.rs); the IPC wire schema is
[`schema/finch_ipc.capnp`](schema/finch_ipc.capnp).

## Subsystems

Each row names the top-level modules a subsystem owns and its authoritative local documentation.
"None yet" marks a subsystem without local documentation; the subsystem program adds one before
code moves. Documents with known stale claims are flagged in
[documentation status](#documentation-status).

| Subsystem and responsibility | Modules under `src/` | Local documentation |
|------------------------------|----------------------|---------------------|
| **Typed runtime** (VM, CoForth, CoLisp): parse, verify, and run typed programs under capability authority | `vm`, `lisp`, `runtime` | Language contracts compiled into the binary and given to the model: [`FINCH_VM.md`](vocabulary/language/FINCH_VM.md), [`FINCH_FORTH.md`](vocabulary/language/FINCH_FORTH.md), [`FINCH_LISP.md`](vocabulary/language/FINCH_LISP.md); reference: [typed VM migration audit](docs/TYPED_VM_MIGRATION_AUDIT.md) |
| **Programs**: durable program identity, catalog, and corpus; task-graph planning | `programs`, `poset` | None yet |
| **Memory and local models**: MemTree storage and retrieval, local model loading, routing, feedback | `memory`, `models`, `local`, `generators`, `training`, `feedback`, `router`, `logging`, `memory_status.rs`, `workbook.rs` | [Local model loader](src/models/unified_loader.rs), [ONNX loader](src/models/ONNX.md), [bootstrap loading](src/models/BOOTSTRAP.md), [deferred LoRA path](src/models/LORA.md), [router](src/router/ROUTING.md), [automatic-training status](docs/AUTOMATIC_TRAINING.md) |
| **Frontend** (CLI and TUI): commands, the interactive REPL, rendering, setup | `cli`, `main.rs`, `startup.rs`, `samples.rs` | [TUI renderer](src/cli/tui/ARCHITECTURE.md), [atomic history](src/cli/repl_event/ATOMIC_HISTORY.md) |
| **Brain and backend**: durable named Brains, the HTTP server and runner, daemon lifecycle | `brain`, `server`, `daemon`, `client`, `agent`, `review`, `registry` (migration only), `graph` | None yet; see the [Brain test inventory](tests/BRAIN_TEST_INVENTORY.md) |
| **Tools and authority**: tool execution, permissions, MCP client, GUI automation | `tools` (including `tools/mcp`) | [Tool execution and permissions](src/tools/EXECUTION.md), [MCP client guide](docs/MCP_USER_GUIDE.md), [macOS GUI automation](docs/MACOS_GUI_AUTOMATION.md) |
| **Providers**: provider graph and wire transports, OAuth, planning prompts | `providers`, `claude`, `oauth`, `llms`, `planning` | [Claude client](src/claude/CLIENT.md), [OAuth boundary](docs/OAUTH.md), [ChatGPT subscription transport](docs/CHATGPT_SUBSCRIPTION_TRANSPORT.md), [OpenAI transport](docs/OPENAI_TRANSPORT.md) |
| **IPC, transport, node**: Cap'n Proto IPC, node identity, service discovery | `ipc`, `node`, `network`, `service` (mDNS), `node_name.rs` | None yet; wire schema in [`schema/finch_ipc.capnp`](schema/finch_ipc.capnp) |
| **Config, context, license, metrics**: configuration, instruction loading, licensing, metrics | `config`, `context`, `license`, `metrics`, `monitoring`, `errors.rs` | [Configuration](src/config/CONFIGURATION.md), [context assembly](src/context/ASSEMBLY.md), [licensing](src/license/LICENSING.md) |
| **Tests**: integration tests and the isolation supervisor | `tests/`, `src/bin/finch-test-supervisor.rs` | [Test guide](tests/README.md), [Brain test inventory](tests/BRAIN_TEST_INVENTORY.md) |
| **CI and scripts**: workflows, repository checks, the shared Cargo slot | `.github/workflows/`, `scripts/`, the [Cargo slot wrapper](.agents/skills/finch-backlog/scripts/with-cargo-slot) | [Repository hygiene](docs/REPOSITORY_HYGIENE.md), [Rust toolchain](docs/RUST_TOOLCHAIN.md) |
| **Release**: tagged binary builds | [`release.yml`](.github/workflows/release.yml) | [Release process](CLAUDE.md#release-process); open work: signed packages and verified rollback ([#119](https://github.com/darwin-finch/finch/issues/119)), newer-release notification ([#144](https://github.com/darwin-finch/finch/issues/144)) |

`src/evolution/mod.rs` is tracked but not compiled
([#573](https://github.com/darwin-finch/finch/issues/573)).

## Dependencies

Module boundaries are not enforced today. Nearly every top-level module belongs to one strongly
connected component of production `crate::` imports (see the [snapshot](#snapshot)), so any of them
can reach any other.

Two-way edges that block the first extractions, one import each way:

| Edge | Evidence |
|------|----------|
| `vm` ↔ `runtime` | `src/vm/runtime.rs` imports `runtime::fiber::CpuFiberScheduler`; `src/runtime/outcome.rs` imports `vm` |
| `vm` ↔ `programs` | `src/vm/runtime.rs` imports `programs::ProgramLanguage`; `src/programs/mod.rs` imports `vm` |
| `programs` ↔ `runtime` | `src/programs/corpus.rs` takes `&ProgramRuntime`; `src/runtime/outcome.rs` imports `programs` |
| `runtime` ↔ `brain` | `src/runtime/scheduler.rs` imports `brain::store::RunId`; `src/brain/store.rs` imports `runtime` |
| `models` ↔ `cli` | `src/models/bootstrap.rs` imports `cli::OutputManager`; `src/cli/setup_wizard.rs` imports `models` |

Memory has no two-way edge. Its only production import is `crate::programs`
(`memory::program_registry` and `MemorySystem::save_lisp_define`), so it joins the component through memory → programs → runtime and
vm. Its separate extraction blocker is heavy dependencies: `memory::neural_embedding` uses ONNX
Runtime (`ort`), `tokenizers`, and `hf_hub` directly.

The application layer is knotted mostly through `tools`: `src/tools/types.rs` imports `cli`,
`runtime`, `server`, `local`, and `models` types, and each of those imports `tools` back. `ipc`
↔ `server` and `claude` ↔ `providers` add further loops. This is why the program forbids
extracting Brain, runtime, server, and IPC as one change.

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
- [OpenAI transport](docs/OPENAI_TRANSPORT.md) names a superseded IPC generation; the current one
  is `IPC_PROTOCOL_VERSION` in `src/ipc/mod.rs`.
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

- **Facades before crates.** Each subsystem gets a narrow public interface with private
  implementation children, co-located instructions and documentation, and boundary tests.
  Reverse and cross-subsystem implementation imports are removed before code moves between
  crates.
- **Extraction order.** `finch-vm` (CoForth and CoLisp frontends, typed IR, verifier,
  interpreter, capability and effect contracts, CPU fiber scheduler), then `finch-programs`
  (depends only on `finch-vm`), then `finch-memory` (MemTree, retrieval, TF-IDF fallback, and an
  embedding port, without ONNX, Candle, tokenizer, Hugging Face, HTTP, or TUI stacks).
  Application subsystems follow only after their cycles are removed and the VM and memory
  measurements justify continuing.
- **Target dependency direction**, refined during facade work:

  ```text
  vm          memory          tools-api
  ↓              ↓                ↓
  programs       └──── runtime ────┘
     ↓                 ↓
   brain            transport ← provider
                          ↓
  ui-model → tui       finch composition root
  ```

- **One binary.** The root package stays the only composition root and produces the single
  `finch` binary, with one workspace lockfile and shared build caching.
- **No junk drawer.** Each port or data type belongs to its consuming domain; there is no generic
  contracts crate, and not every directory becomes a crate.
- **Mechanical moves.** Code moves never carry behavior, wire, persistence, checkpoint, or schema
  changes.

Related design documents, intent rather than evidence:
[shared program runtime](docs/SHARED_PROGRAM_RUNTIME_PLAN.md),
[Brain convergence](docs/BRAIN_CONVERGENCE_PLAN.md),
[VM-native agent runtime](docs/VM_NATIVE_AGENT_RUNTIME_PLAN.md),
[typed Lisp/Forth capabilities and JIT](docs/TYPED_LISP_FORTH_CAPABILITY_JIT_PLAN.md).

## Open questions

- Where the provider graph should be built once; the REPL path builds it twice today.
- Whether `poset` belongs with programs or with Brain planning.
- Which subsystem owns `planning`, `agent`, and `review`, which sit between providers and Brain.
- How `tools` splits into a dependency-free API and application-bound implementations.

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
