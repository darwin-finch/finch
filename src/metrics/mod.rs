// Metrics module
// Public interface for logging and tracking metrics

mod logger;
mod similarity;
mod trends;
mod types;

pub use crate::vm::WireFailureClass;
pub use logger::MetricsLogger;
pub use logger::{RequestGroupSummary, RequestSummary};
pub use similarity::semantic_similarity;
pub use trends::{TrainingTrends, Trend};
pub use types::{
    ProviderKind, RequestMetric, RequestOutcome, ResponseComparison, WireAdherenceMetric,
};
