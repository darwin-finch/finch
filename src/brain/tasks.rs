//! Typed task-list state owned by a durable Brain.

use serde::{Deserialize, Serialize};

/// Priority of one Brain-owned task.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum BrainTaskPriority {
    High,
    #[default]
    Medium,
    Low,
}

/// Lifecycle status of one Brain-owned task.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum BrainTaskStatus {
    #[default]
    Pending,
    InProgress,
    Completed,
}

/// One typed task in the Brain's authoritative task-list projection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BrainTask {
    /// Stable identifier across replacements.
    pub id: String,
    /// Human-readable description.
    pub content: String,
    pub status: BrainTaskStatus,
    #[serde(default)]
    pub priority: BrainTaskPriority,
}
