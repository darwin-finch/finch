# brain/journal: append-only events and replay

Owns the authoritative append-only log for a Brain: the durable event envelope, mutation
receipts, checksummed JSONL persistence, and torn/corrupt-tail recovery. Everything else about a
Brain — its schedule, its runs, its projections — is a replay of this log, which is why it's
isolated as its own facade: the log's framing and recovery behavior are the one thing that must
never regress silently.

Ownership, dependencies, and test commands are in [`AGENTS.md`](AGENTS.md).
