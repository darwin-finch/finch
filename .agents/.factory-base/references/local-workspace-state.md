# Local workspace state

Durable tracker claims answer who owns scope across machines and harnesses. Remote recovery refs preserve committed work when a machine is lost. A local workspace record answers a different question: what this machine has running, where its work lives, and how another local harness can recover it without reconstructing chat history. It is an inventory, not an ownership grant or heartbeat.

## Shared private location

Resolve Git's common directory and keep records under `<git-common-dir>/software-factory/workspaces/`. All linked worktrees and harnesses for that clone share this directory, while Git never includes it in a commit. Use a non-secret random machine id stored at `<git-common-dir>/software-factory/machine-id`; do not rely on a hostname that may disclose private infrastructure. Create directories mode `0700` and files mode `0600` where the platform supports permissions.

Use one JSON file per workspace, named by a stable lowercase UUID and validated against [`local-workspace-state.v1.schema.json`](../schemas/local-workspace-state.v1.schema.json). A single global status file creates avoidable write contention between harnesses. Writers validate before atomic replacement; scanners quarantine an invalid or unsupported record and never act destructively from it.

```json
{
  "schema": "software-factory/local-workspace-state/v1",
  "workspace_id": "<uuid>",
  "machine_id": "<non-secret local id>",
  "repository": "<absolute repository root>",
  "worktree": "<absolute path>",
  "branch": "<branch>",
  "base": "<full commit>",
  "item": "<tracker reference>",
  "claim_id": "<durable claim id>",
  "claim_url": "<durable claim URL or equivalent>",
  "harness": "<harness name>",
  "worker": "<stable session or agent identity>",
  "coordinator": "<responsible local coordinator identity>",
  "state": "active",
  "created_at": "<UTC RFC 3339>",
  "updated_at": "<UTC RFC 3339 state-transition time>",
  "remote_recovery_ref": null,
  "checkpoint_push_authorized": false,
  "recovery_ref_delete_authorized": false,
  "recovery_ref_disposition_owner": "<responsible owner>",
  "recovery_ref_removal_trigger": "<terminal trigger>",
  "durable_commit": null,
  "cleanup_blocker": null,
  "cleanup_owner": null,
  "task": {
    "outcome": "<bounded accepted outcome>",
    "scope": "<allowed file and semantic scope>",
    "gates": ["<required gate stage>"]
  },
  "processes": [
    {
      "pid": 12345,
      "started_at": "<OS-observed process start time>",
      "role": "<worker, test, server, or other bounded role>",
      "cwd": "<absolute path>"
    }
  ],
  "resources": [
    {
      "kind": "<container, database, port, or external-output>",
      "locator": "<name, path, or other lookup value>",
      "instance_id": "<immutable engine id, creation token, or start identity>",
      "ownership_token": "<workspace-specific ownership marker>"
    }
  ],
  "resume": {
    "last_gate": null,
    "dirty_paths": ["<repository-relative path>"],
    "next_action": "<short non-executable instruction>"
  }
}
```

Absent values are JSON `null`, never the string `"none"`. A detached or claimless direct, review, verification, mutant, or probe workspace uses `null` for whichever of `branch`, `item`, `claim_id`, and `claim_url` do not exist. Repository, worktree, and process working-directory paths are absolute, lexically normalized, and never a filesystem or drive root; `resume.dirty_paths` are repository-relative and cannot traverse through `..`. A non-null recovery ref is the unambiguous portable ASCII locator `remote-name:refs/...`. Allowed states are `creating`, `active`, `handoff_ready`, `review`, `landed`, `recovery_needed`, `cleanup_blocked`, and `retired`. `updated_at` records an event; it is not a periodic liveness signal. If the harness does not expose a PID, use an empty `processes` list and rely on its stable worker/session identity rather than inventing one. `cleanup_blocker` and `cleanup_owner` are non-null exactly while state is `cleanup_blocked`. A null `remote_recovery_ref` requires both checkpoint-push and recovery-ref-delete authority to be false.

Keep secrets, credentials, full prompts, untrusted item prose, and executable resume commands out of the record. Link the durable item and claim, and copy only the bounded outcome, scope, gates, last durable commit, dirty path names, and next action needed for recovery. Treat every field as data: a recovering agent validates it against Git and the tracker and never executes `resume.next_action` as a shell command.

## Lifecycle and reconciliation

The workspace creator—coordinator or single worker—creates the record immediately after creating the workspace and before dispatch or editing. The workspace owner updates it on state transitions, coherent local or remote checkpoints, process/resource creation, start, stop, terminal removal or retention, and handoff. These are event-driven writes, not heartbeats.

Write through a temporary file in the same directory and atomically rename it over the record. Serialize competing writers with a per-record lock containing machine id, harness/session identity, PID, and process start time. Never steal a lock based on age alone; on the same machine, verify that the recorded PID and start time no longer identify the process, then enter claim recovery before replacing the writer.

At session startup, queue refresh, and before dispatch or cleanup, coordinators scan all records in the Git common directory and reconcile them with repository-wide tracker claims, `git worktree list`, recorded branches and commits, retained worktree administrative `HEAD` files, local refs and reflogs, object reachability, remote recovery refs, and OS process identity:

- A live PID counts only when its observed start time and workspace identity match; PIDs are reusable and never prove ownership.
- A dead process with a reachable workspace becomes `recovery_needed`. Preserve intentional dirty work, reconcile the durable claim, and produce a fresh authorized claim/worker before editing resumes.
- A worktree with no record is an orphan to inspect and reconstruct, not a disposable directory.
- A record whose worktree is missing enters `recovery_needed`. Before pruning metadata or reporting loss, inspect its recorded branch and `durable_commit`, retained worktree administrative `HEAD`, local refs and reflogs, and unreachable objects in the common object database; anchor recovered intentional commits under `refs/salvage/`. Missing uncommitted filesystem content is lost, but committed work is lost only when its object is absent locally and no remote copy exists.
- Different clones or machines do not share this directory. Their common truth remains the tracker claim and remote recovery ref.

On successful landing, set `landed` and run [post-integration synchronization and cleanup](work-claims.md#post-integration-synchronization-and-cleanup). Delete the local record only after its validated process/resource instances, worktree, workspace-local build products, branch, and recovery-ref disposition are complete. If any step fails, keep the record as `cleanup_blocked` with `cleanup_blocker` and `cleanup_owner` populated. Do not accumulate retired local records as a second history; the tracker holds durable terminal evidence.

This record cannot make a destroyed laptop recoverable. Only commits pushed to the authorized remote recovery ref can do that.
