# brain/projection: read-only snapshots

Owns read-only views over Brain state: `BrainSnapshot`, client wire messages, unhydrated list
summaries, and observer-safe effect-audit projection. It exists as a facade separate from the
journal and store so that "give me the current state" call sites can never accidentally mutate,
hydrate, or create anything — this facade only ever reads.

Ownership, dependencies, and test commands are in [`AGENTS.md`](AGENTS.md).
