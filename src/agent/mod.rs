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

use crate::claude::types::{ContentBlock, Message, MessageRequest};
use crate::claude::ClaudeClient;
use crate::config::constants::DEFAULT_CLAUDE_MODEL;
use crate::config::{persona::Persona, Config};
use crate::generators::claude::CODING_SYSTEM_PROMPT;
use crate::tools::implementations::{
    BashTool, EditTool, GlobTool, GrepTool, PatchTool, ReadTool, WebFetchTool, WriteTool,
};
use crate::tools::types::ToolDefinition;
use crate::tools::{PermissionManager, PermissionRule, ToolExecutor, ToolRegistry};

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
        let model = self
            .config
            .active_teacher()
            .and_then(|t| t.model.clone())
            .unwrap_or_else(|| DEFAULT_CLAUDE_MODEL.to_string());
        let reflector = ReflectionEngine::new(create_client(&self.config)?, model.clone());

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
                max_tokens: crate::config::constants::DEFAULT_MAX_TOKENS,
                messages: messages.clone(),
                system: Some(system.clone()),
                tools: Some(tool_defs.clone()),
            };

            let response = client
                .send_message(&request)
                .await
                .context("Teacher API request failed")?;

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

            // Execute tool calls
            messages.push(response.to_message());
            let tool_uses = response.tool_uses();
            let mut result_blocks = Vec::new();

            for tu in &tool_uses {
                // Log tool use
                let cmd_preview = tu
                    .input
                    .get("command")
                    .or_else(|| tu.input.get("pattern"))
                    .or_else(|| tu.input.get("path"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let _ = logger.log(AgentEvent::ToolUse {
                    tool: tu.name.clone(),
                    cmd: cmd_preview,
                });

                let tool_use = crate::tools::types::ToolUse {
                    id: tu.id.clone(),
                    name: tu.name.clone(),
                    input: tu.input.clone(),
                };

                let exec_result = {
                    let guard = executor.lock().await;
                    guard
                        .execute_tool::<fn() -> anyhow::Result<()>>(
                            &tool_use, None, // conversation
                            None, // save_models_fn
                            None, // batch_trainer
                            None, // local_generator
                            None, // tokenizer
                            None, // repl_mode
                            None, // plan_content
                            None, // live_output
                        )
                        .await
                };

                let (content, is_error) = match exec_result {
                    Ok(result) => (result.content, result.is_error),
                    Err(e) => (format!("Error: {e}"), true),
                };
                result_blocks.push(ContentBlock::tool_result(
                    tu.id.clone(),
                    content,
                    if is_error { Some(true) } else { None },
                ));
            }

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
    let provider = crate::providers::create_provider(&config.teachers)?;
    Ok(ClaudeClient::with_provider(provider))
}

fn truncate(s: &str, max_len: usize) -> &str {
    if s.len() <= max_len {
        s
    } else {
        &s[..max_len]
    }
}
