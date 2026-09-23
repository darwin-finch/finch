# Source index

This capsule gives tools a small, attributable map of a source file without sending the file body
to a model. It owns the common envelope—exact source identity, spans, provenance, truncation, and
generation-bound span reads—that code retrieval and long-document retrieval can share. Provenance
separates the broad evidence class (`structural/parser`, `semantic/embedding`, and peers) from the
concrete method, so code and corpus tools do not invent incompatible labels.

A code-outline caller supplies one workspace file and receives named definitions with line and byte
spans. A later corpus caller can use the same envelope for headings or passages, so OCR-derived and
plain-text material do not invent a second provenance format. Ranking, model-assisted hops,
persistent indexing, and corpus discovery are intentionally outside this first boundary.

See [`AGENTS.md`](AGENTS.md) for dependency rules, invariants, and focused tests; [`mod.rs`](mod.rs)
is the exact callable surface.
