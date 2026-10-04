//! Per-query state and metadata for concurrent query execution.
//!
//! `QueryStateManager` tracks every in-flight query (identified by `Uuid`)
//! through its lifecycle: pending → streaming → awaiting tool results → done.
//! Each query has associated `WorkUnit` rows that drive the live TUI display.

use crate::cli::messages::WorkUnit;
use crate::providers::Message;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

/// Correlation metadata for a provider query dispatched by one named-Brain
/// run. This is never authentication or authority.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BrainTurnProvenance {
    pub brain_id: crate::brain::BrainId,
    pub run_id: crate::brain::RunId,
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

/// Which provider entry and model a query runs on, for its request metric.
/// Identities only; never prompt or response text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RequestRoute {
    /// Provider entry (profile) name.
    pub provider: String,
    /// Model the entry was asked for.
    pub model: String,
    /// Local or cloud by the entry's kind; `None` when no configured entry
    /// matches the generator.
    pub kind: Option<crate::metrics::ProviderKind>,
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

    /// Application-owned maximum VM authority for provider wire produced by
    /// this query. Local owner queries leave it unset; named-Brain turns bind
    /// the daemon-issued ceiling before dispatch.
    pub grant_ceiling: Option<crate::vm::EffectSet>,

    /// Cancellation token for this query
    pub cancellation_token: CancellationToken,

    /// Completed provider identity/accounting retained until a named-Brain
    /// turn crosses its durable daemon commit boundary.
    pub invocation_metadata: Option<crate::providers::InvocationMetadata>,

    /// When this query was created
    pub created_at: std::time::Instant,

    /// Bound by the LLM worker when it picks the generator for this query.
    pub request_route: Option<RequestRoute>,

    /// Set by the first terminal transition, so a query yields exactly one
    /// request metric however many paths later try to close it.
    pub request_metric_taken: bool,

    /// One reactive tool activity block for the entire provider/tool loop.
    /// Keeping it live across continuation requests prevents each round trip
    /// from becoming a separate anonymous transcript block.
    pub tool_work_unit: Option<Arc<WorkUnit>>,
    /// Transient live VM output for a named-Brain turn. Once the daemon
    /// publishes the correlated Result, EventLoop folds this into the run
    /// group and removes the transient unit without losing live updates.
    pub brain_output_work_unit: Option<Arc<WorkUnit>>,
}

/// Manages state for all in-flight queries
pub struct QueryStateManager {
    states: Arc<RwLock<HashMap<Uuid, QueryMetadata>>>,
    /// Where a query's request metric is appended when it first reaches a
    /// terminal state. Unset means nothing is recorded.
    request_metrics: std::sync::RwLock<Option<Arc<crate::metrics::MetricsLogger>>>,
}

/// Provider and model shown for a query that ended before the LLM worker
/// bound a generator to it.
const UNBOUND_REQUEST_IDENTITY: &str = "not bound";

/// Build the one request metric a query yields, on its first terminal
/// transition. Returns `None` for a non-terminal state and for every later
/// terminal transition of the same query.
fn take_request_metric(
    metadata: &mut QueryMetadata,
    state: &QueryState,
) -> Option<crate::metrics::RequestMetric> {
    let outcome = match state {
        QueryState::Completed { .. } => crate::metrics::RequestOutcome::Completed,
        QueryState::Failed { .. } => crate::metrics::RequestOutcome::Failed,
        QueryState::Cancelled => crate::metrics::RequestOutcome::Cancelled,
        QueryState::Processing | QueryState::ExecutingTools { .. } => return None,
    };
    if metadata.request_metric_taken {
        return None;
    }
    metadata.request_metric_taken = true;
    let elapsed_ms = u64::try_from(metadata.created_at.elapsed().as_millis()).unwrap_or(u64::MAX);
    let (provider, model, kind) = match &metadata.request_route {
        Some(route) => (route.provider.as_str(), route.model.as_str(), route.kind),
        None => (UNBOUND_REQUEST_IDENTITY, UNBOUND_REQUEST_IDENTITY, None),
    };
    // The wire-adherence vocabulary: a turn the daemon asked this runner to
    // perform for a named Brain, or one the session's own user typed.
    let surface = if metadata.brain_turn_provenance.is_some() {
        "named_brain"
    } else {
        "interactive"
    };
    Some(crate::metrics::RequestMetric::turn(
        provider, model, kind, outcome, surface, elapsed_ms,
    ))
}

impl QueryStateManager {
    /// Create a new query state manager
    pub fn new() -> Self {
        Self {
            states: Arc::new(RwLock::new(HashMap::new())),
            request_metrics: std::sync::RwLock::new(None),
        }
    }

    /// Record one source-free request metric per query into `logger` from
    /// now on. The event loop calls this with its injected logger; a manager
    /// nobody calls it on records nothing.
    pub fn record_request_metrics_to(&self, logger: Option<Arc<crate::metrics::MetricsLogger>>) {
        *self
            .request_metrics
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = logger;
    }

    /// Bind the provider entry and model this query runs on. The LLM worker
    /// calls it each time it picks a generator for the query, so the metric
    /// names the generator that last served the turn.
    pub async fn bind_request_route(&self, query_id: Uuid, route: RequestRoute) {
        if let Some(metadata) = self.states.write().await.get_mut(&query_id) {
            metadata.request_route = Some(route);
        }
    }

    /// Append a taken request metric. Called after the state lock is
    /// released; a write failure is logged and never fails the query.
    fn log_request_metric(&self, metric: Option<crate::metrics::RequestMetric>) {
        let Some(metric) = metric else {
            return;
        };
        let logger = self
            .request_metrics
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        let Some(logger) = logger else {
            return;
        };
        if let Err(error) = logger.log(&metric) {
            tracing::warn!("failed to record request metric: {error}");
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
            grant_ceiling: None,
            cancellation_token: CancellationToken::new(),
            invocation_metadata: None,
            created_at: std::time::Instant::now(),
            request_route: None,
            request_metric_taken: false,
            tool_work_unit: None,
            brain_output_work_unit: None,
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

    /// Bind an application-authored VM authority ceiling before provider
    /// dispatch. Provider output has no path to mutate this metadata.
    pub async fn bind_grant_ceiling(&self, query_id: Uuid, grant_ceiling: crate::vm::EffectSet) {
        if let Some(metadata) = self.states.write().await.get_mut(&query_id) {
            metadata.grant_ceiling = Some(grant_ceiling);
        }
    }

    /// Update the state of a query
    pub async fn update_state(&self, query_id: Uuid, state: QueryState) {
        let metric = {
            let mut states = self.states.write().await;
            let Some(metadata) = states.get_mut(&query_id) else {
                return;
            };
            let metric = take_request_metric(metadata, &state);
            metadata.state = state;
            metric
        };
        self.log_request_metric(metric);
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
            vec![crate::providers::ContentBlock::Text {
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
        content: Vec<crate::providers::ContentBlock>,
        conversation: &Arc<RwLock<crate::cli::conversation::ConversationHistory>>,
    ) -> bool {
        let metric = {
            let mut states = self.states.write().await;
            let Some(metadata) = states.get_mut(&query_id) else {
                return false;
            };
            if metadata.cancellation_token.is_cancelled()
                || matches!(
                    metadata.state,
                    QueryState::Cancelled
                        | QueryState::Failed { .. }
                        | QueryState::Completed { .. }
                )
            {
                return false;
            }
            conversation
                .write()
                .await
                .add_message(crate::providers::Message {
                    role: "assistant".to_string(),
                    content,
                });
            let state = QueryState::Completed { response };
            let metric = take_request_metric(metadata, &state);
            metadata.state = state;
            metric
        };
        self.log_request_metric(metric);
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

    /// Whether provider output may still project into live UI, accounting,
    /// conversation, or execution state for this query.
    pub async fn accepts_provider_projection(&self, query_id: Uuid) -> bool {
        self.states
            .read()
            .await
            .get(&query_id)
            .is_some_and(|metadata| {
                !metadata.cancellation_token.is_cancelled()
                    && !matches!(
                        metadata.state,
                        QueryState::Cancelled
                            | QueryState::Failed { .. }
                            | QueryState::Completed { .. }
                    )
            })
    }

    /// Get full metadata for a query
    pub async fn get_metadata(&self, query_id: Uuid) -> Option<QueryMetadata> {
        self.states.read().await.get(&query_id).cloned()
    }

    pub async fn set_invocation_metadata(
        &self,
        query_id: Uuid,
        invocation: crate::providers::InvocationMetadata,
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

    /// Every in-flight query's Tools unit, used to attach orphan tool/child
    /// results instead of starting a new root.
    pub(crate) async fn live_tool_work_units(&self) -> Vec<Arc<WorkUnit>> {
        self.states
            .read()
            .await
            .values()
            .filter_map(|metadata| metadata.tool_work_unit.clone())
            .collect()
    }

    pub async fn set_brain_output_work_unit(&self, query_id: Uuid, unit: Option<Arc<WorkUnit>>) {
        if let Some(metadata) = self.states.write().await.get_mut(&query_id) {
            metadata.brain_output_work_unit = unit;
        }
    }

    pub async fn brain_output_work_unit(&self, query_id: Uuid) -> Option<Arc<WorkUnit>> {
        self.states
            .read()
            .await
            .get(&query_id)
            .and_then(|metadata| metadata.brain_output_work_unit.clone())
    }

    /// Cancel a query
    pub async fn cancel_query(&self, query_id: Uuid) -> bool {
        let metric = {
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
            let metric = take_request_metric(metadata, &QueryState::Cancelled);
            metadata.state = QueryState::Cancelled;
            metric
        };
        self.log_request_metric(metric);
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
    async fn test_named_brain_grant_ceiling_binding_survives_query_metadata_round_trip() {
        let manager = QueryStateManager::new();
        let id = manager.create_query(vec![]).await;
        let ceiling = crate::vm::TypedRuntime::intrinsic_grants();

        manager.bind_grant_ceiling(id, ceiling.clone()).await;

        let metadata = manager
            .get_metadata(id)
            .await
            .expect("named-Brain query metadata must remain resident until turn completion");
        assert_eq!(
            metadata.grant_ceiling,
            Some(ceiling),
            "named-Brain query metadata lost the daemon-issued VM grant ceiling before provider wire execution"
        );
    }

    #[tokio::test]
    async fn test_default_creates_empty_manager() {
        let manager = QueryStateManager::default();
        let count = manager.count_by_state(|_| true).await;
        assert_eq!(count, 0);
    }
}
