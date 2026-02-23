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
    Completed { response: String },

    /// Query failed with an error
    Failed { error: String },

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

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[tokio::test]
    async fn test_create_query_returns_unique_ids() {
        let manager = QueryStateManager::new();
        let id1 = manager.create_query(vec![]).await;
        let id2 = manager.create_query(vec![]).await;
        assert_ne!(id1, id2, "each query should get a unique UUID");
    }

    #[tokio::test]
    async fn test_new_query_starts_in_processing_state() {
        let manager = QueryStateManager::new();
        let id = manager.create_query(vec![]).await;
        let state = manager.get_state(id).await.expect("state should exist");
        assert!(matches!(state, QueryState::Processing));
    }

    #[tokio::test]
    async fn test_get_state_unknown_id_returns_none() {
        let manager = QueryStateManager::new();
        let unknown = Uuid::new_v4();
        assert!(manager.get_state(unknown).await.is_none());
    }

    #[tokio::test]
    async fn test_update_state_to_completed() {
        let manager = QueryStateManager::new();
        let id = manager.create_query(vec![]).await;
        manager
            .update_state(
                id,
                QueryState::Completed {
                    response: "all done".to_string(),
                },
            )
            .await;
        match manager.get_state(id).await.unwrap() {
            QueryState::Completed { response } => assert_eq!(response, "all done"),
            _ => panic!("Expected Completed"),
        }
    }

    #[tokio::test]
    async fn test_update_state_to_failed() {
        let manager = QueryStateManager::new();
        let id = manager.create_query(vec![]).await;
        manager
            .update_state(
                id,
                QueryState::Failed {
                    error: "timeout".to_string(),
                },
            )
            .await;
        match manager.get_state(id).await.unwrap() {
            QueryState::Failed { error } => assert_eq!(error, "timeout"),
            _ => panic!("Expected Failed"),
        }
    }

    #[tokio::test]
    async fn test_update_state_to_executing_tools() {
        let manager = QueryStateManager::new();
        let id = manager.create_query(vec![]).await;
        manager
            .update_state(
                id,
                QueryState::ExecutingTools {
                    tools_pending: 3,
                    tools_completed: 1,
                },
            )
            .await;
        match manager.get_state(id).await.unwrap() {
            QueryState::ExecutingTools {
                tools_pending,
                tools_completed,
            } => {
                assert_eq!(tools_pending, 3);
                assert_eq!(tools_completed, 1);
            }
            _ => panic!("Expected ExecutingTools"),
        }
    }

    #[tokio::test]
    async fn test_cancel_query_sets_cancelled_state() {
        let manager = QueryStateManager::new();
        let id = manager.create_query(vec![]).await;
        manager.cancel_query(id).await;
        assert!(matches!(
            manager.get_state(id).await.unwrap(),
            QueryState::Cancelled
        ));
    }

    #[tokio::test]
    async fn test_cancel_query_triggers_cancellation_token() {
        let manager = QueryStateManager::new();
        let id = manager.create_query(vec![]).await;

        // Get the token before cancelling
        let token = {
            let metadata = manager.get_metadata(id).await.unwrap();
            metadata.cancellation_token.clone()
        };

        assert!(!token.is_cancelled(), "token should not be cancelled yet");
        manager.cancel_query(id).await;
        assert!(
            token.is_cancelled(),
            "token should be cancelled after cancel_query()"
        );
    }

    #[tokio::test]
    async fn test_remove_query_cleans_up_state() {
        let manager = QueryStateManager::new();
        let id = manager.create_query(vec![]).await;
        assert!(manager.get_state(id).await.is_some());
        manager.remove_query(id).await;
        assert!(
            manager.get_state(id).await.is_none(),
            "state should be gone after removal"
        );
    }

    #[tokio::test]
    async fn test_count_by_state_processing() {
        let manager = QueryStateManager::new();
        manager.create_query(vec![]).await;
        manager.create_query(vec![]).await;
        let id3 = manager.create_query(vec![]).await;
        manager
            .update_state(
                id3,
                QueryState::Completed {
                    response: "done".to_string(),
                },
            )
            .await;

        let processing = manager
            .count_by_state(|s| matches!(s, QueryState::Processing))
            .await;
        assert_eq!(processing, 2);

        let completed = manager
            .count_by_state(|s| matches!(s, QueryState::Completed { .. }))
            .await;
        assert_eq!(completed, 1);
    }

    #[tokio::test]
    async fn test_cleanup_removes_old_completed_queries() {
        let manager = QueryStateManager::new();
        let id = manager.create_query(vec![]).await;
        manager
            .update_state(
                id,
                QueryState::Completed {
                    response: "done".to_string(),
                },
            )
            .await;

        // Zero-duration threshold: everything completed is "old"
        manager.cleanup_old_queries(Duration::from_secs(0)).await;
        assert!(
            manager.get_state(id).await.is_none(),
            "old completed query should be cleaned up"
        );
    }

    #[tokio::test]
    async fn test_cleanup_keeps_in_progress_queries() {
        let manager = QueryStateManager::new();
        let id = manager.create_query(vec![]).await;
        // Still in Processing state — cleanup should NOT remove it

        manager.cleanup_old_queries(Duration::from_secs(0)).await;
        assert!(
            manager.get_state(id).await.is_some(),
            "in-progress query should survive cleanup"
        );
    }

    #[tokio::test]
    async fn test_cleanup_removes_old_failed_and_cancelled() {
        let manager = QueryStateManager::new();
        let id_fail = manager.create_query(vec![]).await;
        let id_cancel = manager.create_query(vec![]).await;

        manager
            .update_state(
                id_fail,
                QueryState::Failed {
                    error: "err".to_string(),
                },
            )
            .await;
        manager.update_state(id_cancel, QueryState::Cancelled).await;

        manager.cleanup_old_queries(Duration::from_secs(0)).await;

        assert!(
            manager.get_state(id_fail).await.is_none(),
            "old failed should be cleaned"
        );
        assert!(
            manager.get_state(id_cancel).await.is_none(),
            "old cancelled should be cleaned"
        );
    }

    #[tokio::test]
    async fn test_get_metadata_returns_full_metadata() {
        let manager = QueryStateManager::new();
        let id = manager.create_query(vec![]).await;
        let metadata = manager.get_metadata(id).await.unwrap();
        assert_eq!(metadata.id, id);
        assert!(matches!(metadata.state, QueryState::Processing));
    }

    #[tokio::test]
    async fn test_default_creates_empty_manager() {
        let manager = QueryStateManager::default();
        let count = manager.count_by_state(|_| true).await;
        assert_eq!(count, 0);
    }
}
