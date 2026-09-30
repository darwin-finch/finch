// Autonomous agent loop — works through a task backlog independently
//
// Usage:
//   finch agent [--persona <name|path>] [--tasks <path>] [--reflect-every <n>] [--once]

pub mod activity_log;
pub mod backlog;
pub mod reflection;

use anyhow::{Context, Result};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::claude::ClaudeClient;
use crate::claude::MessageRequest;
use crate::config::{Config, Persona};
use crate::generators::CODING_SYSTEM_PROMPT;
use crate::providers::{ContentBlock, EventProvenance, Message};
use crate::tools::ToolDefinition;
use crate::tools::{
    BashTool, EditTool, GlobTool, GrepTool, PatchTool, ReadTool, WebFetchTool, WriteTool,
};
use crate::tools::{PermissionManager, PermissionRule, ToolExecutor, ToolRegistry};
use crate::tools::{
    PreparedCall, ToolCatalog, ToolLoop, ToolLoopIdentity, ToolLoopResult, ToolLoopTerminal,
};

use activity_log::{ActivityLogger, AgentEvent};
use backlog::{AgentTask, TaskBacklog};
use reflection::ReflectionEngine;

/// Configuration for the agent loop
pub struct AgentConfig {
    /// Persona to use (name of builtin or path to .toml file)
    pub persona_spec: String,
    /// Path to tasks.toml
    pub tasks_path: PathBuf,
    /// How many completed tasks between self-reflections
    pub reflect_every: usize,
    /// Stop after completing one task (useful for testing)
    pub once: bool,
}

impl AgentConfig {
    /// Resolve the task file path from flag / cwd / home fallback
    pub fn resolve_tasks_path(override_path: Option<PathBuf>) -> PathBuf {
        if let Some(p) = override_path {
            return p;
        }
        // Check .finch/tasks.toml in current directory
        let cwd_tasks = std::env::current_dir()
            .map(|d| d.join(".finch/tasks.toml"))
            .unwrap_or_default();
        if cwd_tasks.exists() {
            return cwd_tasks;
        }
        // Fall back to ~/.finch/tasks.toml
        dirs::home_dir()
            .map(|h| h.join(".finch/tasks.toml"))
            .unwrap_or_else(|| PathBuf::from(".finch/tasks.toml"))
    }
}

/// The main autonomous agent
pub struct AgentLoop {
    config: Config,
    agent_config: AgentConfig,
}

impl AgentLoop {
    pub fn new(config: Config, agent_config: AgentConfig) -> Self {
        Self {
            config,
            agent_config,
        }
    }

    /// Run the agent loop (returns when `--once` is set or Ctrl-C received)
    pub async fn run(&mut self) -> Result<()> {
        // Load persona
        let (persona, persona_path) = self.load_persona()?;
        println!("Agent persona: {}", persona.name());
        if let Some(ref git_name) = persona.behavior.git_name {
            println!(
                "  Git identity: {} <{}>",
                git_name,
                persona
                    .behavior
                    .git_email
                    .as_deref()
                    .unwrap_or("agent@local.finch")
            );
        }

        // Load backlog
        let mut backlog = TaskBacklog::load(self.agent_config.tasks_path.clone())
            .context("Failed to load task backlog")?;

        let pending_count = backlog
            .tasks()
            .iter()
            .filter(|t| t.status == backlog::TaskStatus::Pending)
            .count();
        println!("Task backlog: {} pending tasks", pending_count);
        println!("Log: {}", ActivityLogger::new()?.today_path().display());
        println!();

        // Set up activity logger and tool executor
        let logger = ActivityLogger::new()?;
        let (executor, tool_defs) = build_tool_executor(&self.config).await?;
        let client = create_client(&self.config)?;

        // Set up reflection engine
        let model = client.model_name().to_string();
        let reflector = ReflectionEngine::new(client.clone(), model.clone());

        let mut completed_count: usize = 0;
        let mut completed_descs: Vec<String> = Vec::new();

        loop {
            // Try to get the next pending task
            let task_id = {
                match backlog.next_pending() {
                    Some(t) => t.id.clone(),
                    None => {
                        if self.agent_config.once {
                            println!("No pending tasks. Exiting (--once).");
                            break;
                        }
                        println!("No pending tasks. Sleeping 60s...");
                        let _ = logger.log(AgentEvent::Idle { sleep_s: 60 });
                        tokio::time::sleep(Duration::from_secs(60)).await;
                        backlog.reload().context("Failed to reload task backlog")?;
                        continue;
                    }
                }
            };

            // Fetch the task info (re-borrow after getting ID)
            let task = backlog
                .tasks()
                .iter()
                .find(|t| t.id == task_id)
                .expect("task must exist")
                .clone();

            println!("[Task {}] {}", task.id, task.description);
            let _ = logger.log(AgentEvent::TaskStart {
                id: task.id.clone(),
                desc: task.description.clone(),
            });
            backlog.mark_running(&task.id)?;

            let start = Instant::now();
            let result = self
                .run_task(
                    &task,
                    &persona,
                    &client,
                    model.clone(),
                    executor.clone(),
                    tool_defs.clone(),
                    &logger,
                )
                .await;

            let duration_s = start.elapsed().as_secs();

            match result {
                Ok(()) => {
                    println!("[Task {}] Done ({:.1}s)", task.id, duration_s);
                    let _ = logger.log(AgentEvent::TaskDone {
                        id: task.id.clone(),
                        duration_s,
                    });
                    backlog.mark_done(&task.id)?;
                    completed_count += 1;
                    completed_descs.push(task.description.clone());

                    // Trigger reflection every N tasks
                    if completed_count.is_multiple_of(self.agent_config.reflect_every) {
                        println!("Running self-reflection after {} tasks...", completed_count);
                        match reflector
                            .reflect(&persona, persona_path.as_deref(), &completed_descs)
                            .await
                        {
                            Ok(summary) if !summary.is_empty() => {
                                println!("Reflection: {}", &summary[..summary.len().min(120)]);
                                let _ = logger.log(AgentEvent::Reflect { summary });
                                completed_descs.clear();
                            }
                            Ok(_) => {}
                            Err(e) => tracing::warn!("Reflection failed: {}", e),
                        }
                    }
                }
                Err(e) => {
                    let reason = format!("{:#}", e);
                    println!(
                        "[Task {}] Failed: {}",
                        task.id,
                        &reason[..reason.len().min(200)]
                    );
                    let _ = logger.log(AgentEvent::TaskFailed {
                        id: task.id.clone(),
                        duration_s,
                        reason: reason.clone(),
                    });
                    backlog.mark_failed(&task.id, &reason)?;
                }
            }

            if self.agent_config.once {
                break;
            }

            // Brief pause between tasks
            tokio::time::sleep(Duration::from_secs(2)).await;
        }

        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    async fn run_task(
        &self,
        task: &AgentTask,
        persona: &Persona,
        client: &ClaudeClient,
        model: String,
        executor: Arc<tokio::sync::Mutex<ToolExecutor>>,
        tool_defs: Vec<ToolDefinition>,
        logger: &ActivityLogger,
    ) -> Result<()> {
        // Build system prompt: coding base + persona + task context
        let repo_cwd = task.repo.as_deref().unwrap_or(".");
        let mut system = format!(
            "{}\n\nWorking directory: {}",
            CODING_SYSTEM_PROMPT, repo_cwd
        );
        let persona_msg = persona.to_system_message();
        if !persona_msg.is_empty() {
            system.push_str("\n\n");
            system.push_str(&persona_msg);
        }

        // Build the initial user message
        let mut user_msg = format!("Task: {}", task.description);
        if let Some(notes) = &task.notes {
            user_msg.push_str(&format!("\n\nNotes: {}", notes));
        }
        if let Some(repo) = &task.repo {
            user_msg.push_str(&format!("\n\nRepository: {}", repo));
        }

        let mut messages = vec![Message::user(&user_msg)];

        const MAX_TURNS: usize = 25;

        for _ in 0..MAX_TURNS {
            let request = MessageRequest {
                model: model.clone(),
                max_tokens: crate::config::DEFAULT_MAX_TOKENS,
                messages: messages.clone(),
                system: Some(system.clone()),
                tools: Some(tool_defs.clone()),
            };

            let response = client
                .send_message(&request)
                .await
                .context("Cloud provider API request failed")?;

            if !response.has_tool_uses() {
                // Final answer — print it and commit any changes
                let text = response.text();
                if !text.is_empty() {
                    println!("{}", text);
                }

                // Auto-commit if git changes exist in the task repo
                if let Some(repo_path) = &task.repo {
                    if let Err(e) = self.maybe_commit(repo_path, task, persona, logger).await {
                        tracing::warn!("Auto-commit failed: {}", e);
                    }
                }
                return Ok(());
            }

            // Execute tool calls. Admitted through the same `ToolLoop` round
            // protocol the REPL and scheduler use (finch-tools-api), so a
            // provider response that repeats a tool-call id fails that id
            // closed instead of running it twice; see issue #1058 and
            // `src/tools/README.md`. This changes only admission bookkeeping,
            // not the agent-mode permission rule (still auto-approve, via the
            // same `executor` built in `build_tool_executor`) or the
            // provider-visible result order (still one result per call,
            // pushed in the order the provider returned them).
            messages.push(response.to_message());
            let tool_uses = response.tool_uses();
            let mut result_blocks = Vec::new();

            let catalog = ToolCatalog::offered(tool_defs.iter().map(|def| def.name.clone()));
            let mut tool_loop = ToolLoop::new(
                ToolLoopIdentity {
                    provider: client.provider_name().to_string(),
                    model: model.clone(),
                    brain: None,
                    run_id: Some(task.id.clone()),
                },
                catalog,
            );
            for (index, tu) in tool_uses.iter().enumerate() {
                tool_loop.observe_complete(
                    tu.id.clone(),
                    tu.name.clone(),
                    tu.input.clone(),
                    EventProvenance {
                        provider: client.provider_name().to_string(),
                        model: model.clone(),
                        event: "tool_call".to_string(),
                        sequence: index as u64 + 1,
                        opaque_replay: None,
                    },
                );
            }

            for call in tool_loop.finish_observation() {
                match call {
                    PreparedCall::Rejected(rejected) => {
                        let _ = logger.log(AgentEvent::ToolUse {
                            tool: rejected.name.clone(),
                            cmd: format!("rejected: {:?}", rejected.reason),
                        });
                        let result = ToolLoopResult::from_reject(&rejected);
                        result_blocks.push(ContentBlock::tool_result(
                            rejected.id,
                            result.content,
                            Some(true),
                        ));
                    }
                    PreparedCall::Ready(validated) => {
                        let Ok(validated) = tool_loop.admit_execution(&validated.id) else {
                            continue;
                        };

                        // Log tool use
                        let cmd_preview = validated
                            .input
                            .get("command")
                            .or_else(|| validated.input.get("pattern"))
                            .or_else(|| validated.input.get("path"))
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string();
                        let _ = logger.log(AgentEvent::ToolUse {
                            tool: validated.name.clone(),
                            cmd: cmd_preview,
                        });

                        let tool_use = crate::tools::ToolUse {
                            id: validated.id.clone(),
                            name: validated.name.clone(),
                            input: validated.input.clone(),
                        };

                        let exec_result = {
                            let guard = executor.lock().await;
                            guard
                                .execute_tool::<fn() -> anyhow::Result<()>>(
                                    &tool_use, None, // save_models_fn
                                    None, // repl_mode
                                    None, // plan_content
                                    None, // live_output
                                    None, // effect_audit
                                    None, // grant_ceiling
                                )
                                .await
                        };

                        let (content, is_error) = match exec_result {
                            Ok(result) => (result.content, result.is_error),
                            Err(e) => (format!("Error: {e}"), true),
                        };
                        let appended = if is_error {
                            ToolLoopResult::error(&validated.id, content)
                        } else {
                            ToolLoopResult::success(&validated.id, content)
                        };
                        let Some(appended) = tool_loop.append_result(appended) else {
                            continue;
                        };
                        result_blocks.push(ContentBlock::tool_result(
                            appended.id,
                            appended.content,
                            appended.is_error.then_some(true),
                        ));
                    }
                }
            }
            tool_loop.terminalize(ToolLoopTerminal::Completed);

            messages.push(Message::with_content("user", result_blocks));
        }

        anyhow::bail!(
            "Reached max tool turns ({}) without completing task",
            MAX_TURNS
        )
    }

    /// Commit any staged/unstaged changes in the repo with the persona's git identity
    async fn maybe_commit(
        &self,
        repo_path: &str,
        task: &AgentTask,
        persona: &Persona,
        logger: &ActivityLogger,
    ) -> Result<()> {
        use std::process::Command;

        // Check if there are any changes to commit
        let status = Command::new("git")
            .args(["-C", repo_path, "status", "--porcelain"])
            .output()
            .context("Failed to run git status")?;

        if status.stdout.is_empty() {
            return Ok(()); // No changes
        }

        // Stage all changes
        let add = Command::new("git")
            .args(["-C", repo_path, "add", "-A"])
            .output()
            .context("Failed to run git add")?;

        if !add.status.success() {
            anyhow::bail!("git add failed: {}", String::from_utf8_lossy(&add.stderr));
        }

        // Build commit message
        let commit_msg = format!(
            "agent: {}\n\nTask ID: {}\nAgent: {}",
            truncate(&task.description, 72),
            task.id,
            persona.name()
        );

        // Determine git identity
        let git_name = persona
            .behavior
            .git_name
            .as_deref()
            .unwrap_or("Finch Agent");
        let git_email = persona
            .behavior
            .git_email
            .as_deref()
            .unwrap_or("agent@local.finch");

        // Commit with persona identity
        let commit = Command::new("git")
            .args([
                "-C",
                repo_path,
                "-c",
                &format!("user.name={}", git_name),
                "-c",
                &format!("user.email={}", git_email),
                "commit",
                "-m",
                &commit_msg,
            ])
            .output()
            .context("Failed to run git commit")?;

        if !commit.status.success() {
            let stderr = String::from_utf8_lossy(&commit.stderr);
            if stderr.contains("nothing to commit") {
                return Ok(());
            }
            anyhow::bail!("git commit failed: {}", stderr);
        }

        // Extract commit hash
        let log = Command::new("git")
            .args(["-C", repo_path, "log", "-1", "--format=%h"])
            .output()
            .context("Failed to get commit hash")?;
        let hash = String::from_utf8_lossy(&log.stdout).trim().to_string();

        println!(
            "  Committed: {} ({})",
            &commit_msg.lines().next().unwrap_or(""),
            hash
        );
        let _ = logger.log(AgentEvent::Commit {
            repo: repo_path.to_string(),
            hash,
            msg: commit_msg.lines().next().unwrap_or("").to_string(),
        });

        Ok(())
    }

    /// Load persona from spec (builtin name, ~/.finch/personas/<name>.toml, or file path)
    fn load_persona(&self) -> Result<(Persona, Option<PathBuf>)> {
        let spec = &self.agent_config.persona_spec;

        // 1. Check if it's an absolute or relative path
        let as_path = PathBuf::from(spec);
        if as_path.exists() {
            let persona = Persona::load(&as_path)
                .with_context(|| format!("Failed to load persona from {}", as_path.display()))?;
            return Ok((persona, Some(as_path)));
        }

        // 2. Check ~/.finch/personas/<name>.toml (user-editable copies)
        if let Some(home) = dirs::home_dir() {
            let user_path = home.join(".finch/personas").join(format!("{}.toml", spec));
            if user_path.exists() {
                let persona = Persona::load(&user_path).with_context(|| {
                    format!("Failed to load persona from {}", user_path.display())
                })?;
                return Ok((persona, Some(user_path)));
            }
        }

        // 3. Fall back to built-in
        let persona =
            Persona::load_builtin(spec).with_context(|| format!("Unknown persona: '{}'", spec))?;
        Ok((persona, None))
    }
}

/// Build the tool executor for agent mode (auto-approve all tools)
async fn build_tool_executor(
    _config: &Config,
) -> Result<(Arc<tokio::sync::Mutex<ToolExecutor>>, Vec<ToolDefinition>)> {
    let mut registry = ToolRegistry::new();
    registry.register(Box::new(ReadTool));
    registry.register(Box::new(GlobTool));
    registry.register(Box::new(GrepTool));
    registry.register(Box::new(WebFetchTool::new()));
    registry.register(Box::new(BashTool));
    registry.register(Box::new(EditTool));
    registry.register(Box::new(PatchTool));
    registry.register(Box::new(WriteTool));

    // In agent mode, auto-approve everything by default
    // (controlled by features.auto_approve_tools, but agent mode always runs headless)
    let permissions = PermissionManager::new().with_default_rule(PermissionRule::Allow);

    let patterns_path = dirs::home_dir()
        .map(|h| h.join(".finch/tool_patterns.json"))
        .unwrap_or_else(|| PathBuf::from(".finch/tool_patterns.json"));

    let executor = ToolExecutor::new(registry, permissions, patterns_path)
        .context("Failed to create tool executor")?;
    let executor = Arc::new(tokio::sync::Mutex::new(executor));

    let tool_defs = executor.lock().await.list_all_tools().await;
    Ok((executor, tool_defs))
}

fn create_client(config: &Config) -> Result<ClaudeClient> {
    let graph = crate::providers::create_provider_graph_from_config(config)?;
    Ok(ClaudeClient::with_shared_provider(graph.default_provider()))
}

fn truncate(s: &str, max_len: usize) -> &str {
    if s.len() <= max_len {
        s
    } else {
        &s[..max_len]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ProviderEntry;

    #[test]
    fn test_saved_legacy_chatgpt_only_config_rejects_agent_startup() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("config.toml");
        Config::with_providers(vec![ProviderEntry::LegacyChatgptSubscription {
            credential_ref: "codex-app-server:managed".to_string(),
            model: Some("gpt-5.6-sol".to_string()),
            name: Some("subscription".to_string()),
        }])
        .save_to(&path)
        .unwrap();
        let config = crate::config::load_config_from_path(&path).unwrap();

        let error = create_client(&config)
            .err()
            .expect("legacy subscription must fail agent startup");
        assert!(error
            .to_string()
            .contains("Legacy chatgpt_subscription profiles are unsupported"));
    }

    // ── ToolLoop admission on the headless agent path (issue #1058) ──────────
    //
    // `run_task` used to call `ToolExecutor::execute_tool` directly for every
    // provider `ToolUse`, with no round-admission lifecycle at all: nothing
    // stopped two tool_use blocks in the same provider response from sharing
    // one tool-call id. The interactive REPL and scheduler both fail closed
    // on that case via `finch-tools-api::ToolLoop`
    // (`test_tool_loop_duplicate_id_fails_closed_without_second_execution`,
    // `test_scheduler_duplicate_tool_id_fails_closed_without_execution`); this
    // production-boundary test proves the same guarantee now holds on the
    // headless `finch agent` loop, through the real `run_task` and the real
    // `ToolExecutor`/`BashTool`, not a helper-only unit test.

    use crate::providers::{
        CapabilitySupport, ContentBlock, ModelCapabilities, ProviderBackend, ProviderResponse,
        ReasoningCapability, StreamChunk, ValidatedProviderRequest, WireProtocol,
    };
    use crate::tools::{BashTool, PermissionManager, PermissionRule, ToolExecutor, ToolRegistry};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tokio::sync::mpsc::Receiver;

    /// Fake provider whose first turn offers two `ToolUse` blocks under the
    /// *same* tool-call id (a hostile-or-buggy-provider shape ToolLoop must
    /// reject), then finishes on the second turn. Never touches the network.
    struct DuplicateIdProvider {
        calls: AtomicUsize,
        marker_path: PathBuf,
    }

    #[async_trait::async_trait]
    impl ProviderBackend for DuplicateIdProvider {
        async fn send_message_validated(
            &self,
            _request: ValidatedProviderRequest,
        ) -> Result<ProviderResponse> {
            let call = self.calls.fetch_add(1, Ordering::SeqCst);
            if call == 0 {
                let marker = self.marker_path.display();
                Ok(ProviderResponse {
                    id: "resp-1".into(),
                    model: "dup-model".into(),
                    content: vec![
                        ContentBlock::ToolUse {
                            id: "call-1".into(),
                            name: "bash".into(),
                            input: serde_json::json!({"command": format!("echo hit >> {marker}")}),
                        },
                        ContentBlock::ToolUse {
                            id: "call-1".into(),
                            name: "bash".into(),
                            input: serde_json::json!({"command": format!("echo hit >> {marker} # second")}),
                        },
                    ],
                    stop_reason: Some("tool_use".into()),
                    role: "assistant".into(),
                    provider: "dup".into(),
                    usage: None,
                    allowance: None,
                })
            } else {
                Ok(ProviderResponse {
                    id: "resp-2".into(),
                    model: "dup-model".into(),
                    content: vec![ContentBlock::Text {
                        text: "done".into(),
                    }],
                    stop_reason: Some("end_turn".into()),
                    role: "assistant".into(),
                    provider: "dup".into(),
                    usage: None,
                    allowance: None,
                })
            }
        }

        async fn send_message_stream_validated(
            &self,
            _request: ValidatedProviderRequest,
        ) -> Result<Receiver<Result<StreamChunk>>> {
            unreachable!("test never streams")
        }

        fn name(&self) -> &str {
            "dup"
        }

        fn default_model(&self) -> &str {
            "dup-model"
        }

        fn capabilities(&self, model: &str) -> ModelCapabilities {
            ModelCapabilities::static_metadata(
                "dup",
                model,
                "2026-01-01",
                "test fixture",
                CapabilitySupport::Supported,
                CapabilitySupport::Supported,
                CapabilitySupport::Unsupported,
                ReasoningCapability::unsupported("2026-01-01", "test fixture"),
                Some(1_000_000),
                Some(64_000),
                None,
            )
            .with_wire_protocol(
                WireProtocol::AnthropicMessages,
                "2026-01-01",
                "test fixture",
            )
        }
    }

    #[tokio::test]
    async fn test_headless_agent_duplicate_tool_call_id_is_not_double_executed() {
        let marker_dir = tempfile::tempdir().unwrap();
        let marker_path = marker_dir.path().join("marker.txt");

        let provider = std::sync::Arc::new(DuplicateIdProvider {
            calls: AtomicUsize::new(0),
            marker_path: marker_path.clone(),
        });
        let client = ClaudeClient::with_shared_provider(provider);

        // Build a headless executor the same way `build_tool_executor` does,
        // with a disposable patterns path instead of the real home directory.
        let mut registry = ToolRegistry::new();
        registry.register(Box::new(BashTool));
        let permissions = PermissionManager::new().with_default_rule(PermissionRule::Allow);
        let patterns_dir = tempfile::tempdir().unwrap();
        let executor = ToolExecutor::new(
            registry,
            permissions,
            patterns_dir.path().join("tool_patterns.json"),
        )
        .expect("test executor must construct");
        let executor = Arc::new(tokio::sync::Mutex::new(executor));
        let tool_defs = executor.lock().await.list_all_tools().await;

        let config = Config::with_providers(Vec::new());
        let agent_config = AgentConfig {
            persona_spec: "default".to_string(),
            tasks_path: PathBuf::from(".finch/tasks.toml"),
            reflect_every: 1,
            once: true,
        };
        let agent = AgentLoop::new(config, agent_config);

        let task = AgentTask {
            id: "t1".to_string(),
            description: "duplicate id regression".to_string(),
            repo: None,
            status: backlog::TaskStatus::Running,
            priority: backlog::TaskPriority::Normal,
            notes: None,
            failure_reason: None,
        };
        let persona = Persona::default();
        let log_dir = tempfile::tempdir().unwrap();
        let logger = ActivityLogger::with_dir(log_dir.path().to_path_buf());

        agent
            .run_task(
                &task,
                &persona,
                &client,
                "dup-model".to_string(),
                executor,
                tool_defs,
                &logger,
            )
            .await
            .expect("run_task must complete despite the duplicate id");

        let contents = std::fs::read_to_string(&marker_path).unwrap_or_default();
        let hits = contents.lines().filter(|line| line.contains("hit")).count();
        assert_eq!(
            hits, 0,
            "a tool-call id repeated with conflicting inputs in one provider turn \
             must never execute (ToolLoop fails the whole id closed, matching \
             test_tool_loop_duplicate_id_fails_closed_without_second_execution: \
             prepared.len()==1 and admit_execution returns NotReady for it); \
             marker file contents={contents:?}"
        );
    }

    /// Fake provider whose first turn asks for a tool name that was never
    /// registered/advertised (`build_tool_executor` only registers a fixed
    /// set), alongside a real `bash` call, then finishes on the second turn.
    struct UnknownToolProvider {
        calls: AtomicUsize,
        marker_path: PathBuf,
    }

    #[async_trait::async_trait]
    impl ProviderBackend for UnknownToolProvider {
        async fn send_message_validated(
            &self,
            _request: ValidatedProviderRequest,
        ) -> Result<ProviderResponse> {
            let call = self.calls.fetch_add(1, Ordering::SeqCst);
            if call == 0 {
                let marker = self.marker_path.display();
                Ok(ProviderResponse {
                    id: "resp-1".into(),
                    model: "dup-model".into(),
                    content: vec![
                        ContentBlock::ToolUse {
                            id: "call-unknown".into(),
                            name: "definitely_not_a_registered_tool".into(),
                            input: serde_json::json!({}),
                        },
                        // A second, real call in the same turn proves the
                        // unknown one is rejected on its own and does not
                        // abort the rest of the turn.
                        ContentBlock::ToolUse {
                            id: "call-real".into(),
                            name: "bash".into(),
                            input: serde_json::json!({"command": format!("echo real >> {marker}")}),
                        },
                    ],
                    stop_reason: Some("tool_use".into()),
                    role: "assistant".into(),
                    provider: "dup".into(),
                    usage: None,
                    allowance: None,
                })
            } else {
                Ok(ProviderResponse {
                    id: "resp-2".into(),
                    model: "dup-model".into(),
                    content: vec![ContentBlock::Text {
                        text: "done".into(),
                    }],
                    stop_reason: Some("end_turn".into()),
                    role: "assistant".into(),
                    provider: "dup".into(),
                    usage: None,
                    allowance: None,
                })
            }
        }

        async fn send_message_stream_validated(
            &self,
            _request: ValidatedProviderRequest,
        ) -> Result<Receiver<Result<StreamChunk>>> {
            unreachable!("test never streams")
        }

        fn name(&self) -> &str {
            "dup"
        }

        fn default_model(&self) -> &str {
            "dup-model"
        }

        fn capabilities(&self, model: &str) -> ModelCapabilities {
            ModelCapabilities::static_metadata(
                "dup",
                model,
                "2026-01-01",
                "test fixture",
                CapabilitySupport::Supported,
                CapabilitySupport::Supported,
                CapabilitySupport::Unsupported,
                ReasoningCapability::unsupported("2026-01-01", "test fixture"),
                Some(1_000_000),
                Some(64_000),
                None,
            )
            .with_wire_protocol(
                WireProtocol::AnthropicMessages,
                "2026-01-01",
                "test fixture",
            )
        }
    }

    #[tokio::test]
    async fn test_headless_agent_unknown_tool_name_never_executes_and_does_not_abort_the_turn() {
        let marker_dir = tempfile::tempdir().unwrap();
        let marker_path = marker_dir.path().join("marker.txt");

        let provider = std::sync::Arc::new(UnknownToolProvider {
            calls: AtomicUsize::new(0),
            marker_path: marker_path.clone(),
        });
        let client = ClaudeClient::with_shared_provider(provider);

        let mut registry = ToolRegistry::new();
        registry.register(Box::new(BashTool));
        let permissions = PermissionManager::new().with_default_rule(PermissionRule::Allow);
        let patterns_dir = tempfile::tempdir().unwrap();
        let executor = ToolExecutor::new(
            registry,
            permissions,
            patterns_dir.path().join("tool_patterns.json"),
        )
        .expect("test executor must construct");
        let executor = Arc::new(tokio::sync::Mutex::new(executor));
        let tool_defs = executor.lock().await.list_all_tools().await;

        let config = Config::with_providers(Vec::new());
        let agent_config = AgentConfig {
            persona_spec: "default".to_string(),
            tasks_path: PathBuf::from(".finch/tasks.toml"),
            reflect_every: 1,
            once: true,
        };
        let agent = AgentLoop::new(config, agent_config);

        let task = AgentTask {
            id: "t2".to_string(),
            description: "unknown tool regression".to_string(),
            repo: None,
            status: backlog::TaskStatus::Running,
            priority: backlog::TaskPriority::Normal,
            notes: None,
            failure_reason: None,
        };
        let persona = Persona::default();
        let log_dir = tempfile::tempdir().unwrap();
        let logger = ActivityLogger::with_dir(log_dir.path().to_path_buf());

        agent
            .run_task(
                &task,
                &persona,
                &client,
                "dup-model".to_string(),
                executor,
                tool_defs,
                &logger,
            )
            .await
            .expect("run_task must complete: the unknown tool must not abort the turn");

        let contents = std::fs::read_to_string(&marker_path).unwrap_or_default();
        assert_eq!(
            contents, "real\n",
            "the never-registered tool name must never execute (no side effect of its \
             own), while the other real call in the same turn still runs; marker file \
             contents={contents:?}"
        );
    }
}
