# brain/run: run lifecycle and leases

Owns one Brain run's lifecycle: status and kind, runner leases and handoffs, the legal-transition
table between states, cancellation reservations, and disconnect-terminalization intent. Exact-once
terminal state — a run finishes exactly once, even across cancel/disconnect/restart races — is
this facade's central invariant, which is why lease and transition logic live here instead of
scattered across callers.

Ownership, dependencies, and test commands are in [`AGENTS.md`](AGENTS.md).
