// Metrics module
// Public interface for logging and tracking metrics

mod logger;
mod types;

pub use logger::MetricsLogger;
pub use types::RequestMetric;
