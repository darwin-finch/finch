# Boundary-load evidence

Reasoning locality is useful when it names an architectural obligation rather than a proxy for model effort. A subsystem is **loadable** when an agent can change it using its own implementation and the public contracts of its dependencies, without opening those dependencies' private implementations. The strongest mechanical warning is therefore a **dependency implementation escape**: inspected-path evidence shows that ordinary work in subsystem A entered a declared private surface of dependency B.

File counts, tokens, repository size, retries, and navigation volume may explain an investigation. None is inherently bad, and this kernel does not combine them into a maintainability score.

## Explicit subsystem manifest

A consuming repository opts in with `.agents/subsystems.json`, validated by [`subsystem-manifest.v1.schema.json`](../schemas/subsystem-manifest.v1.schema.json). Subsystem IDs are architectural identities chosen by maintainers; the manifest never discovers them from packages or directories.

Each subsystem declares any number of discontiguous public-contract and private-implementation path roots plus its direct dependency IDs. A root matches itself and descendants. Roots are repository-relative POSIX paths. Contract and implementation roots must not overlap; ambiguous ownership is a manifest error rather than a guessed classification. A future layout that cannot be expressed honestly gets a new schema version instead of implicit precedence rules.

The manifest only states intended information-hiding boundaries. It does not make an undeclared path public, prove the facade is sufficient, or grant authority.

```json
{
  "schema": "software-factory/subsystem-manifest/v1",
  "subsystems": [
    {
      "id": "transactions",
      "contract_roots": ["src/transactions/api"],
      "implementation_roots": ["src/transactions/private"],
      "dependencies": ["rpc"]
    },
    {
      "id": "rpc",
      "contract_roots": ["src/rpc/api.py"],
      "implementation_roots": ["src/rpc/providers"],
      "dependencies": []
    }
  ]
}
```

Validate before collecting evidence:

```sh
.agents/scripts/boundary_load.py validate-manifest .agents/subsystems.json
```

## Separate evidence record

Boundary evidence is not part of `work-claim:v1`. Claims record exclusive ownership; [`boundary-load-evidence.v1.schema.json`](../schemas/boundary-load-evidence.v1.schema.json) records observations about completed work and can be stored in the terminal item/PR evidence according to the active binding.

Every path observation has three coverage states:

- `not_observed`: the collector had no usable observation; no `paths` field is permitted;
- `partial`: some paths were observed, but the collector cannot establish completeness;
- `complete`: the collector can establish that all relevant paths are represented. An empty `paths` list then means observed and none occurred.

Each observation carries its own collector, version, harness, and source. This matters because Git can completely observe modified paths while the active harness may not expose complete inspected paths. Do not promote `partial` to `complete` because a trace looks plausible.

`.agents/scripts/boundary_load.py record` derives modified paths from `git diff` and accepts inspected-path evidence only with explicit coverage and provenance. The packet also classifies the claim as `single_subsystem` or `cross_subsystem`; legitimate cross-subsystem work remains visible but is not treated as an ordinary boundary-load sample. The record snapshots path classifications and the manifest digest so later layout changes do not silently reinterpret history.

```sh
.agents/scripts/boundary_load.py record \
  --claim-id CLAIM --item ITEM --primary-subsystem transactions \
  --scope-kind single_subsystem --base BASE_SHA --candidate TIP_SHA \
  > boundary-load-CLAIM.json
```

That bootstrap record has complete Git-modified paths and `not_observed` inspected paths. A collector adds `--inspection-coverage`, `--inspected-paths`, and the four provenance options. Validate records before aggregation with `boundary_load.py validate-evidence`.

## Historical report

`.agents/scripts/boundary_load.py report` groups records by primary subsystem in completion order and compares two adjacent rolling windows. Only `single_subsystem` records whose inspected-path observation is `complete` enter the dependency-implementation escape denominator. Cross-subsystem, `partial`, and `not_observed` counts remain visible as scope and coverage diagnostics.

The report states counts, rates, and whether the recent window increased, decreased, or stayed level. Its v1 comparison key is deliberately coarse: primary subsystem plus `single_subsystem` scope. An increase is an audit candidate, not a diagnosis. The audit must still check that task mix and apparent change size stayed comparable, then distinguish a weak facade from poor ticket scope, legitimate cross-subsystem work, hidden shared state, misplaced boundaries, and other causes. Do not add a guessed task-size score merely to make the statistic look controlled.

## Completed-pass checkpoint

The latest reachable annotated Git tag under `boundary-load-audit/v1/` marks the integrated tree after the last completed audit/refactor pass. The final component is a UTC timestamp such as `boundary-load-audit/v1/20260927T120000Z`. Lightweight and unreachable tags are not checkpoints.

`boundary_load.py checkpoint` reports the current checkpoint. `boundary_load.py report RECORDS --since-checkpoint` still reads full history for rolling baselines, but only marks an increase as newly actionable when at least one eligible claim is newer than the checkpoint. The checkpoint is a suppression boundary for repeated periodic audits, not permission to ignore older evidence or skip tracker deduplication.

An audit or a batch of filed tickets does not complete a pass. Create the next annotated tag only after a clean audit or after all accepted corrective work has landed and post-change evidence is recorded. Tag creation and push require explicit authority; never move an existing checkpoint.

Audit findings enter the ordinary path: `$codebase-audit` evidence → `$ticket-creation` proposal → `$backlog-grooming` contract and normal cost score → `$implement` → `$review`. No report creates a ticket, marks work READY, or reserves maintenance capacity. The existing 1–5 cost score is ordinal and must not be accumulated as currency.

An evidence-backed refactoring proposal names the triggering observations, architectural hypothesis, proposed boundary change, preserved behavior/API invariants, and expected change in dependency-implementation escapes. Post-change evidence may test that expectation. Replaying historical tasks with fresh agents is optional and remains premature until ordinary evidence shows that its cost would change a decision.

## Harness bootstrap adapter

`.agents/scripts/boundary_path_hook.py` is an optional `PostToolUse` command hook for Codex, Claude Code, and Grok Build. It accepts the snake_case hook fields used by Codex and Claude Code and the camelCase native fields used by Grok Build. Structured `Read` or MCP navigation inputs can therefore append repository-relative paths to the same neutral trace. Shell commands and unrecognized tool inputs are not normalized. The adapter always emits `partial` inspection traces, which do not enter rate denominators.

```json
{
  "hooks": {
    "PostToolUse": [{
      "matcher": "Read",
      "hooks": [{
        "type": "command",
        "command": "python3 .agents/scripts/boundary_path_hook.py"
      }]
    }]
  }
}
```

Place the hook in the harness's supported project or user configuration and set `SOFTWARE_FACTORY_BOUNDARY_TRACE` in the harness process to an ignored local file. Codex uses `hooks.json` or hook configuration, Claude Code uses `.claude/settings.json`, and Grok Build uses `.grok/hooks/*.json` while also accepting Claude hook files. Narrow the matcher to the structured read/navigation tools actually used in that environment. The adapter resolves paths against an explicit `SOFTWARE_FACTORY_REPOSITORY_ROOT`, a harness workspace/project root, or Git's top level, in that order; paths outside that root are discarded.

Pass the trace to `boundary_load.py record` with coverage `partial`, collector `structured-post-tool-use`, version `1`, and harness `codex`, `claude-code`, or `grok-build`. `codex_boundary_hook.py` remains as a compatibility entry point for existing Codex configuration.

This proves the capture-to-record path without pretending that Codex, Claude Code, OpenCode, and other harnesses expose one common trace API. Harness-specific adapters may emit the same neutral trace format; the evidence schema records which one did so.
