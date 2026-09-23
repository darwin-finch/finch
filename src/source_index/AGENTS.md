# source_index capsule: bounded source identity, spans, and outlines

Supplements the root [`AGENTS.md`](../../AGENTS.md), which applies in full.

**Owns** workspace-namespaced exact-byte source identity, workspace-relative paths,
revision/content generations, bounded source spans, retrieval provenance, deterministic structural
outlines, Git-defined repository manifests, direct-child directory menus, and an atomic versioned
cache. It does not own tool registration, hop selection, provider prompting, embeddings, model
calls, MemTree, application state-path selection, or document/media discovery.

**Interface:** [`mod.rs`](mod.rs) is the facade. `identity` and `outline` stay private; callers use
only the facade exports. Parser-library values never cross this boundary.

**Dependencies:** this capsule may use Git as a path-set authority plus deterministic parsing,
Markdown, hashing, capability-filesystem, locking, and serialization libraries. It must not
depend on `cli`, `tools`, providers, models, memory, runtime, server, or application configuration.
The application layer injects one canonical workspace root and one existing owner-only state
directory. Source and cache leaves are opened relative to capability directories; repository source
opens do not follow leaf symlinks, and `..` cannot redirect a read.

**Invariants:** source files, paths, files, directories, child menus, record counts, AGENTS leads,
and cache images are bounded and byte-stable; identity
includes a hash of the exact working bytes rather than trusting Git HEAD and an opaque
canonical-workspace namespace prevents cross-workspace reuse; every span is tied to that identity;
stale validation and span consumption fail closed after mutation. Repository membership follows
tracked Git index entries plus untracked `--exclude-standard` entries, without user/global ignore
files; symlinks and gitlinks are excluded. Cache publication serializes builders and atomically
replaces one complete image, preserving the previous generation on failure. Byte ranges are
zero-based and half-open over UTF-8 source bytes; line ranges are one-based and inclusive.
Structural outlines never copy comments, string contents, documentation, or function bodies. The
first prose paragraph of `AGENTS.md`, capped at 512 UTF-8 bytes and carrying identity plus span, is
the only body text persisted for routing. Tree-sitter parse errors are reported in the envelope.

The state capability may create its own final directory leaf with Unix mode 0700. It admits at
most eight workspace cache images and 512 MiB of regular cache/lock/temporary leaves, rejects
matching symlink or non-regular leaves, and never deletes another workspace's image automatically.

**Extension rules:** add a grammar only with deterministic fixtures proving definitions and spans.
Change storage schema, serialized-outline, and outline algorithm versions independently. Keep cache records body-free;
new body exceptions require explicit review and a hard bound. Sibling hops, semantic ranking, and
inferred relationships belong in later reviewed stages; do not hide model calls behind this facade.

**Focused tests:** `./scripts/test_brains.sh cargo test --lib -- source_index::`.
