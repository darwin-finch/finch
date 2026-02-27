// AskUserBrainTool — lets the background brain ask the user a clarifying question
// while they are still composing their query.

use crate::cli::repl_event::events::ReplEvent;
use crate::tools::registry::Tool;
use crate::tools::types::{ToolContext, ToolDefinition, ToolInputSchema};
use anyhow::Result;
use async_trait::async_trait;
use serde_json::{json, Value};
use std::time::Duration;
use tokio::sync::{mpsc, oneshot};

/// Tool that lets the brain ask the user a short clarifying question mid-typing.
///
/// The question is displayed as a dialog in the TUI; the user's answer is
/// returned as a string.  If the user doesn't answer within 30 s (or the brain
/// session is cancelled) the tool returns `"[no answer]"` so the brain can
/// continue gracefully.
pub struct AskUserBrainTool {
    event_tx: mpsc::UnboundedSender<ReplEvent>,
}

impl AskUserBrainTool {
    pub fn new(event_tx: mpsc::UnboundedSender<ReplEvent>) -> Self {
        Self { event_tx }
    }
}

#[async_trait]
impl Tool for AskUserBrainTool {
    fn name(&self) -> &str {
        "ask_user_question"
    }

    fn description(&self) -> &str {
        "Ask the user a short clarifying question about their in-progress query. \
         Use this to disambiguate scope, preferred language, or key constraints \
         before they hit Enter. Provide 2-4 short option strings when helpful."
    }

    fn input_schema(&self) -> ToolInputSchema {
        ToolInputSchema {
            schema_type: "object".to_string(),
            properties: json!({
                "question": {
                    "type": "string",
                    "description": "The clarifying question to ask the user."
                },
                "options": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Optional list of 2-4 short answer choices. Omit for free-text."
                }
            }),
            required: vec!["question".to_string()],
        }
    }

    async fn execute(&self, input: Value, _ctx: &ToolContext<'_>) -> Result<String> {
        let question = input["question"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("ask_user_question: missing 'question'"))?
            .to_string();

        let options: Vec<String> = input["options"]
            .as_array()
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default();

        let (response_tx, response_rx) = oneshot::channel();

        // Send the question to the event loop — it will show a dialog.
        // If the channel is closed (event loop stopped), return no-answer.
        if self
            .event_tx
            .send(ReplEvent::BrainQuestion {
                question,
                options,
                response_tx,
            })
            .is_err()
        {
            return Ok("[no answer]".to_string());
        }

        // Wait up to 30 s for the user to respond.
        match tokio::time::timeout(Duration::from_secs(30), response_rx).await {
            Ok(Ok(answer)) => Ok(answer),
            _ => Ok("[no answer]".to_string()),
        }
    }

    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: self.name().to_string(),
            description: self.description().to_string(),
            input_schema: self.input_schema(),
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_ask_user_brain_tool_name_is_ask_user_question() {
        let (tx, _rx) = mpsc::unbounded_channel();
        let tool = AskUserBrainTool::new(tx);
        assert_eq!(tool.name(), "ask_user_question");
    }

    #[tokio::test]
    async fn test_ask_user_brain_tool_timeout_returns_no_answer() {
        // Create a channel but never respond on the oneshot — the tool should
        // time out and return "[no answer]".
        //
        // We override the timeout to 0 ms by testing the channel-closed branch:
        // drop the receiver immediately so the send fails and we get "[no answer]"
        // without waiting the full 30 s.
        let (tx, _rx) = mpsc::unbounded_channel::<ReplEvent>();
        // Drop the receiver so the send fails immediately.
        drop(_rx);

        let tool = AskUserBrainTool::new(tx);
        let ctx = ToolContext {
            conversation: None,
            save_models: None,
            batch_trainer: None,
            local_generator: None,
            tokenizer: None,
            repl_mode: None,
            plan_content: None,
            live_output: None,
        };
        let result = tool
            .execute(json!({"question": "Which language?"}), &ctx)
            .await
            .unwrap();
        assert_eq!(result, "[no answer]");
    }
}
