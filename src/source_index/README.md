# Source index

This capsule gives retrieval tools a small, attributable map of source without putting repository
bodies in the routing index. It owns exact source identity, spans, provenance, deterministic
structural outlines, direct-child directory menus, and the versioned repository cache. The cache is
a routing aid rather than a source of truth: callers validate its generation and use
generation-bound span reads before consuming source bytes.

A code-outline caller supplies one workspace file and receives named definitions with line and byte
spans, then reads only a selected span against the same identity. A repository-routing caller opens
an owner-only cache, builds a Git-defined snapshot, and walks direct-child menus and body-free file
outlines; the first prose paragraph of a directory's `AGENTS.md` is the sole bounded body exception
and helps choose the next capsule. A changed file, revision, ignore rule, or path set makes that
snapshot stale before source is returned.

Repository consistency is optimistic rather than an atomic filesystem snapshot. A routing caller
rebuilds whenever generation validation fails, then validates each selected leaf again by consuming
its recorded span through the resolver. A mutation after snapshot validation can introduce a new
candidate that the current route did not see, but it cannot substitute new bytes for the selected
generation.

This module does not choose hops, rank candidates, call models, discover non-repository corpora, or
own application state paths. Those decisions stay at the composition root. The next retrieval stage
may use the snapshot to implement sibling-aware code hops, while document and OCR ingestion can
reuse the identity/span/provenance envelope without sharing this Git-specific repository cache.

See [`AGENTS.md`](AGENTS.md) for dependency rules, invariants, and focused tests; [`mod.rs`](mod.rs)
is the exact callable surface.
