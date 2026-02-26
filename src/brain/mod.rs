// Brain — background context-gathering agent
//
// When the user starts typing, BrainSession::spawn() launches a lightweight
// agentic loop that reads/searches the codebase and optionally asks the user a
// clarifying question — so that by the time they hit Enter the brain already
// has pre-gathered context ready to be injected into the real query.
//
// Tool set: read, glob, grep, ask_user_question (no bash, no web_fetch).
// Max turns: 6 (3-4 tool calls + summary reply).

mod ask_user;
pub use ask_user::AskUserBrainTool;

use crate::cli::repl_event::events::ReplEvent;
use crate::claude::types::{ContentBlock, Message};
use crate::providers::{LlmProvider, ProviderRequest};
use crate::tools::implementations::glob::GlobTool;
use crate::tools::implementations::grep::GrepTool;
use crate::tools::implementations::read::ReadTool;
use crate::tools::registry::Tool;
use crate::tools::types::{ToolContext, ToolDefinition, ToolUse};
use anyhow::Result;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio::sync::{mpsc, RwLock};
use tokio_util::sync::CancellationToken;
use tracing::{debug, info};
use uuid::Uuid;

/// Maximum turns the brain may run.  6 allows 3-4 tool calls + final summary.
const BRAIN_MAX_TURNS: usize = 6;

/// A running background brain session.
///
/// Drop or call [`cancel`] to stop the brain immediately.
pub struct BrainSession {
    pub id: Uuid,
    cancel: CancellationToken,
    /// Set to `true` by `cancel()` before the token fires.  The brain task
    /// checks this flag before writing its summary so a stale session whose
    /// `run_brain_loop` future happened to finish at the same instant as
    /// cancellation doesn't overwrite the brain_context that now belongs to
    /// a newer session.
    cancelled: Arc<AtomicBool>,
}

impl BrainSession {
    /// Spawn a brain loop in the background.
    ///
    /// The brain writes its final context summary into `brain_context` once it
    /// finishes.  Events (e.g. `BrainQuestion`) are sent on `event_tx`.
    pub fn spawn(
        partial_input: String,
        provider: Arc<dyn LlmProvider>,
        event_tx: mpsc::UnboundedSender<ReplEvent>,
        brain_context: Arc<RwLock<Option<String>>>,
        cwd: String,
    ) -> Self {
        let id = Uuid::new_v4();
        let cancel = CancellationToken::new();
        let cancel_clone = cancel.clone();
        let cancelled = Arc::new(AtomicBool::new(false));
        let cancelled_clone = Arc::clone(&cancelled);

        let handle = tokio::spawn(async move {
            tokio::select! {
                _ = cancel_clone.cancelled() => {
                    debug!("Brain {} cancelled", id);
                }
                result = run_brain_loop(
                    &partial_input,
                    provider.as_ref(),
                    event_tx,
                    &cwd,
                ) => {
                    match result {
                        Ok(summary) => {
                            // Guard against post-cancel writes: if this session was
                            // cancelled just as run_brain_loop completed, discard.
                            if cancelled_clone.load(Ordering::Acquire) {
                                debug!("Brain {} finished but was cancelled — discarding summary", id);
                            } else {
                                debug!("Brain {} finished, writing {} chars", id, summary.len());
                                *brain_context.write().await = Some(summary);
                            }
                        }
                        Err(e) => {
                            debug!("Brain {} error: {}", id, e);
                        }
                    }
                }
            }
        });

        // Propagate task panics to the log so they aren't silently lost.
        tokio::spawn(async move {
            if let Err(e) = handle.await {
                tracing::error!("Brain task panicked: {:?}", e);
            }
        });

        Self { id, cancel, cancelled }
    }

    /// Cancel the brain session.  Idempotent.
    ///
    /// Sets the `cancelled` flag **before** firing the `CancellationToken` so
    /// the task's write-guard check is guaranteed to be ordered correctly.
    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::Release);
        self.cancel.cancel();
    }
}

// ---------------------------------------------------------------------------
// Brain loop (headless, no permission checks)
// ---------------------------------------------------------------------------

/// System prompt template.  `{cwd}` is replaced at call time.
fn brain_system_prompt(cwd: &str) -> String {
    format!(
        "You are a background context-gathering agent. The user is composing a query \
         in their terminal.\n\
         Your job: speculatively read and search the codebase to pre-gather relevant \
         context so the main AI has a head start.\n\
         You may ask the user short clarifying questions using the ask_user_question tool.\n\
         Available tools: read, glob, grep, ask_user_question.\n\
         Stop after 3-5 tool calls. Summarise your findings concisely (200-400 words).\n\
         Working directory: {cwd}",
        cwd = cwd
    )
}

async fn run_brain_loop(
    partial_input: &str,
    provider: &dyn LlmProvider,
    event_tx: mpsc::UnboundedSender<ReplEvent>,
    cwd: &str,
) -> Result<String> {
    let system = brain_system_prompt(cwd);

    // Build brain tools (read-only + ask_user_question)
    let tools: Vec<Box<dyn Tool>> = vec![
        Box::new(ReadTool),
        Box::new(GlobTool),
        Box::new(GrepTool),
        Box::new(AskUserBrainTool::new(event_tx)),
    ];
    let tool_defs: Vec<ToolDefinition> = tools.iter().map(|t| t.definition()).collect();

    let task = format!(
        "The user is typing: \"{}\"\n\n\
         Search the codebase for relevant context.",
        partial_input
    );

    let mut messages: Vec<Message> = vec![Message::user(&task)];

    for turn in 0..BRAIN_MAX_TURNS {
        debug!("Brain turn {}/{}", turn + 1, BRAIN_MAX_TURNS);

        let request = ProviderRequest::new(messages.clone())
            .with_system(system.clone())
            .with_max_tokens(2048)
            .with_tools(tool_defs.clone());

        let response = provider
            .send_message(&request)
            .await
            .map_err(|e| anyhow::anyhow!("Brain provider error: {}", e))?;

        if !response.has_tool_uses() {
            let text = response.text();
            info!("Brain finished in {} turns ({} chars)", turn + 1, text.len());
            return Ok(text);
        }

        messages.push(response.to_message());

        let tool_uses = response.tool_uses();
        let mut result_blocks: Vec<ContentBlock> = Vec::with_capacity(tool_uses.len());

        for tool_use in &tool_uses {
            debug!("Brain calling tool: {}", tool_use.name);
            let (content, is_error) = match execute_brain_tool(&tools, tool_use).await {
                Ok(out) => (out, false),
                Err(e) => (format!("Error: {}", e), true),
            };
            result_blocks.push(ContentBlock::ToolResult {
                tool_use_id: tool_use.id.clone(),
                content,
                is_error: if is_error { Some(true) } else { None },
            });
        }

        messages.push(Message::with_content("user", result_blocks));
    }

    anyhow::bail!(
        "Brain reached max_turns ({}) without producing a summary",
        BRAIN_MAX_TURNS
    )
}

/// Execute one tool inside the brain (no permission checks).
async fn execute_brain_tool(tools: &[Box<dyn Tool>], tool_use: &ToolUse) -> Result<String> {
    let tool = tools
        .iter()
        .find(|t| t.name() == tool_use.name)
        .ok_or_else(|| {
            anyhow::anyhow!(
                "Brain tool '{}' not available",
                tool_use.name
            )
        })?;

    let context = ToolContext {
        conversation: None,
        save_models: None,
        batch_trainer: None,
        local_generator: None,
        tokenizer: None,
        repl_mode: None,
        plan_content: None,
        live_output: None,
    };

    tool.execute(tool_use.input.clone(), &context).await
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_brain_tools_are_read_only() {
        // The brain has exactly read, glob, grep, ask_user_question — no bash.
        let (tx, _rx) = mpsc::unbounded_channel::<ReplEvent>();
        let tools: Vec<Box<dyn Tool>> = vec![
            Box::new(ReadTool),
            Box::new(GlobTool),
            Box::new(GrepTool),
            Box::new(AskUserBrainTool::new(tx)),
        ];
        let names: Vec<&str> = tools.iter().map(|t| t.name()).collect();
        assert!(names.contains(&"read"));
        assert!(names.contains(&"glob"));
        assert!(names.contains(&"grep"));
        assert!(names.contains(&"ask_user_question"));
        assert!(!names.contains(&"bash"), "brain must not have bash");
        assert!(!names.contains(&"web_fetch"), "brain must not have web_fetch");
    }

    #[test]
    fn test_brain_system_prompt_includes_cwd() {
        let prompt = brain_system_prompt("/Users/test/project");
        assert!(
            prompt.contains("/Users/test/project"),
            "system prompt should contain cwd"
        );
    }

    #[tokio::test]
    async fn test_brain_session_cancel_drops_without_panic() {
        // Create a minimal session and cancel it immediately.
        // The provider Arc is a placeholder that never gets called because we
        // cancel before the loop can run a turn.
        use crate::providers::types::ProviderResponse;
        use crate::providers::LlmProvider;
        use async_trait::async_trait;
        use tokio::sync::mpsc::Receiver;

        struct NeverProvider;
        #[async_trait]
        impl LlmProvider for NeverProvider {
            async fn send_message(
                &self,
                _req: &ProviderRequest,
            ) -> Result<ProviderResponse> {
                anyhow::bail!("NeverProvider should not be called")
            }
            async fn send_message_stream(
                &self,
                _req: &ProviderRequest,
            ) -> Result<Receiver<Result<crate::providers::StreamChunk>>> {
                anyhow::bail!("NeverProvider streaming not supported")
            }
            fn name(&self) -> &str {
                "never"
            }
            fn default_model(&self) -> &str {
                "never"
            }
        }

        let (tx, _rx) = mpsc::unbounded_channel::<ReplEvent>();
        let ctx = Arc::new(RwLock::new(None::<String>));
        let session = BrainSession::spawn(
            "test input".to_string(),
            Arc::new(NeverProvider),
            tx,
            ctx,
            "/tmp".to_string(),
        );
        // Cancel immediately — must not panic.
        session.cancel();
        // Give the task a moment to observe the cancellation.
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }

    #[tokio::test]
    async fn test_brain_session_ids_are_unique() {
        // Two BrainSessions must have different UUIDs.
        use crate::providers::types::ProviderResponse;
        use crate::providers::LlmProvider;
        use async_trait::async_trait;
        use tokio::sync::mpsc::Receiver;

        struct NeverProvider;
        #[async_trait]
        impl LlmProvider for NeverProvider {
            async fn send_message(&self, _r: &ProviderRequest) -> Result<ProviderResponse> {
                anyhow::bail!("unused")
            }
            async fn send_message_stream(
                &self,
                _r: &ProviderRequest,
            ) -> Result<Receiver<Result<crate::providers::StreamChunk>>> {
                anyhow::bail!("unused")
            }
            fn name(&self) -> &str {
                "never"
            }
            fn default_model(&self) -> &str {
                "never"
            }
        }

        let make_session = || {
            let (tx, _rx) = mpsc::unbounded_channel::<ReplEvent>();
            let ctx = Arc::new(RwLock::new(None::<String>));
            BrainSession::spawn(
                "input".to_string(),
                Arc::new(NeverProvider),
                tx,
                ctx,
                "/tmp".to_string(),
            )
        };

        let s1 = make_session();
        let s2 = make_session();
        assert_ne!(s1.id, s2.id, "each BrainSession should have a unique UUID");
        s1.cancel();
        s2.cancel();
    }
}
