# source_index capsule: bounded source identity, spans, and outlines

Supplements the root [`AGENTS.md`](../../CLAUDE.md), which applies in full.

**Owns** workspace-namespaced exact-byte source identity, workspace-relative paths,
revision/content generations, bounded source spans, retrieval provenance, deterministic structural
outlines, and generation-bound span consumption. It does not own tool registration, provider prompting, embeddings, model calls,
MemTree, or document/media discovery.

**Interface:** [`mod.rs`](mod.rs) is the facade. `identity` and `outline` stay private; callers use
only the facade exports. Parser-library values never cross this boundary.

**Dependencies:** this capsule may use deterministic parser and hashing libraries. It must not
depend on `cli`, `tools`, providers, models, memory, runtime, server, or application configuration.
The application layer injects one canonical workspace root shared with permission policy. Source
files are opened relative to a capability directory, so containment and reading use one file object
and outward symlinks or `..` cannot redirect the read.

**Invariants:** source files, record counts, and record labels are bounded and byte-stable; identity
includes a hash of the exact working bytes rather than trusting Git HEAD and an opaque
canonical-workspace namespace prevents cross-workspace reuse; every span is tied to that identity;
stale validation and span consumption fail closed after mutation. Byte ranges are zero-based and
half-open over UTF-8 source bytes; line ranges are one-based and inclusive. Structural outlines
never copy comments, string contents, documentation, or function bodies. Tree-sitter parse errors
are reported in the envelope rather than hidden.

**Extension rules:** add a grammar only with deterministic fixtures proving definitions and spans.
Semantic ranking, persisted indexes, and inferred relationships require separate reviewed stages;
do not hide model calls behind this parser facade.

**Focused tests:** `./scripts/test_brains.sh cargo test --lib -- source_index::`.
