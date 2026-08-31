//! Per-query state and metadata for concurrent query execution.
//!
//! `QueryStateManager` tracks every in-flight query (identified by `Uuid`)
//! through its lifecycle: pending → streaming → awaiting tool results → done.
//! Each query has associated `WorkUnit` rows that drive the live TUI display.

use crate::claude::Message;
use crate::cli::messages::{ProgramOutputMessage, ProgramSourceMessage, WorkUnit};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

/// Correlation metadata for a provider query dispatched by one named-Brain
/// run. This is never authentication or authority.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BrainTurnProvenance {
    pub brain_id: crate::brain::store::BrainId,
    pub run_id: crate::brain::store::RunId,
    pub request_seq: u64,
}

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

    /// Present only when a named-Brain run caused this provider query.
    pub brain_turn_provenance: Option<BrainTurnProvenance>,

    /// Opaque daemon-owned authority for physical effects performed by tools
    /// in this named-Brain turn. This is never reconstructed from provenance.
    pub effect_audit: Option<crate::server::RunnerEffectAuditControl>,

    /// Cancellation token for this query
    pub cancellation_token: CancellationToken,

    /// Completed provider identity/accounting retained until a named-Brain
    /// turn crosses its durable daemon commit boundary.
    pub invocation_metadata: Option<crate::providers::types::InvocationMetadata>,

    /// When this query was created
    pub created_at: std::time::Instant,

    /// One reactive tool activity block for the entire provider/tool loop.
    /// Keeping it live across continuation requests prevents each round trip
    /// from becoming a separate anonymous transcript block.
    pub tool_work_unit: Option<Arc<WorkUnit>>,
    /// Stable live VM output for a named-Brain turn. The correlated canonical
    /// Result adopts and finalizes this same semantic message in place.
    pub brain_output_message: Option<Arc<ProgramOutputMessage>>,
    /// Stable local source projection awaiting its matching canonical Program.
    pub brain_source_message: Option<Arc<ProgramSourceMessage>>,
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
            brain_turn_provenance: None,
            effect_audit: None,
            cancellation_token: CancellationToken::new(),
            invocation_metadata: None,
            created_at: std::time::Instant::now(),
            tool_work_unit: None,
            brain_output_message: None,
            brain_source_message: None,
        };

        self.states.write().await.insert(id, metadata);
        id
    }

    /// Bind the query to its durable Brain/run before provider dispatch.
    pub async fn bind_brain_turn_provenance(
        &self,
        query_id: Uuid,
        provenance: BrainTurnProvenance,
    ) {
        if let Some(metadata) = self.states.write().await.get_mut(&query_id) {
            metadata.brain_turn_provenance = Some(provenance);
        }
    }

    /// Bind the daemon-issued effect capability before provider/tool dispatch.
    pub async fn bind_effect_audit(
        &self,
        query_id: Uuid,
        effect_audit: crate::server::RunnerEffectAuditControl,
    ) {
        if let Some(metadata) = self.states.write().await.get_mut(&query_id) {
            metadata.effect_audit = Some(effect_audit);
        }
    }

    /// Update the state of a query
    pub async fn update_state(&self, query_id: Uuid, state: QueryState) {
        if let Some(metadata) = self.states.write().await.get_mut(&query_id) {
            metadata.state = state;
        }
    }

    /// Enter tool execution unless cancellation already won the race with a
    /// provider completion.
    pub async fn begin_tool_execution(&self, query_id: Uuid, tools_pending: usize) -> bool {
        let mut states = self.states.write().await;
        let Some(metadata) = states.get_mut(&query_id) else {
            return false;
        };
        if matches!(metadata.state, QueryState::Cancelled) {
            return false;
        }
        metadata.state = QueryState::ExecutingTools {
            tools_pending,
            tools_completed: 0,
        };
        true
    }

    /// Publish a text-only provider completion while holding the same state
    /// lock used by cancellation. This makes the history append and terminal
    /// state one linearized operation: cancellation either wins first and no
    /// message is published, or observes an already-completed query.
    pub async fn try_publish_completion(
        &self,
        query_id: Uuid,
        response: String,
        source_for_history: String,
        conversation: &Arc<RwLock<crate::cli::conversation::ConversationHistory>>,
    ) -> bool {
        self.try_publish_completion_content(
            query_id,
            response,
            vec![crate::claude::ContentBlock::Text {
                text: source_for_history,
            }],
            conversation,
        )
        .await
    }

    /// Atomically publish a provider completion with its ordered opaque
    /// continuation blocks intact.
    pub async fn try_publish_completion_content(
        &self,
        query_id: Uuid,
        response: String,
        content: Vec<crate::claude::ContentBlock>,
        conversation: &Arc<RwLock<crate::cli::conversation::ConversationHistory>>,
    ) -> bool {
        let mut states = self.states.write().await;
        let Some(metadata) = states.get_mut(&query_id) else {
            return false;
        };
        if metadata.cancellation_token.is_cancelled()
            || matches!(
                metadata.state,
                QueryState::Cancelled | QueryState::Failed { .. } | QueryState::Completed { .. }
            )
        {
            return false;
        }
        conversation
            .write()
            .await
            .add_message(crate::claude::Message {
                role: "assistant".to_string(),
                content,
            });
        metadata.state = QueryState::Completed { response };
        true
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

    pub async fn set_invocation_metadata(
        &self,
        query_id: Uuid,
        invocation: crate::providers::types::InvocationMetadata,
    ) {
        if let Some(metadata) = self.states.write().await.get_mut(&query_id) {
            metadata.invocation_metadata = Some(invocation);
        }
    }

    pub async fn set_tool_work_unit(&self, query_id: Uuid, unit: Option<Arc<WorkUnit>>) {
        if let Some(metadata) = self.states.write().await.get_mut(&query_id) {
            metadata.tool_work_unit = unit;
        }
    }

    pub async fn tool_work_unit(&self, query_id: Uuid) -> Option<Arc<WorkUnit>> {
        self.states
            .read()
            .await
            .get(&query_id)
            .and_then(|metadata| metadata.tool_work_unit.clone())
    }

    pub async fn set_brain_output_message(
        &self,
        query_id: Uuid,
        unit: Option<Arc<ProgramOutputMessage>>,
    ) {
        if let Some(metadata) = self.states.write().await.get_mut(&query_id) {
            metadata.brain_output_message = unit;
        }
    }

    pub async fn brain_output_message(&self, query_id: Uuid) -> Option<Arc<ProgramOutputMessage>> {
        self.states
            .read()
            .await
            .get(&query_id)
            .and_then(|metadata| metadata.brain_output_message.clone())
    }

    pub async fn set_brain_source_message(
        &self,
        query_id: Uuid,
        message: Option<Arc<ProgramSourceMessage>>,
    ) {
        if let Some(metadata) = self.states.write().await.get_mut(&query_id) {
            metadata.brain_source_message = message;
        }
    }

    pub async fn brain_source_message(&self, query_id: Uuid) -> Option<Arc<ProgramSourceMessage>> {
        self.states
            .read()
            .await
            .get(&query_id)
            .and_then(|metadata| metadata.brain_source_message.clone())
    }

    /// Cancel a query
    pub async fn cancel_query(&self, query_id: Uuid) -> bool {
        let mut states = self.states.write().await;
        let Some(metadata) = states.get_mut(&query_id) else {
            return false;
        };
        if matches!(
            metadata.state,
            QueryState::Completed { .. } | QueryState::Failed { .. } | QueryState::Cancelled
        ) {
            return false;
        }
        metadata.cancellation_token.cancel();
        metadata.state = QueryState::Cancelled;
        true
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
    async fn query_retains_one_tool_work_unit_across_continuations() {
        let manager = QueryStateManager::new();
        let id = manager.create_query(vec![]).await;
        let unit = Arc::new(WorkUnit::new("Tools"));

        manager
            .set_tool_work_unit(id, Some(Arc::clone(&unit)))
            .await;
        let retained = manager.tool_work_unit(id).await.expect("tool unit");
        assert!(Arc::ptr_eq(&unit, &retained));

        manager.set_tool_work_unit(id, None).await;
        assert!(manager.tool_work_unit(id).await.is_none());
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
        assert!(manager.cancel_query(id).await);
        assert!(matches!(
            manager.get_state(id).await.unwrap(),
            QueryState::Cancelled
        ));
    }

    #[tokio::test]
    async fn cancelled_query_cannot_reenter_tool_execution() {
        let manager = QueryStateManager::new();
        let id = manager.create_query(vec![]).await;
        manager.cancel_query(id).await;

        assert!(!manager.begin_tool_execution(id, 2).await);
        assert!(matches!(
            manager.get_state(id).await,
            Some(QueryState::Cancelled)
        ));
    }

    #[tokio::test]
    async fn cancelled_query_cannot_publish_late_provider_history() {
        let manager = QueryStateManager::new();
        let id = manager.create_query(vec![]).await;
        let conversation = Arc::new(RwLock::new(
            crate::cli::conversation::ConversationHistory::new(),
        ));
        manager.cancel_query(id).await;

        assert!(
            !manager
                .try_publish_completion(
                    id,
                    "rendered late".to_string(),
                    "provider late".to_string(),
                    &conversation,
                )
                .await
        );
        assert!(conversation.read().await.get_messages().is_empty());
        assert!(matches!(
            manager.get_state(id).await,
            Some(QueryState::Cancelled)
        ));
    }

    #[tokio::test]
    async fn published_completion_cannot_be_reclassified_as_cancelled() {
        let manager = QueryStateManager::new();
        let id = manager.create_query(vec![]).await;
        let conversation = Arc::new(RwLock::new(
            crate::cli::conversation::ConversationHistory::new(),
        ));

        assert!(
            manager
                .try_publish_completion(
                    id,
                    "rendered".to_string(),
                    "provider source".to_string(),
                    &conversation,
                )
                .await
        );
        assert!(!manager.cancel_query(id).await);

        assert_eq!(conversation.read().await.get_messages().len(), 1);
        assert!(matches!(
            manager.get_state(id).await,
            Some(QueryState::Completed { response }) if response == "rendered"
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
    async fn test_named_brain_effect_audit_binding_survives_query_metadata_round_trip() {
        let manager = QueryStateManager::new();
        let id = manager.create_query(vec![]).await;
        let (audit_tx, _audit_rx) = tokio::sync::mpsc::unbounded_channel();

        manager
            .bind_effect_audit(id, crate::server::RunnerEffectAuditControl::new(audit_tx))
            .await;

        assert!(manager
            .get_metadata(id)
            .await
            .and_then(|metadata| metadata.effect_audit)
            .is_some());
    }

    #[tokio::test]
    async fn test_default_creates_empty_manager() {
        let manager = QueryStateManager::default();
        let count = manager.count_by_state(|_| true).await;
        assert_eq!(count, 0);
    }
}
