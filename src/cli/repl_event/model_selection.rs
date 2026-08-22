//! Session-scoped model selection and query pinning.

use std::collections::HashMap;
use std::future::Future;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::{Mutex, RwLock};
use uuid::Uuid;

use crate::client::LocalModelStatus;
use crate::generators::Generator;

/// Mutable model state shared by the TUI, command handler, and LLM worker.
#[derive(Clone)]
pub(crate) struct ModelSelection {
    generator: Arc<RwLock<Arc<dyn Generator>>>,
    active_index: Arc<RwLock<usize>>,
    pending_index: Arc<RwLock<Option<usize>>>,
    generation: Arc<AtomicU64>,
    transition: Arc<Mutex<()>>,
}

impl ModelSelection {
    pub(crate) fn new(active_index: usize, generator: Arc<dyn Generator>) -> Self {
        Self::from_handle(active_index, Arc::new(RwLock::new(generator)))
    }

    pub(crate) fn from_handle(
        active_index: usize,
        generator: Arc<RwLock<Arc<dyn Generator>>>,
    ) -> Self {
        Self {
            generator,
            active_index: Arc::new(RwLock::new(active_index)),
            pending_index: Arc::new(RwLock::new(None)),
            generation: Arc::new(AtomicU64::new(0)),
            transition: Arc::new(Mutex::new(())),
        }
    }

    pub(crate) fn generator_handle(&self) -> Arc<RwLock<Arc<dyn Generator>>> {
        Arc::clone(&self.generator)
    }

    pub(crate) async fn generator(&self) -> Arc<dyn Generator> {
        self.generator.read().await.clone()
    }

    pub(crate) async fn active_index(&self) -> usize {
        *self.active_index.read().await
    }

    pub(crate) async fn pending_index(&self) -> Option<usize> {
        *self.pending_index.read().await
    }

    /// Immediately make a ready generator the default for new queries.
    pub(crate) async fn activate(&self, index: usize, generator: Arc<dyn Generator>) {
        let _transition = self.transition.lock().await;
        self.generation.fetch_add(1, Ordering::SeqCst);
        *self.pending_index.write().await = None;
        *self.generator.write().await = generator;
        *self.active_index.write().await = index;
    }

    /// Mark a profile as pending and return the token allowed to activate it.
    pub(crate) async fn begin_pending(&self, index: usize) -> u64 {
        let _transition = self.transition.lock().await;
        let token = self.generation.fetch_add(1, Ordering::SeqCst) + 1;
        *self.pending_index.write().await = Some(index);
        token
    }

    /// Invalidate any startup task without changing the generator serving queries.
    pub(crate) async fn cancel_pending(&self) {
        let _transition = self.transition.lock().await;
        self.generation.fetch_add(1, Ordering::SeqCst);
        *self.pending_index.write().await = None;
    }

    pub(crate) fn is_current(&self, token: u64) -> bool {
        self.generation.load(Ordering::SeqCst) == token
    }

    /// Activate a pending generator only if no later `/model` command won.
    pub(crate) async fn complete_pending(
        &self,
        token: u64,
        index: usize,
        generator: Arc<dyn Generator>,
    ) -> bool {
        let _transition = self.transition.lock().await;
        if !self.is_current(token) {
            return false;
        }
        *self.generator.write().await = generator;
        *self.active_index.write().await = index;
        *self.pending_index.write().await = None;
        true
    }

    pub(crate) async fn fail_pending(&self, token: u64) -> bool {
        let _transition = self.transition.lock().await;
        if !self.is_current(token) {
            return false;
        }
        *self.pending_index.write().await = None;
        true
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum LocalActivationOutcome {
    Activated(String),
    Cancelled,
    Failed(String),
    NotAvailable,
    StatusError(String),
}

/// Poll daemon bootstrap state and atomically activate a local generator.
pub(crate) async fn activate_local_when_ready<F, Fut>(
    selection: ModelSelection,
    token: u64,
    target_index: usize,
    generator: Arc<dyn Generator>,
    mut read_status: F,
    poll_interval: Duration,
) -> LocalActivationOutcome
where
    F: FnMut() -> Fut,
    Fut: Future<Output = anyhow::Result<LocalModelStatus>>,
{
    loop {
        if !selection.is_current(token) {
            return LocalActivationOutcome::Cancelled;
        }

        match read_status().await {
            Ok(LocalModelStatus::Ready(model)) => {
                return if selection
                    .complete_pending(token, target_index, generator)
                    .await
                {
                    LocalActivationOutcome::Activated(model)
                } else {
                    LocalActivationOutcome::Cancelled
                };
            }
            Ok(LocalModelStatus::Failed(error)) => {
                return if selection.fail_pending(token).await {
                    LocalActivationOutcome::Failed(error)
                } else {
                    LocalActivationOutcome::Cancelled
                };
            }
            Ok(LocalModelStatus::NotAvailable) => {
                return if selection.fail_pending(token).await {
                    LocalActivationOutcome::NotAvailable
                } else {
                    LocalActivationOutcome::Cancelled
                };
            }
            Ok(LocalModelStatus::Initializing)
            | Ok(LocalModelStatus::Downloading(_))
            | Ok(LocalModelStatus::Loading(_)) => {
                if !poll_interval.is_zero() {
                    tokio::time::sleep(poll_interval).await;
                }
            }
            Err(error) => {
                return if selection.fail_pending(token).await {
                    LocalActivationOutcome::StatusError(error.to_string())
                } else {
                    LocalActivationOutcome::Cancelled
                };
            }
        }
    }
}

/// Keeps every tool continuation on the generator that began its query.
#[derive(Default)]
pub(crate) struct GeneratorPins {
    generators: RwLock<HashMap<Uuid, Arc<dyn Generator>>>,
}

impl GeneratorPins {
    pub(crate) async fn for_turn(
        &self,
        query_id: Uuid,
        new_query: bool,
        active: Arc<dyn Generator>,
    ) -> Arc<dyn Generator> {
        if !new_query {
            if let Some(generator) = self.generators.read().await.get(&query_id).cloned() {
                return generator;
            }
        }
        self.generators
            .write()
            .await
            .insert(query_id, Arc::clone(&active));
        active
    }

    pub(crate) async fn release(&self, query_id: Uuid) {
        self.generators.write().await.remove(&query_id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::Result;
    use async_trait::async_trait;
    use tokio::sync::mpsc;

    use crate::claude::Message;
    use crate::cli::conversation::ConversationHistory;
    use crate::generators::{
        GeneratorCapabilities, GeneratorResponse, ResponseMetadata, StreamChunk,
    };
    use crate::tools::types::ToolDefinition;

    struct MockGenerator {
        name: &'static str,
        capabilities: GeneratorCapabilities,
    }

    impl MockGenerator {
        fn named(name: &'static str) -> Arc<dyn Generator> {
            Arc::new(Self {
                name,
                capabilities: GeneratorCapabilities {
                    supports_streaming: false,
                    supports_tools: true,
                    supports_conversation: true,
                    max_context_messages: None,
                },
            })
        }
    }

    #[async_trait]
    impl Generator for MockGenerator {
        async fn generate(
            &self,
            _messages: Vec<Message>,
            _tools: Option<Vec<ToolDefinition>>,
        ) -> Result<GeneratorResponse> {
            Ok(GeneratorResponse {
                text: self.name.to_string(),
                content_blocks: vec![],
                tool_uses: vec![],
                metadata: ResponseMetadata {
                    generator: self.name.to_string(),
                    model: self.name.to_string(),
                    confidence: None,
                    stop_reason: None,
                    input_tokens: None,
                    output_tokens: None,
                    latency_ms: None,
                },
            })
        }

        async fn generate_stream(
            &self,
            _messages: Vec<Message>,
            _tools: Option<Vec<ToolDefinition>>,
        ) -> Result<Option<mpsc::Receiver<Result<StreamChunk>>>> {
            Ok(None)
        }

        fn capabilities(&self) -> &GeneratorCapabilities {
            &self.capabilities
        }

        fn name(&self) -> &str {
            self.name
        }
    }

    #[tokio::test]
    async fn switching_generator_preserves_conversation_history() {
        let mut conversation = ConversationHistory::new();
        conversation.add_user_message("remember this".to_string());
        conversation.add_assistant_message("I will".to_string());
        let before = serde_json::to_value(conversation.snapshot()).unwrap();

        let selection = ModelSelection::new(0, MockGenerator::named("old"));
        selection.activate(1, MockGenerator::named("new")).await;

        assert_eq!(selection.active_index().await, 1);
        assert_eq!(selection.generator().await.name(), "new");
        assert_eq!(
            serde_json::to_value(conversation.snapshot()).unwrap(),
            before
        );
    }

    #[tokio::test]
    async fn in_flight_query_remains_pinned_after_switch() {
        let old = MockGenerator::named("old");
        let selection = ModelSelection::new(0, Arc::clone(&old));
        let pins = GeneratorPins::default();
        let first_query = Uuid::new_v4();

        let first_turn = pins
            .for_turn(first_query, true, selection.generator().await)
            .await;
        selection.activate(1, MockGenerator::named("new")).await;
        let continuation = pins
            .for_turn(first_query, false, selection.generator().await)
            .await;
        let next_query = pins
            .for_turn(Uuid::new_v4(), true, selection.generator().await)
            .await;

        assert_eq!(first_turn.name(), "old");
        assert_eq!(continuation.name(), "old");
        assert_eq!(next_query.name(), "new");
    }

    #[tokio::test]
    async fn loading_local_model_activates_when_ready() {
        let selection = ModelSelection::new(0, MockGenerator::named("cloud"));
        let token = selection.begin_pending(1).await;
        let statuses = Arc::new(Mutex::new(vec![
            LocalModelStatus::Ready("Qwen 3B".to_string()),
            LocalModelStatus::Loading("Qwen 3B".to_string()),
        ]));
        let read_status = {
            let statuses = Arc::clone(&statuses);
            move || {
                let statuses = Arc::clone(&statuses);
                async move { Ok(statuses.lock().await.pop().unwrap()) }
            }
        };

        let outcome = activate_local_when_ready(
            selection.clone(),
            token,
            1,
            MockGenerator::named("local"),
            read_status,
            Duration::ZERO,
        )
        .await;

        assert_eq!(
            outcome,
            LocalActivationOutcome::Activated("Qwen 3B".to_string())
        );
        assert_eq!(selection.active_index().await, 1);
        assert_eq!(selection.pending_index().await, None);
        assert_eq!(selection.generator().await.name(), "local");
    }

    #[tokio::test]
    async fn newer_switch_cancels_pending_local_activation() {
        let selection = ModelSelection::new(0, MockGenerator::named("cloud-a"));
        let token = selection.begin_pending(1).await;
        selection.activate(2, MockGenerator::named("cloud-b")).await;

        let outcome = activate_local_when_ready(
            selection.clone(),
            token,
            1,
            MockGenerator::named("local"),
            || async { Ok(LocalModelStatus::Ready("Qwen 3B".to_string())) },
            Duration::ZERO,
        )
        .await;

        assert_eq!(outcome, LocalActivationOutcome::Cancelled);
        assert_eq!(selection.active_index().await, 2);
        assert_eq!(selection.generator().await.name(), "cloud-b");
    }

    #[tokio::test]
    async fn failed_local_startup_retains_previous_generator() {
        let selection = ModelSelection::new(0, MockGenerator::named("cloud"));
        let token = selection.begin_pending(1).await;

        let outcome = activate_local_when_ready(
            selection.clone(),
            token,
            1,
            MockGenerator::named("local"),
            || async { Ok(LocalModelStatus::Failed("bad weights".to_string())) },
            Duration::ZERO,
        )
        .await;

        assert_eq!(
            outcome,
            LocalActivationOutcome::Failed("bad weights".to_string())
        );
        assert_eq!(selection.active_index().await, 0);
        assert_eq!(selection.pending_index().await, None);
        assert_eq!(selection.generator().await.name(), "cloud");
    }
}
