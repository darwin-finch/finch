//! Bounded child-agent scheduler with structured fork/join results.

use crate::claude::{ContentBlock, Message};
use crate::generators::Generator;
use crate::runtime::ProgramRuntime;
use crate::tools::implementations::{
    GetLanguageDefinitionTool, GetVmStateTool, GlobTool, GrepTool, InspectWordTool, ReadTool,
    SearchWordTool, SubmitProgramTool,
};
use crate::tools::permissions::{PermissionCheck, PermissionManager};
use crate::tools::registry::Tool;
use crate::tools::types::ToolContext;
use crate::vm::EffectSet;
use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::{broadcast, Notify, RwLock, Semaphore};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

const MAX_DEPTH: usize = 4;
const MAX_TURNS: usize = 10;
const MAX_TIMEOUT_MS: u64 = 60 * 60 * 1000;
const MAX_OUTPUT_BYTES: usize = 16 * 1024 * 1024;

#[derive(Clone)]
pub struct ProviderResolver {
    active: Arc<RwLock<Arc<dyn Generator>>>,
    profiles: Arc<Vec<crate::config::ProviderEntry>>,
    daemon_client: Option<Arc<crate::client::DaemonClient>>,
}

impl ProviderResolver {
    pub fn new(active: Arc<dyn Generator>) -> Self {
        Self {
            active: Arc::new(RwLock::new(active)),
            profiles: Arc::new(Vec::new()),
            daemon_client: None,
        }
    }

    pub fn with_profiles(
        active: Arc<dyn Generator>,
        profiles: Vec<crate::config::ProviderEntry>,
        daemon_client: Option<Arc<crate::client::DaemonClient>>,
    ) -> Self {
        Self {
            active: Arc::new(RwLock::new(active)),
            profiles: Arc::new(profiles),
            daemon_client,
        }
    }

    pub async fn activate(&self, generator: Arc<dyn Generator>) {
        *self.active.write().await = generator;
    }

    pub fn generator_handle(&self) -> Arc<RwLock<Arc<dyn Generator>>> {
        Arc::clone(&self.active)
    }

    pub async fn resolve(
        &self,
        provider: Option<&str>,
        model: Option<&str>,
    ) -> Result<Arc<dyn Generator>> {
        let active = self.active.read().await.clone();
        if provider.is_none() && model.is_none() {
            return Ok(active);
        }
        let matches_provider = |entry: &crate::config::ProviderEntry| {
            provider.is_none_or(|requested| {
                requested == entry.profile_name() || requested == entry.provider_type()
            })
        };
        let matches_model = |entry: &crate::config::ProviderEntry| {
            model.is_none_or(|requested| {
                requested == entry.profile_name() || entry.model() == Some(requested)
            })
        };
        let Some(entry) = self
            .profiles
            .iter()
            .find(|entry| matches_provider(entry) && matches_model(entry))
        else {
            let requested = model.or(provider).unwrap_or("unknown");
            if requested == active.name() {
                return Ok(active);
            }
            bail!("NoEligibleModel: no configured profile matches '{requested}'");
        };
        if entry.profile_name() == active.name() {
            return Ok(active);
        }
        if entry.is_local() {
            let client = self.daemon_client.clone().ok_or_else(|| {
                anyhow::anyhow!("NoEligibleModel: local profile requires a running daemon")
            })?;
            return Ok(Arc::new(
                crate::generators::daemon_local::DaemonLocalGenerator::new(
                    client,
                    entry.profile_name(),
                ),
            ));
        }
        let provider = crate::providers::create_provider_from_entry(entry)?;
        let client = crate::claude::ClaudeClient::with_provider(provider);
        let inner: Arc<dyn Generator> = Arc::new(crate::generators::claude::ClaudeGenerator::new(
            Arc::new(client),
        ));
        Ok(Arc::new(crate::generators::ProfiledGenerator::new(
            entry.profile_name(),
            inner,
        )))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentRole {
    General,
    Explore,
    Research,
    Code,
}

impl Default for AgentRole {
    fn default() -> Self {
        Self::General
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentBudget {
    pub max_turns: usize,
    pub timeout_ms: u64,
    pub max_output_bytes: usize,
}

impl Default for AgentBudget {
    fn default() -> Self {
        Self {
            max_turns: MAX_TURNS,
            timeout_ms: 120_000,
            max_output_bytes: 256 * 1024,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentTaskSpec {
    pub task: String,
    #[serde(default)]
    pub role: AgentRole,
    #[serde(default)]
    pub background: Option<String>,
    #[serde(default)]
    pub provider: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub budget: AgentBudget,
}

impl AgentTaskSpec {
    fn validate(&self) -> Result<()> {
        if self.task.trim().is_empty() {
            bail!("agent task cannot be empty");
        }
        if !(1..=MAX_TURNS).contains(&self.budget.max_turns) {
            bail!("agent max_turns must be between 1 and {MAX_TURNS}");
        }
        if !(1..=MAX_TIMEOUT_MS).contains(&self.budget.timeout_ms) {
            bail!("agent timeout_ms must be between 1 and {MAX_TIMEOUT_MS}");
        }
        if !(1..=MAX_OUTPUT_BYTES).contains(&self.budget.max_output_bytes) {
            bail!("agent max_output_bytes must be between 1 and {MAX_OUTPUT_BYTES}");
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentIdentity {
    pub agent_id: Uuid,
    pub task_id: Uuid,
    pub parent_agent_id: Option<Uuid>,
    pub root_agent_id: Uuid,
    pub depth: usize,
    pub provider_model: String,
    pub vm_revision: u64,
    pub manifest_generation: u64,
    /// Inherited authority fixed when this child is created. Later
    /// session/project/global grants cannot silently widen a live child;
    /// an exact task-scoped user approval remains an explicit escalation.
    #[serde(default)]
    pub grant_ceiling: EffectSet,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentTaskStatus {
    Queued,
    Running,
    Completed,
    Failed,
    Cancelled,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentTaskResult {
    pub identity: AgentIdentity,
    pub status: AgentTaskStatus,
    pub final_message: String,
    pub diagnostics: Vec<String>,
    pub turns: usize,
    pub elapsed_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentTaskSnapshot {
    pub identity: AgentIdentity,
    pub task: String,
    pub role: AgentRole,
    pub status: AgentTaskStatus,
    pub result: Option<AgentTaskResult>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AgentEvent {
    TaskQueued {
        snapshot: AgentTaskSnapshot,
    },
    TaskStarted {
        snapshot: AgentTaskSnapshot,
    },
    ToolStarted {
        task_id: Uuid,
        name: String,
    },
    ToolCompleted {
        task_id: Uuid,
        name: String,
        is_error: bool,
    },
    TaskFinished {
        result: AgentTaskResult,
    },
}

struct TaskRecord {
    snapshot: AgentTaskSnapshot,
    cancellation: CancellationToken,
    notify: Arc<Notify>,
}

pub struct AgentScheduler {
    resolver: ProviderResolver,
    runtime: Arc<ProgramRuntime>,
    tasks: RwLock<HashMap<Uuid, TaskRecord>>,
    concurrency: Arc<Semaphore>,
    events: broadcast::Sender<AgentEvent>,
}

impl AgentScheduler {
    pub fn new(resolver: ProviderResolver, runtime: Arc<ProgramRuntime>) -> Arc<Self> {
        let (events, _) = broadcast::channel(256);
        let scheduler = Arc::new(Self {
            resolver,
            runtime: Arc::clone(&runtime),
            tasks: RwLock::new(HashMap::new()),
            concurrency: Arc::new(Semaphore::new(4)),
            events,
        });
        runtime.attach_agent_scheduler(&scheduler);
        scheduler
    }

    pub fn subscribe(&self) -> broadcast::Receiver<AgentEvent> {
        self.events.subscribe()
    }

    pub async fn spawn(
        self: &Arc<Self>,
        spec: AgentTaskSpec,
        parent: Option<&AgentIdentity>,
    ) -> Result<AgentIdentity> {
        spec.validate()?;
        let depth = parent.map_or(0, |identity| identity.depth + 1);
        if depth > MAX_DEPTH {
            bail!("agent nesting depth exceeds {MAX_DEPTH}");
        }
        let provider = self
            .resolver
            .resolve(spec.provider.as_deref(), spec.model.as_deref())
            .await?;
        if !provider.capabilities().supports_tools && spec.role != AgentRole::Research {
            bail!("selected model does not support tools required by this role");
        }
        let task_id = Uuid::new_v4();
        let agent_id = Uuid::new_v4();
        let grant_ceiling = self.runtime.effective_grants_for(parent)?;
        let identity = AgentIdentity {
            agent_id,
            task_id,
            parent_agent_id: parent.map(|identity| identity.agent_id),
            root_agent_id: parent.map_or(agent_id, |identity| identity.root_agent_id),
            depth,
            provider_model: provider.name().to_string(),
            vm_revision: self.runtime.revision(),
            manifest_generation: self.runtime.manifest_generation(),
            grant_ceiling,
        };
        let snapshot = AgentTaskSnapshot {
            identity: identity.clone(),
            task: spec.task.clone(),
            role: spec.role,
            status: AgentTaskStatus::Queued,
            result: None,
        };
        let cancellation = CancellationToken::new();
        self.tasks.write().await.insert(
            task_id,
            TaskRecord {
                snapshot,
                cancellation: cancellation.clone(),
                notify: Arc::new(Notify::new()),
            },
        );
        if let Ok(snapshot) = self.poll(task_id).await {
            let _ = self.events.send(AgentEvent::TaskQueued { snapshot });
        }
        let scheduler = Arc::clone(self);
        let child_identity = identity.clone();
        tokio::spawn(async move {
            scheduler
                .run_task(child_identity, spec, provider, cancellation)
                .await;
        });
        Ok(identity)
    }

    pub async fn poll(&self, task_id: Uuid) -> Result<AgentTaskSnapshot> {
        self.tasks
            .read()
            .await
            .get(&task_id)
            .map(|record| record.snapshot.clone())
            .ok_or_else(|| anyhow::anyhow!("unknown agent task: {task_id}"))
    }

    /// Root callers may inspect all tasks. Child callers may only address their
    /// direct children, which prevents a sibling from observing or controlling
    /// another branch of the task tree.
    pub async fn authorize(&self, task_id: Uuid, caller: Option<&AgentIdentity>) -> Result<()> {
        let tasks = self.tasks.read().await;
        let target = tasks
            .get(&task_id)
            .ok_or_else(|| anyhow::anyhow!("unknown agent task: {task_id}"))?;
        if let Some(caller) = caller {
            if target.snapshot.identity.parent_agent_id != Some(caller.agent_id) {
                bail!("agent task is outside the caller's child scope");
            }
        }
        Ok(())
    }

    pub async fn wait(&self, task_id: Uuid) -> Result<AgentTaskResult> {
        loop {
            let notify = {
                let tasks = self.tasks.read().await;
                let record = tasks
                    .get(&task_id)
                    .ok_or_else(|| anyhow::anyhow!("unknown agent task: {task_id}"))?;
                if let Some(result) = &record.snapshot.result {
                    return Ok(result.clone());
                }
                Arc::clone(&record.notify)
            };
            notify.notified().await;
        }
    }

    pub async fn cancel(&self, task_id: Uuid) -> Result<()> {
        let tasks = self.tasks.read().await;
        let record = tasks
            .get(&task_id)
            .ok_or_else(|| anyhow::anyhow!("unknown agent task: {task_id}"))?;
        record.cancellation.cancel();
        Ok(())
    }

    async fn run_task(
        self: Arc<Self>,
        identity: AgentIdentity,
        spec: AgentTaskSpec,
        provider: Arc<dyn Generator>,
        cancellation: CancellationToken,
    ) {
        let started = Instant::now();
        let permit = tokio::select! {
            permit = Arc::clone(&self.concurrency).acquire_owned() => permit.ok(),
            _ = cancellation.cancelled() => None,
        };
        if permit.is_none() {
            self.finish_cancelled(identity, started).await;
            return;
        }
        {
            let mut tasks = self.tasks.write().await;
            if let Some(record) = tasks.get_mut(&identity.task_id) {
                record.snapshot.status = AgentTaskStatus::Running;
                let _ = self.events.send(AgentEvent::TaskStarted {
                    snapshot: record.snapshot.clone(),
                });
            }
        }

        let execution = tokio::time::timeout(
            std::time::Duration::from_millis(spec.budget.timeout_ms),
            self.agent_loop(&identity, &spec, provider, &cancellation),
        )
        .await;
        drop(permit);

        let (status, message, diagnostics, turns) = match execution {
            Ok(Ok((message, turns))) => (AgentTaskStatus::Completed, message, Vec::new(), turns),
            Ok(Err(error)) if cancellation.is_cancelled() => (
                AgentTaskStatus::Cancelled,
                String::new(),
                vec![error.to_string()],
                0,
            ),
            Ok(Err(error)) => (
                AgentTaskStatus::Failed,
                String::new(),
                vec![error.to_string()],
                0,
            ),
            Err(_) => (
                AgentTaskStatus::Failed,
                String::new(),
                vec!["agent deadline exceeded".to_string()],
                0,
            ),
        };
        let result = AgentTaskResult {
            identity: identity.clone(),
            status,
            final_message: truncate(message, spec.budget.max_output_bytes),
            diagnostics,
            turns,
            elapsed_ms: started.elapsed().as_millis().min(u64::MAX as u128) as u64,
        };
        self.store_result(result).await;
    }

    async fn finish_cancelled(&self, identity: AgentIdentity, started: Instant) {
        self.store_result(AgentTaskResult {
            identity,
            status: AgentTaskStatus::Cancelled,
            final_message: String::new(),
            diagnostics: vec!["cancelled before execution".to_string()],
            turns: 0,
            elapsed_ms: started.elapsed().as_millis().min(u64::MAX as u128) as u64,
        })
        .await;
    }

    async fn store_result(&self, result: AgentTaskResult) {
        let notify = {
            let mut tasks = self.tasks.write().await;
            let Some(record) = tasks.get_mut(&result.identity.task_id) else {
                return;
            };
            record.snapshot.status = result.status;
            record.snapshot.result = Some(result.clone());
            Arc::clone(&record.notify)
        };
        let _ = self.events.send(AgentEvent::TaskFinished { result });
        notify.notify_waiters();
    }

    async fn agent_loop(
        self: &Arc<Self>,
        identity: &AgentIdentity,
        spec: &AgentTaskSpec,
        provider: Arc<dyn Generator>,
        cancellation: &CancellationToken,
    ) -> Result<(String, usize)> {
        let tools = self.child_tools(identity);
        let definitions = tools
            .iter()
            .map(|tool| tool.definition())
            .collect::<Vec<_>>();
        let preamble = format!(
            "You are child agent {} of root {} at depth {}. Your model is {}. \
             VM revision={} manifest_generation={}. Stay within the assigned task and return a final answer.\n\nTask: {}{}",
            identity.agent_id,
            identity.root_agent_id,
            identity.depth,
            identity.provider_model,
            identity.vm_revision,
            identity.manifest_generation,
            spec.task,
            spec.background
                .as_ref()
                .map(|value| format!("\n\nContext from parent:\n{value}"))
                .unwrap_or_default(),
        );
        let mut messages = vec![Message::user(preamble)];

        for turn in 1..=spec.budget.max_turns.clamp(1, MAX_TURNS) {
            let response = tokio::select! {
                response = provider.generate(messages.clone(), Some(definitions.clone())) => response?,
                _ = cancellation.cancelled() => bail!("agent cancelled"),
            };
            if response.tool_uses.is_empty() {
                return Ok((response.text, turn));
            }
            messages.push(Message::with_content("assistant", response.content_blocks));
            let mut results = Vec::with_capacity(response.tool_uses.len());
            for tool_use in response.tool_uses {
                let _ = self.events.send(AgentEvent::ToolStarted {
                    task_id: identity.task_id,
                    name: tool_use.name.clone(),
                });
                let execution = execute_child_tool(&tools, &tool_use.name, tool_use.input).await;
                let (content, is_error) = match execution {
                    Ok(content) => (content, false),
                    Err(error) => (format!("Error: {error}"), true),
                };
                let _ = self.events.send(AgentEvent::ToolCompleted {
                    task_id: identity.task_id,
                    name: tool_use.name,
                    is_error,
                });
                results.push(ContentBlock::tool_result(
                    tool_use.id,
                    content,
                    is_error.then_some(true),
                ));
            }
            messages.push(Message::with_content("user", results));
        }
        bail!("agent reached its turn limit without a final response")
    }

    fn child_tools(self: &Arc<Self>, identity: &AgentIdentity) -> Vec<Box<dyn Tool>> {
        let mut tools: Vec<Box<dyn Tool>> = vec![
            Box::new(ReadTool),
            Box::new(GlobTool),
            Box::new(GrepTool),
            Box::new(SubmitProgramTool::child(
                Arc::clone(&self.runtime),
                identity.clone(),
            )),
            Box::new(GetVmStateTool::new(Arc::clone(&self.runtime))),
            Box::new(GetLanguageDefinitionTool),
            Box::new(SearchWordTool::new(Arc::clone(&self.runtime), None)),
            Box::new(InspectWordTool::new(Arc::clone(&self.runtime), None)),
        ];
        if identity.depth < MAX_DEPTH {
            tools.push(Box::new(
                crate::tools::implementations::AgentSpawnTool::child(
                    Arc::clone(self),
                    identity.clone(),
                ),
            ));
            tools.push(Box::new(
                crate::tools::implementations::AgentAwaitTool::child(
                    Arc::clone(self),
                    identity.clone(),
                ),
            ));
            tools.push(Box::new(
                crate::tools::implementations::AgentPollTool::child(
                    Arc::clone(self),
                    identity.clone(),
                ),
            ));
            tools.push(Box::new(
                crate::tools::implementations::AgentCancelTool::child(
                    Arc::clone(self),
                    identity.clone(),
                ),
            ));
        }
        tools
    }
}

async fn execute_child_tool(tools: &[Box<dyn Tool>], name: &str, input: Value) -> Result<String> {
    let tool = tools
        .iter()
        .find(|tool| tool.name() == name)
        .ok_or_else(|| anyhow::anyhow!("child tool is unavailable: {name}"))?;
    let permissions = PermissionManager::for_peer();
    match permissions.check_tool_use(name, &input) {
        PermissionCheck::Allow => {}
        PermissionCheck::AskUser(reason) => bail!("owner approval required: {reason}"),
        PermissionCheck::Deny(reason) => bail!("permission denied: {reason}"),
    }
    let context = ToolContext {
        conversation: None,
        save_models: None,
        batch_trainer: None,
        local_generator: None,
        tokenizer: None,
        repl_mode: None,
        plan_content: None,
        live_output: None,
        stack: None,
        poset: None,
    };
    tool.execute(input, &context).await
}

fn truncate(mut value: String, max_bytes: usize) -> String {
    if value.len() <= max_bytes {
        return value;
    }
    let mut end = max_bytes;
    while end > 0 && !value.is_char_boundary(end) {
        end -= 1;
    }
    value.truncate(end);
    value.push_str("\n[result truncated]");
    value
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::generators::{GeneratorCapabilities, GeneratorResponse, ResponseMetadata};
    use crate::tools::types::ToolDefinition;
    use crate::vm::{CapabilityKind, CapabilityRequirement, ResourceSelector};
    use async_trait::async_trait;

    fn grant_agent_capabilities(runtime: &ProgramRuntime) {
        for capability in [
            CapabilityKind::AgentSpawn,
            CapabilityKind::AgentAwait,
            CapabilityKind::AgentPoll,
            CapabilityKind::AgentCancel,
        ] {
            runtime
                .grant_typed_capability(CapabilityRequirement {
                    capability,
                    selector: ResourceSelector::None,
                })
                .unwrap();
        }
    }

    struct EchoGenerator;

    #[async_trait]
    impl Generator for EchoGenerator {
        async fn generate(
            &self,
            messages: Vec<Message>,
            _tools: Option<Vec<ToolDefinition>>,
        ) -> Result<GeneratorResponse> {
            Ok(GeneratorResponse {
                text: messages[0].text(),
                content_blocks: vec![ContentBlock::text("done")],
                tool_uses: Vec::new(),
                metadata: ResponseMetadata {
                    generator: "echo".to_string(),
                    model: "echo".to_string(),
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
        ) -> Result<Option<tokio::sync::mpsc::Receiver<Result<crate::generators::StreamChunk>>>>
        {
            Ok(None)
        }

        fn capabilities(&self) -> &GeneratorCapabilities {
            static CAPABILITIES: GeneratorCapabilities = GeneratorCapabilities {
                supports_streaming: false,
                supports_tools: true,
                supports_conversation: true,
                max_context_messages: Some(10),
            };
            &CAPABILITIES
        }

        fn name(&self) -> &str {
            "echo"
        }
    }

    #[tokio::test]
    async fn spawn_returns_identity_and_wait_joins_result() {
        let resolver = ProviderResolver::new(Arc::new(EchoGenerator));
        let scheduler = AgentScheduler::new(resolver, Arc::new(ProgramRuntime::new()));
        let identity = scheduler
            .spawn(
                AgentTaskSpec {
                    task: "inspect".to_string(),
                    role: AgentRole::Explore,
                    background: Some("bounded context".to_string()),
                    provider: None,
                    model: None,
                    budget: AgentBudget::default(),
                },
                None,
            )
            .await
            .unwrap();
        let result = scheduler.wait(identity.task_id).await.unwrap();
        assert_eq!(result.status, AgentTaskStatus::Completed);
        assert!(result.final_message.contains("child agent"));
        assert_eq!(result.identity.depth, 0);
    }

    #[tokio::test]
    async fn explicit_unavailable_model_is_rejected() {
        let resolver = ProviderResolver::new(Arc::new(EchoGenerator));
        let error = match resolver.resolve(Some("other"), None).await {
            Ok(_) => panic!("unexpected provider resolution"),
            Err(error) => error,
        };
        assert!(error.to_string().contains("NoEligibleModel"));
    }

    #[tokio::test]
    async fn spawn_rejects_unbounded_or_empty_resource_budgets() {
        let scheduler = AgentScheduler::new(
            ProviderResolver::new(Arc::new(EchoGenerator)),
            Arc::new(ProgramRuntime::new()),
        );
        let error = scheduler
            .spawn(
                AgentTaskSpec {
                    task: "inspect".into(),
                    role: AgentRole::General,
                    background: None,
                    provider: None,
                    model: None,
                    budget: AgentBudget {
                        max_turns: 0,
                        ..AgentBudget::default()
                    },
                },
                None,
            )
            .await
            .unwrap_err();
        assert!(error.to_string().contains("max_turns"));
        assert!(scheduler.tasks.read().await.is_empty());
    }

    #[tokio::test]
    async fn installed_scheduler_does_not_implicitly_authorize_agent_words() {
        let runtime = Arc::new(ProgramRuntime::new());
        let _scheduler = AgentScheduler::new(
            ProviderResolver::new(Arc::new(EchoGenerator)),
            Arc::clone(&runtime),
        );
        let outcome = runtime
            .submit(crate::runtime::ProgramSubmission {
                language: crate::programs::ProgramLanguage::Forth,
                source_id: None,
                source: r#"s" inspect the VM" agent-spawn"#.to_string(),
                intent: "attempt an ungranted child".to_string(),
                effect: crate::programs::ExecutionEffect::VmWrite,
                declared_capabilities: Vec::new(),
                manifest_generation: runtime.manifest_generation(),
                expected_revision: None,
                budget: None,
            })
            .await
            .unwrap();
        assert_eq!(
            outcome.status,
            crate::runtime::outcome::ExecutionStatus::AuthorizationRequired
        );
        assert!(outcome
            .required_capabilities
            .iter()
            .any(|requirement| { requirement.capability == CapabilityKind::AgentSpawn }));
    }

    #[tokio::test]
    async fn forth_can_fork_and_join_without_shelling_out() {
        let runtime = Arc::new(ProgramRuntime::new());
        grant_agent_capabilities(&runtime);
        let scheduler = AgentScheduler::new(
            ProviderResolver::new(Arc::new(EchoGenerator)),
            Arc::clone(&runtime),
        );
        let outcome = runtime
            .submit(crate::runtime::ProgramSubmission {
                language: crate::programs::ProgramLanguage::Forth,
                source_id: None,
                source: r#"s" inspect the VM" agent-spawn agent-await"#.to_string(),
                intent: "fork and join a child".to_string(),
                effect: crate::programs::ExecutionEffect::VmWrite,
                declared_capabilities: Vec::new(),
                manifest_generation: runtime.manifest_generation(),
                expected_revision: None,
                budget: None,
            })
            .await
            .unwrap();
        assert_eq!(
            outcome.status,
            crate::runtime::outcome::ExecutionStatus::Completed
        );
        assert_eq!(scheduler.tasks.read().await.len(), 1);
    }

    #[tokio::test]
    async fn typed_lisp_can_fork_and_join_without_shelling_out() {
        let runtime = Arc::new(ProgramRuntime::new());
        grant_agent_capabilities(&runtime);
        let scheduler = AgentScheduler::new(
            ProviderResolver::new(Arc::new(EchoGenerator)),
            Arc::clone(&runtime),
        );
        let outcome = runtime
            .submit(crate::runtime::ProgramSubmission {
                language: crate::programs::ProgramLanguage::Lisp,
                source_id: None,
                source: r#"(let ((task-id (agent-spawn "inspect the VM"))) (agent-await task-id))"#
                    .to_string(),
                intent: "fork and join a child".to_string(),
                effect: crate::programs::ExecutionEffect::VmWrite,
                declared_capabilities: Vec::new(),
                manifest_generation: runtime.manifest_generation(),
                expected_revision: None,
                budget: None,
            })
            .await
            .unwrap();
        assert_eq!(
            outcome.status,
            crate::runtime::outcome::ExecutionStatus::Completed
        );
        assert_eq!(
            outcome.backend,
            crate::runtime::outcome::ExecutionBackend::TypedVm
        );
        assert_eq!(scheduler.tasks.read().await.len(), 1);
    }

    #[tokio::test]
    async fn typed_agent_spec_selects_role_context_model_and_budgets() {
        let runtime = Arc::new(ProgramRuntime::new());
        grant_agent_capabilities(&runtime);
        let scheduler = AgentScheduler::new(
            ProviderResolver::new(Arc::new(EchoGenerator)),
            Arc::clone(&runtime),
        );
        let source = r#"(agent-await
            (agent-spawn-with {
                :task "inspect the VM"
                :role "explore"
                :background "focus on typed effects"
                :provider ""
                :model ""
                :max-turns 2
                :timeout-ms 10000
                :max-output-bytes 4096 }))"#;
        let outcome = runtime
            .submit(crate::runtime::ProgramSubmission {
                language: crate::programs::ProgramLanguage::Lisp,
                source_id: None,
                source: source.to_string(),
                intent: "spawn a bounded configured child".to_string(),
                effect: crate::programs::ExecutionEffect::VmWrite,
                declared_capabilities: Vec::new(),
                manifest_generation: runtime.manifest_generation(),
                expected_revision: None,
                budget: None,
            })
            .await
            .unwrap();
        assert_eq!(
            outcome.status,
            crate::runtime::outcome::ExecutionStatus::Completed
        );
        let tasks = scheduler.tasks.read().await;
        let task = tasks.values().next().expect("one structured child task");
        assert_eq!(task.snapshot.role, AgentRole::Explore);
        let result = task.snapshot.result.as_ref().expect("completed child result");
        assert!(result.final_message.contains("focus on typed effects"));
        assert_eq!(result.identity.provider_model, "echo");
    }

    #[tokio::test]
    async fn typed_agent_spec_routes_model_selection_through_the_resolver() {
        let runtime = Arc::new(ProgramRuntime::new());
        grant_agent_capabilities(&runtime);
        let scheduler = AgentScheduler::new(
            ProviderResolver::new(Arc::new(EchoGenerator)),
            Arc::clone(&runtime),
        );
        let source = r#"(agent-spawn-with {
            :task "inspect the VM"
            :role "general"
            :background ""
            :provider ""
            :model "not-configured"
            :max-turns 2
            :timeout-ms 10000
            :max-output-bytes 4096 })"#;
        let outcome = runtime
            .submit(crate::runtime::ProgramSubmission {
                language: crate::programs::ProgramLanguage::Lisp,
                source_id: None,
                source: source.to_string(),
                intent: "reject an unavailable child model".to_string(),
                effect: crate::programs::ExecutionEffect::VmWrite,
                declared_capabilities: Vec::new(),
                manifest_generation: runtime.manifest_generation(),
                expected_revision: None,
                budget: None,
            })
            .await
            .unwrap();
        assert_eq!(
            outcome.status,
            crate::runtime::outcome::ExecutionStatus::Failed
        );
        assert!(outcome
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.contains("NoEligibleModel")));
        assert!(scheduler.tasks.read().await.is_empty());
    }

    #[tokio::test]
    async fn coforth_can_spawn_from_the_same_typed_agent_spec() {
        let runtime = Arc::new(ProgramRuntime::new());
        grant_agent_capabilities(&runtime);
        let scheduler = AgentScheduler::new(
            ProviderResolver::new(Arc::new(EchoGenerator)),
            Arc::clone(&runtime),
        );
        let source = r#"{
            task: "inspect the VM"
            role: "code"
            background: "check shared IR"
            provider: ""
            model: ""
            max-turns: 2
            timeout-ms: 10000
            max-output-bytes: 4096
        } agent-spawn-with agent-await"#;
        let outcome = runtime
            .submit(crate::runtime::ProgramSubmission {
                language: crate::programs::ProgramLanguage::Forth,
                source_id: None,
                source: source.to_string(),
                intent: "spawn the same structured child from Co-Forth".to_string(),
                effect: crate::programs::ExecutionEffect::VmWrite,
                declared_capabilities: Vec::new(),
                manifest_generation: runtime.manifest_generation(),
                expected_revision: None,
                budget: None,
            })
            .await
            .unwrap();
        assert_eq!(
            outcome.status,
            crate::runtime::outcome::ExecutionStatus::Completed
        );
        let tasks = scheduler.tasks.read().await;
        let task = tasks.values().next().expect("one structured child task");
        assert_eq!(task.snapshot.role, AgentRole::Code);
        assert!(task
            .snapshot
            .result
            .as_ref()
            .expect("completed child result")
            .final_message
            .contains("check shared IR"));
    }

    #[tokio::test]
    async fn typed_task_handles_can_be_polled_across_submissions() {
        let runtime = Arc::new(ProgramRuntime::new());
        grant_agent_capabilities(&runtime);
        let scheduler = AgentScheduler::new(
            ProviderResolver::new(Arc::new(EchoGenerator)),
            Arc::clone(&runtime),
        );
        let spawn = runtime
            .submit(crate::runtime::ProgramSubmission {
                language: crate::programs::ProgramLanguage::Lisp,
                source_id: None,
                source: r#"(agent-spawn "inspect the VM")"#.to_string(),
                intent: "start a child for status polling".to_string(),
                effect: crate::programs::ExecutionEffect::VmWrite,
                declared_capabilities: Vec::new(),
                manifest_generation: runtime.manifest_generation(),
                expected_revision: None,
                budget: None,
            })
            .await
            .unwrap();
        assert!(matches!(
            spawn.values.first(),
            Some(crate::programs::ProgramValue::Task(_))
        ));
        let snapshot = runtime.inspect().await.unwrap();
        assert!(matches!(
            snapshot.typed_stack.last().map(|cell| &cell.value_type),
            Some(crate::vm::Type::Task(result)) if **result == crate::vm::Type::String
        ));
        let poll = runtime
            .submit(crate::runtime::ProgramSubmission {
                language: crate::programs::ProgramLanguage::Forth,
                source_id: None,
                source: "agent-poll".to_string(),
                intent: "poll the child".to_string(),
                effect: crate::programs::ExecutionEffect::VmRead,
                declared_capabilities: Vec::new(),
                manifest_generation: runtime.manifest_generation(),
                expected_revision: None,
                budget: None,
            })
            .await;
        assert!(poll.is_ok());
        assert!(matches!(
            poll.unwrap().values.first(),
            Some(crate::programs::ProgramValue::String(_))
        ));
        assert_eq!(scheduler.tasks.read().await.len(), 1);
    }
}
