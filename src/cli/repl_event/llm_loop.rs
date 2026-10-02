//! LLM worker loop — handles AI query dispatch independently of the TUI.
//!
//! `LlmLoop` runs as a separate Tokio task spawned by `EventLoop::run()`.
//! The TUI event loop sends [`LlmRequest`] messages via a channel; `LlmLoop`
//! spawns individual query tasks whose results flow back to the TUI loop via
//! `event_tx`.
//!
//! This separation keeps LLM I/O (network, tokenisation, tool orchestration)
//! out of the TUI select loop, so the UI stays responsive even during long
//! generation turns.

use std::sync::Arc;
use tokio::sync::{mpsc, Mutex, RwLock};
use uuid::Uuid;

use crate::cli::conversation::ConversationHistory;
use crate::cli::output_manager::OutputManager;
use crate::cli::repl::ReplMode;
use crate::cli::status_bar::StatusBar;
use crate::cli::tui::TuiRenderer;
use crate::generators::Generator;
use crate::models::GeneratorState;
use crate::router::Router;
use crate::tools::ToolDefinition;

use super::events::{LlmRequest, ReplEvent};
use super::model_selection::GeneratorPins;
use super::query_processor::{process_query_with_tools, ActiveToolUsesMap};
use super::query_state::{QueryState, QueryStateManager};
use super::tool_execution::ToolExecutionCoordinator;

/// LLM worker loop — owns AI generation concerns, runs as its own Tokio task.
pub struct LlmLoop {
    /// Receive LLM requests from the TUI event loop.
    llm_rx: mpsc::UnboundedReceiver<LlmRequest>,
    /// Send results (streaming chunks, completion, errors) back to the TUI event loop.
    event_tx: mpsc::UnboundedSender<ReplEvent>,

    // ── LLM-specific state ─────────────────────────────────────────────────
    cloud_gen: Arc<RwLock<Arc<dyn Generator>>>,
    /// Generator snapshot for each top-level query. Tool continuations keep
    /// using this snapshot even if `/model` changes the session default.
    pinned_generators: Arc<GeneratorPins>,
    qwen_gen: Arc<dyn Generator>,
    router: Arc<Router>,
    generator_state: Arc<RwLock<GeneratorState>>,
    /// Configured provider profiles -- used to tell whether the active
    /// session generator is a local profile and to find a configured cloud
    /// one to prefer for summarisation instead (#1236).
    available_providers: Vec<crate::config::ProviderEntry>,
    /// Builds a generator for a configured provider profile on demand.
    provider_resolver: crate::scheduler::ProviderResolver,
    tool_definitions: Arc<RwLock<Vec<ToolDefinition>>>,
    tool_coordinator: ToolExecutionCoordinator,
    /// Shared typed runtime that receives raw provider VM-wire programs.
    program_runtime: Arc<crate::runtime::ProgramRuntime>,
    tool_call_history: super::query_processor::ToolCallHistory,

    // ── Shared state (Arc clones also held by EventLoop) ───────────────────
    conversation: Arc<RwLock<ConversationHistory>>,
    query_states: Arc<QueryStateManager>,
    mode: Arc<RwLock<ReplMode>>,
    output_manager: Arc<OutputManager>,
    status_bar: Arc<StatusBar>,
    tui_renderer: Arc<Mutex<TuiRenderer>>,
    active_tool_uses: ActiveToolUsesMap,
    memory_system: Option<Arc<finch_memory::MemorySystem>>,
    /// Local mirror of the selected Brain's committed memory set, shared
    /// with `EventLoop`'s copy -- see `parts::RuntimeParts` doc comment.
    committed_memories:
        Arc<RwLock<Vec<crate::cli::repl_event::memory_commitment::CommittedMemoryRecord>>>,
    memory_commitment_writer: crate::cli::repl_event::memory_commitment::MemoryCommitmentWriter,
    current_graph: Arc<tokio::sync::Mutex<crate::graph::ExecutionGraph>>,
    /// Live persona selection. Each provider round trip snapshots this value,
    /// so tool continuations and named-Brain turns receive the current persona
    /// without adding frontend-only data to canonical conversation history.
    active_persona: Arc<RwLock<crate::config::Persona>>,

    // ── Per-session config ─────────────────────────────────────────────────
    session_label: String,
    cwd: String,
    context_lines: usize,
    max_verbatim_messages: usize,
    context_recall_k: usize,
    streaming_enabled: bool,
    enable_summarization: bool,
    auto_compact_enabled: bool,
    wire_metrics_logger: Option<Arc<crate::metrics::MetricsLogger>>,
    /// Session-scoped committed conversation summary. Reused across turns so
    /// the summarised request prefix stays byte-stable for prompt caching.
    summary_cache: crate::cli::conversation_compactor::SharedSummaryCache,
}

impl LlmLoop {
    /// Construct the LLM loop.
    ///
    /// `cwd` must already be resolved; pass `EventLoop::cwd` after `run()` has
    /// set it from the process working directory.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        channels: crate::cli::repl_event::parts::LlmChannels,
        generation: crate::cli::repl_event::parts::LlmGeneration,
        tools: crate::cli::repl_event::parts::LlmTools,
        ui: crate::cli::repl_event::parts::LlmUi,
        session: crate::cli::repl_event::parts::LlmSession,
        runtime: crate::cli::repl_event::parts::LlmRuntime,
        limits: crate::cli::repl_event::parts::ContextLimits,
    ) -> Self {
        // Unpacked to the names the body already uses, so the grouping is visible at the boundary
        // and invisible below it.
        let crate::cli::repl_event::parts::LlmChannels {
            requests: llm_rx,
            events: event_tx,
        } = channels;
        let crate::cli::repl_event::parts::LlmGeneration {
            cloud: cloud_gen,
            local: qwen_gen,
            router,
            state: generator_state,
            available_providers,
            provider_resolver,
        } = generation;
        let crate::cli::repl_event::parts::LlmTools {
            definitions: tool_definitions,
            coordinator: tool_coordinator,
            call_history: tool_call_history,
            active_uses: active_tool_uses,
        } = tools;
        let crate::cli::repl_event::parts::LlmUi {
            output: output_manager,
            status_bar,
            renderer: tui_renderer,
            streaming_enabled,
        } = ui;
        let crate::cli::repl_event::parts::LlmSession {
            conversation,
            summary_cache,
            active_persona,
            mode,
            query_states,
            label: session_label,
            cwd,
        } = session;
        let crate::cli::repl_event::parts::LlmRuntime {
            program_runtime,
            memory_system,
            current_graph,
            wire_metrics_logger,
            committed_memories,
            memory_commitment_writer,
        } = runtime;
        let crate::cli::repl_event::parts::ContextLimits {
            lines: context_lines,
            max_verbatim_messages,
            recall_k: context_recall_k,
            enable_summarization,
            auto_compact: auto_compact_enabled,
        } = limits;
        Self {
            llm_rx,
            event_tx,
            cloud_gen,
            pinned_generators: Arc::new(GeneratorPins::default()),
            qwen_gen,
            router,
            generator_state,
            available_providers,
            provider_resolver,
            tool_definitions,
            tool_coordinator,
            program_runtime,
            tool_call_history,
            conversation,
            query_states,
            mode,
            output_manager,
            status_bar,
            tui_renderer,
            active_tool_uses,
            memory_system,
            committed_memories,
            memory_commitment_writer,
            current_graph,
            active_persona,
            session_label,
            cwd,
            context_lines,
            max_verbatim_messages,
            context_recall_k,
            streaming_enabled,
            enable_summarization,
            auto_compact_enabled,
            wire_metrics_logger,
            summary_cache,
        }
    }

    /// Run the LLM worker loop.  Consumes `self`; returns when the request
    /// channel is closed (i.e. when `EventLoop` exits).
    pub async fn run(mut self) {
        while let Some(req) = self.llm_rx.recv().await {
            match req {
                LlmRequest::Query {
                    id,
                    text,
                    no_tools,
                    admission,
                    admission_ready,
                    spawned,
                    publication,
                    pending_echo,
                } => {
                    if let Some(ready) = admission_ready {
                        if ready.send(()).is_err() {
                            continue;
                        }
                    }
                    if let Some(admission) = admission {
                        if admission.await.is_err() {
                            continue;
                        }
                    }
                    self.spawn_query(id, text, no_tools, publication, spawned, pending_echo)
                        .await;
                }
            }
        }
    }

    /// Spawn a background Tokio task for one LLM turn.
    ///
    /// `query = ""` for tool-continuation turns (graph is not reset).
    /// `no_tools = true` suppresses tool definitions for conversational turns.
    async fn spawn_query(
        &self,
        query_id: Uuid,
        query: String,
        no_tools: bool,
        publication: Option<tokio::sync::oneshot::Receiver<()>>,
        spawned: Option<tokio::sync::oneshot::Sender<()>>,
        pending_echo: Option<String>,
    ) {
        // Reset the execution graph on fresh queries (not tool continuations).
        if !query.is_empty() {
            let mut g = self.current_graph.lock().await;
            g.reset(query_id, &self.session_label);
            g.add_node(crate::graph::NodeKind::UserInput {
                text: query.clone(),
            });
        }

        let event_tx = self.event_tx.clone();
        let active_generator = self.cloud_gen.read().await.clone();
        let claude_gen = self
            .pinned_generators
            .for_turn(query_id, !query.is_empty(), active_generator)
            .await;
        let qwen_gen = Arc::clone(&self.qwen_gen);
        let router = Arc::clone(&self.router);
        let generator_state = Arc::clone(&self.generator_state);
        let tool_defs: Arc<Vec<ToolDefinition>> = if no_tools {
            Arc::new(vec![])
        } else {
            Arc::new(self.tool_definitions.read().await.clone())
        };
        let conversation = Arc::clone(&self.conversation);
        let query_states = Arc::clone(&self.query_states);
        let tool_coordinator = self.tool_coordinator.clone();
        let program_runtime = Arc::clone(&self.program_runtime);
        let tui_renderer = Arc::clone(&self.tui_renderer);
        let mode = Arc::clone(&self.mode);
        let output_manager = Arc::clone(&self.output_manager);
        let status_bar = Arc::clone(&self.status_bar);
        let active_tool_uses = Arc::clone(&self.active_tool_uses);
        let memory_system = self.memory_system.clone();
        let memory_commitment = crate::cli::repl_event::memory_commitment::MemoryCommitmentHandle {
            mirror: Arc::clone(&self.committed_memories),
            writer: self.memory_commitment_writer.clone(),
        };
        let session_label = self.session_label.clone();
        let cwd = self.cwd.clone();
        let context_lines = self.context_lines;
        let max_verbatim = self.max_verbatim_messages;
        let recall_k = self.context_recall_k;
        let streaming_enabled = self.streaming_enabled;
        let enable_summarization = self.enable_summarization;
        let auto_compact_enabled = self.auto_compact_enabled;
        let wire_metrics_logger = self.wire_metrics_logger.clone();
        let persona_system_prompt = self.active_persona.read().await.to_system_message();
        // Prefer a genuinely cloud-backed provider for summarisation, even
        // when the active session generator (`claude_gen`, despite the name)
        // is a local profile: summarising conversation history is a harder,
        // more nuanced task than ordinary chat -- "preserve key decisions,
        // code written, errors fixed" -- and a weak local model can produce
        // a confidently wrong summary as easily as it produces confidently
        // wrong program source (#1223, #1226, #1230). Falls back to the
        // active generator unchanged when summarisation is off or no cloud
        // provider is configured, matching the pre-#1236 behaviour.
        let summary_gen = if enable_summarization && max_verbatim > 0 {
            resolve_summary_generator(
                &claude_gen,
                &self.available_providers,
                &self.provider_resolver,
            )
            .await
        } else {
            Arc::clone(&claude_gen)
        };
        let summary_cache = Arc::clone(&self.summary_cache);
        let tool_call_history = Arc::clone(&self.tool_call_history);
        let pinned_generators = Arc::clone(&self.pinned_generators);
        let terminal_query_states = Arc::clone(&query_states);

        tokio::spawn(async move {
            if let Some(publication) = publication {
                if publication.await.is_err() {
                    return;
                }
            }
            process_query_with_tools(
                query_id,
                query,
                event_tx,
                claude_gen,
                qwen_gen,
                router,
                generator_state,
                tool_defs,
                conversation,
                query_states,
                tool_coordinator,
                program_runtime,
                tui_renderer,
                mode,
                output_manager,
                status_bar,
                active_tool_uses,
                memory_system,
                memory_commitment,
                session_label,
                cwd,
                context_lines,
                max_verbatim,
                recall_k,
                streaming_enabled,
                enable_summarization,
                auto_compact_enabled,
                summary_gen,
                summary_cache,
                tool_call_history,
                wire_metrics_logger,
                persona_system_prompt,
                pending_echo,
            )
            .await;

            // Returning in ExecutingTools means another turn with the same ID
            // is imminent. Every other return is terminal (including transport
            // errors that currently leave QueryState as Processing).
            if !matches!(
                terminal_query_states.get_state(query_id).await,
                Some(QueryState::ExecutingTools { .. })
            ) {
                pinned_generators.release(query_id).await;
            }
        });
        if let Some(spawned) = spawned {
            let _ = spawned.send(());
        }
    }
}

/// Choose the generator `ConversationCompactor` should summarise with (#1236).
///
/// `active` is the session's current default generator (`LlmGeneration::cloud`
/// -- the name predates local models becoming selectable through it). When
/// `available_providers` shows `active` is backed by a local profile, this
/// looks for a configured cloud profile and resolves it instead; summarising
/// history well matters more than summarising it with whatever model happens
/// to be chatting. It falls back to `active` unchanged when `active` is
/// already non-local, when no cloud profile is configured, or when resolving
/// the configured cloud profile fails -- the pre-#1236 behaviour in every
/// case, never a hard error.
async fn resolve_summary_generator(
    active: &Arc<dyn Generator>,
    available_providers: &[crate::config::ProviderEntry],
    provider_resolver: &crate::scheduler::ProviderResolver,
) -> Arc<dyn Generator> {
    let active_is_local = available_providers
        .iter()
        .find(|entry| entry.profile_name() == active.name())
        .is_some_and(|entry| entry.is_local());
    if !active_is_local {
        return Arc::clone(active);
    }
    let Some(cloud_entry) = available_providers.iter().find(|entry| !entry.is_local()) else {
        return Arc::clone(active);
    };
    match provider_resolver.resolve_entry(cloud_entry).await {
        Ok(generator) => generator,
        Err(error) => {
            tracing::warn!(
                %error,
                cloud_profile = %cloud_entry.profile_name(),
                "failed to resolve configured cloud provider for summarisation; \
                 falling back to the active (local) generator"
            );
            Arc::clone(active)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ProviderEntry;
    use crate::models::{InferenceProvider, ModelFamily, ModelSize};

    fn local_gemma_entry() -> ProviderEntry {
        ProviderEntry::Local {
            inference_provider: InferenceProvider::LlamaCpp,
            execution_target: crate::config::ExecutionTarget::Auto,
            model_family: ModelFamily::Gemma2,
            model_size: ModelSize::Medium,
            model_path: None,
            managed_artifact: None,
            enabled: true,
            name: Some("local-gemma-2-9b".to_string()),
        }
    }

    fn cloud_claude_entry() -> ProviderEntry {
        ProviderEntry::Claude {
            api_key: "test-api-key".to_string(),
            model: None,
            base_url: None,
            chat_path: None,
            models_path: None,
            name: Some("cloud-claude".to_string()),
        }
    }

    /// A minimal `Generator` standing in for a session's active model,
    /// identified only by the profile name it reports through `name()` --
    /// the same identity `resolve_summary_generator` matches against
    /// `ProviderEntry::profile_name()`.
    struct NamedStubGenerator {
        name: String,
    }

    #[async_trait::async_trait]
    impl Generator for NamedStubGenerator {
        async fn generate(
            &self,
            _messages: Vec<crate::providers::Message>,
            _tools: Option<Vec<ToolDefinition>>,
        ) -> Result<crate::generators::GeneratorResponse, anyhow::Error> {
            unreachable!("resolve_summary_generator must never call generate() itself")
        }

        async fn generate_stream(
            &self,
            _messages: Vec<crate::providers::Message>,
            _tools: Option<Vec<ToolDefinition>>,
        ) -> Result<
            Option<
                tokio::sync::mpsc::Receiver<Result<crate::generators::StreamChunk, anyhow::Error>>,
            >,
            anyhow::Error,
        > {
            Ok(None)
        }

        fn capabilities(&self) -> &crate::generators::GeneratorCapabilities {
            static CAPS: std::sync::OnceLock<crate::generators::GeneratorCapabilities> =
                std::sync::OnceLock::new();
            CAPS.get_or_init(|| crate::generators::GeneratorCapabilities {
                supports_streaming: false,
                supports_tools: false,
                supports_conversation: true,
                max_context_messages: None,
            })
        }

        fn name(&self) -> &str {
            &self.name
        }
    }

    fn stub_generator(name: &str) -> Arc<dyn Generator> {
        Arc::new(NamedStubGenerator {
            name: name.to_string(),
        })
    }

    #[tokio::test]
    async fn test_resolve_summary_generator_prefers_configured_cloud_provider_over_active_local_model(
    ) {
        let local = local_gemma_entry();
        let cloud = cloud_claude_entry();
        let active = stub_generator(&local.profile_name());
        // No daemon client: the active local generator is only ever
        // returned as-is by this function, never resolved through the
        // provider resolver, so a missing daemon must not matter here.
        let resolver = crate::scheduler::ProviderResolver::with_profiles(
            Arc::clone(&active),
            vec![local.clone(), cloud.clone()],
            None,
        );

        let resolved =
            resolve_summary_generator(&active, std::slice::from_ref(&local), &resolver).await;
        assert_eq!(
            resolved.name(),
            local.profile_name(),
            "sanity check: with only the local entry visible there is nothing to switch to"
        );

        let resolved = resolve_summary_generator(&active, &[local, cloud.clone()], &resolver).await;
        assert_eq!(
            resolved.name(),
            cloud.profile_name(),
            "ConversationCompactor::summarize must receive the configured cloud provider, \
             not the active local one, once one is configured: got generator {:?}",
            resolved.name()
        );
    }

    #[tokio::test]
    async fn test_resolve_summary_generator_falls_back_to_active_generator_when_no_cloud_provider_configured(
    ) {
        let local = local_gemma_entry();
        let active = stub_generator(&local.profile_name());
        let resolver = crate::scheduler::ProviderResolver::with_profiles(
            Arc::clone(&active),
            vec![local.clone()],
            None,
        );

        let resolved = resolve_summary_generator(&active, &[local], &resolver).await;
        assert_eq!(
            resolved.name(),
            active.name(),
            "with no cloud provider configured, summarisation must keep using the active \
             generator unchanged (pre-#1236 behaviour): got generator {:?} instead of {:?}",
            resolved.name(),
            active.name()
        );
        assert!(
            Arc::ptr_eq(&resolved, &active),
            "the fallback must return the same Arc, not a freshly constructed generator"
        );
    }

    #[tokio::test]
    async fn test_resolve_summary_generator_leaves_an_already_cloud_active_generator_untouched() {
        let cloud = cloud_claude_entry();
        let active = stub_generator(&cloud.profile_name());
        let resolver = crate::scheduler::ProviderResolver::with_profiles(
            Arc::clone(&active),
            vec![cloud.clone()],
            None,
        );

        let resolved = resolve_summary_generator(&active, &[cloud], &resolver).await;
        assert!(
            Arc::ptr_eq(&resolved, &active),
            "an already cloud-backed active generator must not be swapped for another one"
        );
    }
}
