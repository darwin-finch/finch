# Finch execution adapter

This reference connects the harness-neutral Software Factory workflow to Finch's repository-owned
gate and cleanup tools. Every harness uses these repository-relative entry points; harness adapters
under `.agents/harnesses/` resolve model lanes and dispatch mechanics, not different build recipes.

## Gates and Cargo isolation

- Task packets name a stage as `scripts/factory/gates <docs|arch|ci|rust|full>` plus a narrow test
  filter when applicable. Do not copy the commands inside a stage into a packet or handoff.
- When local automated execution is permitted by the operator and machine policy, use that gate
  entry point. If local execution is prohibited, preserve the required stage in the packet and use
  the corresponding CI checks as evidence; report skipped, pending, failed, and passed evidence
  truthfully.
- Any direct Cargo invocation must run through `scripts/factory/with-cargo-slot`. The wrapper gives
  each worktree its own target namespace and serializes repository Cargo operations.
- Invoke `scripts/test_brains.sh` directly for Brain, daemon, server, TUI, and live tests. It enters
  the Cargo slot itself and owns process isolation and cleanup.

## Worktree and generated-output cleanup

The Software Factory [work-claim cleanup protocol](work-claims.md#post-integration-synchronization-and-cleanup)
owns worktree, branch, recovery-ref, process, and resource disposition. Finch's
`scripts/factory/reclaim-cargo-targets` has a narrower job: reclaiming generated Cargo target
namespaces after Git no longer registers their worktree.

Run the reclaimer without `--apply` to inventory candidates first. Use `--apply` only with cleanup
authority and only after its ownership, worktree, dirty-state, live-process, and Cargo-slot checks
identify the namespace as eligible. It never substitutes for removing a worktree, preserving a
commit, ending a claim, or recording cleanup evidence.

Run this cleanup promptly after each terminal worktree rather than batching stale targets until the
end of a queue. On a disk-constrained machine, retained worktrees and target namespaces reduce the
safe worker-pool ceiling. Retention is exceptional: record its owner and removal trigger. The
reclaimer must fail closed for a registered worktree, a live Cargo slot, an ownership mismatch, or
an otherwise ambiguous namespace; never compensate with a broad `rm` or `cargo clean` against a
shared target root.
