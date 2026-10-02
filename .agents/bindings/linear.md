# Binding: Linear

Linear has **no official CLI**. This binding uses the community tool **[schpet/linear-cli](https://github.com/schpet/linear-cli)** (`linear` on PATH), package `@schpet/linear-cli`, Homebrew `schpet/tap/linear`. Pin a version in the consuming repo. Do not invent flags; if a command is missing, use `linear api` with GraphQL (the CLI's documented escape hatch) or say so.

Skills speak this binding when `.agents/binding` sets `tracker: linear`. Also set:

```
tracker: linear
team: ENG
repository: owner/name
```

`team` is the Linear team key. `repository` is **this git checkout**. Linear teams are org-wide; without `repository` on both the binding and the issue, a worker will implement the wrong product. Do not infer the repo from cwd.

Install: `linear auth login`, then `linear config` in the repo (writes `.linear.toml` with `team_id`). Member API key required (not guest).

## Mapping

| Kernel | Linear (`linear` CLI) |
| --- | --- |
| item | Issue identifier `TEAM-123` |
| claim | Issue comment containing `work-claim:v1` |
| blocker | Relation type `blocked-by` / `blocks`, not a sentence in the description |
| score | Four-axis formula in the **description** ([queue.md](../references/queue.md)) |
| ready | Acceptance + owner **and** `repository: owner/name` matching this checkout; delegated work also names base SHA and authority |
| packet | Comment or worker handoff; not a substitute for the issue |
| land | GitHub PR via `linear issue pr` (wraps `gh pr create`). Squash-merge with `gh pr merge --squash` as in [github.md](github.md). Linear is not the VCS. |

When a consuming repository opts into [boundary-load evidence](../references/boundary-load.md), attach or link the separate `software-factory/boundary-load-evidence/v1` JSON in the terminal issue comment or pull request. Do not embed it in `work-claim:v1`.

## Tool: install and auth

```sh
brew install schpet/tap/linear
# or: npm install -D @schpet/linear-cli && npx linear ...
linear auth login          # API key from linear.app/settings/account/security
cd /path/to/this/repo
linear config              # writes .linear.toml (team_id, workspace)
```

Config also via `LINEAR_TEAM_ID`, `LINEAR_WORKSPACE`, or `.linear.toml`. Env beats file.

Non-interactive creates must pass flags (`-t`/`--title`, `-d`/`--description`, `--team`). A bare `linear issue create` prompts and will hang in an agent.

## Commands we actually use

Documented against schpet/linear-cli **2.6.x** (and the `issue relation` command from 1.10.0). Confirm `linear issue relation --help` on the machine before a session; do not invent synonyms.

```sh
# Inspect
linear issue view TEAM-123 --json
linear issue query --team ENG --json --limit 0
linear issue query --team ENG --state unstarted --state started --json
linear issue query --search "login bug" --team ENG --json
linear issue comment list TEAM-123 --json

# Create / update (non-interactive)
linear issue create --team ENG -t "title" -d "description" --label bug
linear issue update TEAM-123 --state "In Review"
linear issue comment add TEAM-123 --body-file claim.md

# Relations (edges). "A blocked-by B" means A waits on B.
linear issue relation add TEAM-123 blocked-by TEAM-100
linear issue relation add TEAM-123 blocks TEAM-456
linear issue relation list TEAM-123
linear issue relation delete TEAM-123 blocked-by TEAM-100

# Land surface (still git)
linear issue pr              # gh pr create with Linear URL in the body
linear issue start TEAM-123  # branch named like eng-123-... and mark started
```

`--json` preserves GraphQL field names. `issue query --json` / `issue view --json` include `inverseRelations` (blocked-by incoming). `issue mine` is the assignee's unstarted queue; the factory uses `issue query`, not `mine`, so it sees the team's READY work.

Comments: `--body-file` for markdown. `@name` in a comment does **not** mention anyone; use the member's Linear URL from `linear team members --json` / `linear user list --json` (`url` field).

If `issue relation` is missing on an older binary, upgrade. Fallback (same mutations the CLI uses):

```sh
linear api '
mutation($input: IssueRelationCreateInput!) {
  issueRelationCreate(input: $input) { success issueRelation { id } }
}' --variables-json '{"input":{"issueId":"FROM_UUID","relatedIssueId":"TO_UUID","type":"blocks"}}'
```

For `blocked-by`, the CLI swaps the two IDs and still sends `type: blocks`. Do not send a GraphQL type named `blocked-by`.

## Repo field (required)

Every delegated issue description **must** contain:

```
repository: owner/name
```

matching `.agents/binding`. Missing that line is `NEEDS_SPECIFICATION`, not a default to cwd. Optional: a Linear label `repo:owner/name` as a second filter; the description line is the source of truth.

## Ready queue

GitHub scripts (`ticket_poset.py`, `ready_queue.py --repo`) call `gh`. They do not speak Linear.

1. `linear issue query --team TEAM --json --limit 0` (and state filters you need).
2. Pipe or save JSON into `.agents/scripts/linear_ready.py --repo owner/name`.

`linear_ready.py` reports. It does not claim, comment, or start issues. Identifiers stay `TEAM-123`, not GitHub numbers.

## Land rule

Same as GitHub: review the candidate that will land; `gh pr merge --squash` with the reviewed tip in the message. `linear issue pr` only creates the PR.

## Publishing boundary

Creating, updating, or commenting on Linear is an external mutation and needs the user's confirmation of the prepared artifact, except where they already authorized the backlog loop to claim and comment as part of dispatch.
