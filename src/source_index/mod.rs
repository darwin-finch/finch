//! Revision-aware, bounded source outlines.
//!
//! This facade is the shared retrieval envelope for code and prose sources.
//! Parsing implementations stay private so callers depend on source identity,
//! spans, provenance, and staleness semantics rather than parser libraries.

mod identity;
mod outline;

pub use identity::{SourceIdentity, SourceResolver};
pub use outline::{
    OutlineRecord, OutlineResult, RetrievalMethod, RetrievalProvenance, RetrievalProvenanceClass,
    SourceExcerpt, SourceSpan,
};
