// Query state management for concurrent query execution

use crate::claude::Message;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

/// State of an in-flight query
#[derive(Debug, Clone)]
pub enum QueryState {
    /// Query is being processed (initial API call)
    Processing,

    /// Waiting for tool execution to complete
    ExecutingTools {
        tools_pending: usize,
        tools_completed: usize,
    },

    /// Query completed successfully
    Completed {
        response: String,
    },

    /// Query failed with an error
    Failed {
        error: String,
    },

    /// Query was cancelled by user
    Cancelled,
}

/// Metadata for a query
#[derive(Debug, Clone)]
pub struct QueryMetadata {
    /// Query ID
    pub id: Uuid,

    /// Current state
    pub state: QueryState,

    /// Snapshot of conversation at query start time
    pub conversation_snapshot: Vec<Message>,

    /// Cancellation token for this query
    pub cancellation_token: CancellationToken,

    /// When this query was created
    pub created_at: std::time::Instant,
}

/// Manages state for all in-flight queries
pub struct QueryStateManager {
    states: Arc<RwLock<HashMap<Uuid, QueryMetadata>>>,
}

impl QueryStateManager {
    /// Create a new query state manager
    pub fn new() -> Self {
        Self {
            states: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Create a new query with initial state
    pub async fn create_query(&self, conversation_snapshot: Vec<Message>) -> Uuid {
        let id = Uuid::new_v4();
        let metadata = QueryMetadata {
            id,
            state: QueryState::Processing,
            conversation_snapshot,
            cancellation_token: CancellationToken::new(),
            created_at: std::time::Instant::now(),
        };

        self.states.write().await.insert(id, metadata);
        id
    }

    /// Update the state of a query
    pub async fn update_state(&self, query_id: Uuid, state: QueryState) {
        if let Some(metadata) = self.states.write().await.get_mut(&query_id) {
            metadata.state = state;
        }
    }

    /// Get the current state of a query
    pub async fn get_state(&self, query_id: Uuid) -> Option<QueryState> {
        self.states
            .read()
            .await
            .get(&query_id)
            .map(|m| m.state.clone())
    }

    /// Get full metadata for a query
    pub async fn get_metadata(&self, query_id: Uuid) -> Option<QueryMetadata> {
        self.states.read().await.get(&query_id).cloned()
    }

    /// Cancel a query
    pub async fn cancel_query(&self, query_id: Uuid) {
        if let Some(metadata) = self.states.read().await.get(&query_id) {
            metadata.cancellation_token.cancel();
        }
        self.update_state(query_id, QueryState::Cancelled).await;
    }

    /// Remove a completed/failed/cancelled query (cleanup)
    pub async fn remove_query(&self, query_id: Uuid) {
        self.states.write().await.remove(&query_id);
    }

    /// Clean up old completed queries (older than threshold)
    pub async fn cleanup_old_queries(&self, max_age: std::time::Duration) {
        let now = std::time::Instant::now();
        let mut states = self.states.write().await;

        states.retain(|_, metadata| {
            let age = now.duration_since(metadata.created_at);

            // Keep if not completed/failed/cancelled, or if still recent
            match metadata.state {
                QueryState::Completed { .. }
                | QueryState::Failed { .. }
                | QueryState::Cancelled => age < max_age,
                _ => true, // Keep in-progress queries
            }
        });
    }

    /// Get count of queries in a specific state
    pub async fn count_by_state(&self, state_matcher: impl Fn(&QueryState) -> bool) -> usize {
        self.states
            .read()
            .await
            .values()
            .filter(|m| state_matcher(&m.state))
            .count()
    }
}

impl Default for QueryStateManager {
    fn default() -> Self {
        Self::new()
    }
}
