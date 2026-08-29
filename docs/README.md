# Finch documentation map

Documentation status matters in this repository. Finch changed rapidly, and older documents often
describe a proposal, a removed name, or an earlier implementation. Start with the **current** group
and verify integration-sensitive claims against source and tests.

## Current user documentation

These documents are intended to describe the current `main` branch:

- [Project overview and source quick start](../README.md)
- [Contributing and attribution](../CONTRIBUTING.md)
- [MCP client guide](MCP_USER_GUIDE.md)
- [macOS GUI automation permissions](MACOS_GUI_AUTOMATION.md)
- [Automatic-training status](AUTOMATIC_TRAINING.md) — disabled; issue
  [#139](https://github.com/darwin-finch/finch/issues/139)
- [Legacy ChatGPT subscription configuration](chatgpt-subscription-provider.md) — migration note,
  not a supported subscription-auth flow

For exact CLI flags, use `finch --help`. For exact configuration fields and routes, use the
implementation references below.

## Developer and implementation reference

These are implementation-oriented references. Co-located module documents generally have narrower
scope and are more reliable than the older project-wide guides, but source and tests remain
authoritative.

- [Configuration types](../src/config/settings.rs),
  [provider profiles](../src/config/provider.rs), and
  [configuration notes](../src/config/CONFIGURATION.md)
- [Z.ai GLM-5.3-Flash transport](ZAI_TRANSPORT.md) — implemented contract; live conformance remains
  gated by [#196](https://github.com/darwin-finch/finch/issues/196)
- [HTTP route definitions](../src/server/handlers.rs)
- [Tool execution and permissions](../src/tools/EXECUTION.md)
- [Context assembly](../src/context/ASSEMBLY.md)
- [TUI internals](../src/cli/tui/ARCHITECTURE.md)
- [Typed VM migration audit](TYPED_VM_MIGRATION_AUDIT.md)
- [Local model/backend status](MODEL_BACKEND_STATUS.md) — dated backend investigation, not
  end-to-end routing or conformance evidence; see
  [#74](https://github.com/darwin-finch/finch/issues/74) and
  [#98](https://github.com/darwin-finch/finch/issues/98)
- [Repository hygiene guard and fixture policy](REPOSITORY_HYGIENE.md)

## Design and planning documents

These documents express intended direction, research, or work in progress. They are not feature
promises and must not be cited as proof that a behavior is implemented.

- [Roadmap](ROADMAP.md)
- [MCP client implementation plan](MCP_CLIENT_IMPLEMENTATION_PLAN.md)
- [Shared program runtime plans](SHARED_PROGRAM_RUNTIME_PLAN.md)
- [Brain convergence plan](BRAIN_CONVERGENCE_PLAN.md)
- [VM-native agent runtime plan](VM_NATIVE_AGENT_RUNTIME_PLAN.md)
- [Typed Lisp/Forth capability and JIT plan](TYPED_LISP_FORTH_CAPABILITY_JIT_PLAN.md)
- [Semiotic transition research note](SEMIOTIC_TRANSITION_SYSTEM.md)
- [Two programmers, one VM](TWO_PROGRAMMERS.md)
- [Template parsing design](DESIGN_TEMPLATE_PARSING.md)
- [Training framework design](TRAINING_FRAMEWORK.md) — historical design; automatic training is
  disabled

## Historical and archived narrative

These files are retained as project history or prior product narrative. They are non-authoritative
and may contain obsolete names, versions, commands, metrics, or capability claims:

- [Former user guide](USER_GUIDE.md)
- [Former project-wide architecture](ARCHITECTURE.md)
- [Former configuration guide](CONFIGURATION.md)
- [Former daemon guide](DAEMON_MODE.md)
- [Former development guide](DEVELOPMENT.md)
- [Former expanded TUI architecture](TUI_ARCHITECTURE.md)
- [Former MCP architecture status](MCP_ARCHITECTURE.md)
- [Adapters versus providers note](ADAPTERS_VS_PROVIDERS.md)
- [LLM dialogs implementation note](LLM_DIALOGS.md)
- [MemTree console design](MEMTREE_CONSOLE.md)
- [Prompt suggestions guide](PROMPT_SUGGESTIONS.md)
- [Tool confirmation overview](TOOL_CONFIRMATION.md)
- [Hacker News draft](HN_POST.md)
- [Reddit drafts](REDDIT_POST.md)

The archive label preserves context; it does not certify that the described behavior ever shipped.
When a historical document conflicts with the README, generated CLI help, current source, or tests,
the current evidence wins.
