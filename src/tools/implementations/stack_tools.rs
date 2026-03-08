// Co-Forth Push tool — lets the AI push an item onto the shared stack.
//
// When the AI calls `Push`, the item becomes visible to the user on the
// stack.  The user can review the full stack with /stack, add their own
// items by typing, and execute everything with /pop.

use crate::tools::registry::Tool;
use crate::tools::types::{ToolContext, ToolInputSchema};
use anyhow::Result;
use async_trait::async_trait;
use serde_json::Value;

pub struct StackPushTool;

#[async_trait]
impl Tool for StackPushTool {
    fn name(&self) -> &str {
        "Push"
    }

    fn description(&self) -> &str {
        "Push an item onto the Co-Forth shared stack.  Both you and the user \
         can push items; the user executes the full stack with /pop.  Use this \
         to accumulate context, constraints, sub-questions, tool results, or \
         observations that the user should review before the final query fires. \
         Each push appears immediately in the user's TUI with its stack index. \
         Optionally specify `tools` (array of tool names) so the node can call \
         those tools when executed — e.g. [\"Bash\", \"Read\"]."
    }

    fn input_schema(&self) -> ToolInputSchema {
        use serde_json::{json, Map};
        let mut props = Map::new();
        props.insert("item".to_string(), json!({
            "type": "string",
            "description": "The text to push onto the stack. Can be a question, \
                            constraint, hypothesis, tool result summary, or context."
        }));
        props.insert("tools".to_string(), json!({
            "type": "array",
            "items": { "type": "string" },
            "description": "Optional list of tool names this node may use when executed, \
                            e.g. [\"Bash\", \"Read\", \"Glob\"]. Omit for plain generation."
        }));
        props.insert("kind".to_string(), json!({
            "type": "string",
            "enum": ["task", "constraint", "question", "observation"],
            "description": "Node kind (default: observation)"
        }));
        ToolInputSchema {
            schema_type: "object".to_string(),
            properties: serde_json::Value::Object(props),
            required: vec!["item".to_string()],
        }
    }

    async fn execute(&self, input: Value, context: &ToolContext<'_>) -> Result<String> {
        let item = input["item"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("Missing 'item' field"))?
            .to_string();

        if item.trim().is_empty() {
            anyhow::bail!("Push item cannot be empty");
        }

        // Optional tool list for the poset node.
        let tools: Vec<String> = input["tools"]
            .as_array()
            .map(|arr| arr.iter().filter_map(|v| v.as_str().map(|s| s.to_string())).collect())
            .unwrap_or_default();

        // Optional node kind (default: observation for AI pushes).
        let kind = match input["kind"].as_str().unwrap_or("observation") {
            "task"        => crate::poset::NodeKind::Task,
            "constraint"  => crate::poset::NodeKind::Constraint,
            "question"    => crate::poset::NodeKind::Question,
            _             => crate::poset::NodeKind::Observation,
        };

        // Add a node to the poset if available.
        if let Some(poset) = &context.poset {
            let mut p = poset.lock().await;
            p.add_node_with_tools(item.clone(), kind, crate::poset::NodeAuthor::Ai, tools.clone());
        }

        match &context.stack {
            Some(stack) => {
                let mut s = stack.lock().await;
                let idx = s.len() + 1;
                s.push(item.clone());
                let depth = s.len();
                drop(s);
                let preview = if item.len() > 60 {
                    format!("{}…", item.chars().take(60).collect::<String>())
                } else {
                    item.clone()
                };
                let tools_note = if tools.is_empty() {
                    String::new()
                } else {
                    format!("  tools:[{}]", tools.join(","))
                };
                Ok(format!(
                    "📚 [{idx}] pushed → \"{preview}\"  (stack depth: {depth}){tools_note}"
                ))
            }
            None => {
                // Stack not available in this context (e.g. brain, qwen generator).
                Ok(format!("(stack unavailable) item: {item}"))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_push_adds_to_stack() {
        use std::sync::Arc;
        use tokio::sync::Mutex;

        let tool = StackPushTool;
        let shared_stack: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));

        let context = ToolContext {
            conversation: None,
            save_models: None,
            batch_trainer: None,
            local_generator: None,
            tokenizer: None,
            repl_mode: None,
            plan_content: None,
            live_output: None,
            stack: Some(Arc::clone(&shared_stack)),
            poset: None,
        };

        let result = tool
            .execute(serde_json::json!({"item": "hello world"}), &context)
            .await
            .unwrap();

        assert!(result.contains("[1]"));
        assert!(result.contains("hello world"));

        let stack = shared_stack.lock().await;
        assert_eq!(stack.len(), 1);
        assert_eq!(stack[0], "hello world");
    }

    #[tokio::test]
    async fn test_push_increments_index() {
        use std::sync::Arc;
        use tokio::sync::Mutex;

        let tool = StackPushTool;
        let shared_stack: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));

        for i in 0..3 {
            let context = ToolContext {
                conversation: None,
                save_models: None,
                batch_trainer: None,
                local_generator: None,
                tokenizer: None,
                repl_mode: None,
                plan_content: None,
                live_output: None,
                stack: Some(Arc::clone(&shared_stack)),
                poset: None,
            };
            let result = tool
                .execute(serde_json::json!({"item": format!("item {i}")}), &context)
                .await
                .unwrap();
            assert!(result.contains(&format!("[{}]", i + 1)));
        }

        assert_eq!(shared_stack.lock().await.len(), 3);
    }

    #[tokio::test]
    async fn test_push_empty_item_returns_error() {
        let tool = StackPushTool;
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
        let result = tool
            .execute(serde_json::json!({"item": "   "}), &context)
            .await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_push_no_stack_context_returns_fallback() {
        let tool = StackPushTool;
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
        let result = tool
            .execute(serde_json::json!({"item": "test"}), &context)
            .await
            .unwrap();
        assert!(result.contains("stack unavailable"));
    }

    #[test]
    fn test_name() {
        assert_eq!(StackPushTool.name(), "Push");
    }
}

// ── Pop ──────────────────────────────────────────────────────────────────────

pub struct StackPopTool;

#[async_trait]
impl Tool for StackPopTool {
    fn name(&self) -> &str {
        "Pop"
    }

    fn description(&self) -> &str {
        "Remove the top item from the Co-Forth shared stack (undo your last push). \
         Use this when you realize your last push was wrong or premature."
    }

    fn input_schema(&self) -> ToolInputSchema {
        ToolInputSchema::simple(vec![])
    }

    async fn execute(&self, _input: Value, context: &ToolContext<'_>) -> Result<String> {
        // Pop from the flat stack.
        let popped = match &context.stack {
            Some(stack) => {
                let mut s = stack.lock().await;
                s.pop()
            }
            None => return Ok("(stack unavailable)".to_string()),
        };

        let Some(item) = popped else {
            return Ok("📚 Stack is already empty.".to_string());
        };

        // Remove the last node from the poset too (keeps them in sync).
        if let Some(poset) = &context.poset {
            let mut p = poset.lock().await;
            if let Some(last_node) = p.nodes.last() {
                let last_id = last_node.id;
                p.nodes.retain(|n| n.id != last_id);
                p.edges.retain(|&(a, b)| a != last_id && b != last_id);
            }
        }

        let depth = match &context.stack {
            Some(s) => s.lock().await.len(),
            None => 0,
        };
        let preview: String = item.chars().take(60).collect();
        let ellipsis = if item.len() > 60 { "…" } else { "" };
        Ok(format!("📚 popped → \"{preview}{ellipsis}\"  (depth:{depth})"))
    }
}

// ── Run ───────────────────────────────────────────────────────────────────────

pub struct StackRunTool;

#[async_trait]
impl Tool for StackRunTool {
    fn name(&self) -> &str {
        "Run"
    }

    fn description(&self) -> &str {
        "Signal that the program is ready to execute — that you and the user \
         have converged on a stack you both agree on. Either party can call /run \
         (user) or this tool (AI) once the vocabulary feels complete. The program \
         executes the nodes in order."
    }

    fn input_schema(&self) -> ToolInputSchema {
        ToolInputSchema::simple(vec![])
    }

    async fn execute(&self, _input: Value, context: &ToolContext<'_>) -> Result<String> {
        match &context.stack {
            Some(stack) => {
                let s = stack.lock().await;
                let count = s.len();
                Ok(format!(
                    "📚 Ready to run ({count} item{} on stack). \
                     Waiting for user to approve with /run.",
                    if count == 1 { "" } else { "s" }
                ))
            }
            None => Ok("📚 Run signalled (stack unavailable in this context).".to_string()),
        }
    }
}

// ── Clear ─────────────────────────────────────────────────────────────────────

pub struct StackClearTool;

#[async_trait]
impl Tool for StackClearTool {
    fn name(&self) -> &str {
        "Clear"
    }

    fn description(&self) -> &str {
        "Clear all items from the Co-Forth shared stack. Use when the current \
         direction is wrong and you want to start fresh."
    }

    fn input_schema(&self) -> ToolInputSchema {
        ToolInputSchema::simple(vec![])
    }

    async fn execute(&self, _input: Value, context: &ToolContext<'_>) -> Result<String> {
        match &context.stack {
            Some(stack) => {
                let mut s = stack.lock().await;
                let count = s.len();
                s.clear();
                Ok(format!(
                    "📚 Cleared {count} item{} from stack.",
                    if count == 1 { "" } else { "s" }
                ))
            }
            None => Ok("(stack unavailable)".to_string()),
        }
    }
}
