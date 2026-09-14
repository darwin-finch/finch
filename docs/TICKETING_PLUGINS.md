# Ticketing plugins and setup contributions

Design intent, not current fact. Tracked as
[#693](https://github.com/darwin-finch/finch/issues/693) and children.
Pyramid's claim HTTP is a *backend*; this document owns Finch's tool and plugin
surface. Do not copy that surface into Pyramid `DESIGN.md`.

## Why this is Finch's

Linear, Jira, Asana, GitHub Issues, and Pyramid are **trackers a Finch instance
talks to**. The LLM needs tools. The setup wizard needs fields. The binary
already has a tool registry and an MCP *client*. Those live here.

Pyramid records: Finch is a client of a versioned HTTP contract
(PYR-771); `pyr` remains for other harnesses; a claim still provisions one
workspace. It does not record MCP-shaped plugins or wizard manifests.

## Compiled-in plugins, MCP-shaped interface

Plugins are linked into `finch` and enabled or disabled in config. No `dlopen`,
no extra process per tracker. Disk is cheap; process sprawl is not.

The **interface** is MCP-shaped (name, description, JSON schema, call) so
bundled plugins and user-installed MCP servers join the same registry and
permission path (`src/tools/registry.rs`, `src/tools/mcp`). External MCP is
still the client for *other people's* servers
([#694](https://github.com/darwin-finch/finch/issues/694)). Do not ship a
ticketing MCP so Finch can STDIO to itself.

Disabled plugins skip registration and must not construct HTTP clients until
enabled and first used. Startup cost is then schema registration, not Jira
login. Binary size grows by REST adapters (small next to Candle/ONNX). RSS
grows when a plugin allocates.

## LLM tools: core, then omit

The model gets a short list of tools this adapter actually has.

**Always** (plugin enabled):

| Tool | Meaning |
|------|---------|
| `tickets.mine` | This identity's work. Pyramid: ready frontier. Linear/GitHub/Asana: assigned. Jira: assignee or stored JQL. |
| `tickets.show` | Read one ticket. |
| `tickets.comment` | Talk on it. |

**Optional — omit from the registry if the tracker cannot:** `claim`/`release`,
`child`, `evidence`, `complete`/`close`. Do not stub `"unsupported"`.

**Search is not core.** If present, `tickets.search` whose description names
JQL, Linear filter, GitHub search, or Pyramid query. No portable query AST.

**Not tools:** team/project discovery (setup wizard). The current ticket
(session context after pull/claim).

"Go through my assigned tickets" is `tickets.mine` plus pull-after-release.
Do not push a ticket into a live Brain turn.

## First-party adapters

| Tracker | Issue |
|---------|--------|
| Pyramid | [#695](https://github.com/darwin-finch/finch/issues/695) — client of PYR-771; Pyramid source stays closed |
| Linear | [#696](https://github.com/darwin-finch/finch/issues/696) |
| Jira | [#697](https://github.com/darwin-finch/finch/issues/697) |
| Asana | [#698](https://github.com/darwin-finch/finch/issues/698) |
| GitHub Issues | [#699](https://github.com/darwin-finch/finch/issues/699) — tracker, not only forge intake |

## Setup wizard contributions

VS Code's useful half is `contributes.configuration`, not a webview per
extension. Each plugin declares:

- id, title, default enabled
- fields: `string`, `url`, `secret`, `bool`, `enum`
- optional probe (whoami, list tools, test connection)
- **auth** ([#700](https://github.com/darwin-finch/finch/issues/700)): fields;
  OAuth/device-code (plugin implements start/poll/exchange; wizard shows URL +
  user code — generalize `setup_wizard/chatgpt_recovery.rs`); and/or an
  installed CLI (`gh auth token`) as a credential *source*. Runtime tools then
  speak HTTP with the stored secret and should not keep requiring that CLI.

`finch setup` enumerates contributions and renders one dialog. Plugins do not
draw ratatui. Today's hardcoded provider catalog
(`src/cli/setup_wizard/catalog.rs`) should become the same API.

MCP servers (#694) use the same field/auth shapes for command, args, env.

## Pull loop vs Pyramid

An idle attached Finch **pulls** the next item from `tickets.mine` (Pyramid:
ready + claim). The TUI shows which ticket this Brain holds. Identity is the
daemon's configured tracker account, not a new CLI login per terminal.

Workspace isolation, claim-owned directories, and sandbox ([#434](https://github.com/darwin-finch/finch/issues/434))
remain Finch execution. Pyramid still names the workspace contract when the
backend is Pyramid.

## Industry pattern: git as the bus

Corporate stacks usually do **not** give the model a ticket API. They wire
GitHub/GitLab to Jira/Azure Boards and infer work from branches and PRs:

- Branch or commit contains `DEV-123` → ticket "in progress"
- PR opened → "in review", manager tagged, diff linked on the board
- Copilot/Codex Agent HQ: a mission-control UI over those sessions
- Azure DevOps MCP: assign a work item to an agent; it reads the ticket and
  reports status
- Codex desktop: isolated git worktrees, parallel agents, one PR each

That is a **git-hook workflow sitting on Jira**. Status is a field updated by
the forge. The agent often never calls `tickets.comment`.

We take the mechanical pieces and refuse the bus:

| Steal | Refuse |
|-------|--------|
| Ticket id on the branch/PR as *correlation* | Silent workflow transitions from git |
| Isolated worktrees, one task each | Using the human's `~/repos` layout |
| Parallel agents, human review of PRs | Unlimited review queue |
| MCP for trackers we do not bundle (e.g. Azure DevOps) | MCP subprocess per first-party tracker |
| PM sees which agent holds which item | A second "Agent HQ" product |

Finch talks to the tracker with `tickets.*`. Git/PR is **evidence** the claim
returns, not the coordinator. Pyramid (when it is the backend) already treats
forge events as cited obligations, not as a second Jira. Smart-commit plugins
that move Jira columns are a Linear/Jira adapter *option* behind an explicit
command, never an implied side effect of `git push`.

Azure Boards is not in the first-party adapter list. If someone needs it, they
enable an MCP server (#694) or we add an adapter later the same way as Jira.
