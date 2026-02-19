// Event handler for MemTree Console
//
// Translates REPL events into tree node operations

use anyhow::Result;
use std::collections::HashMap;
use uuid::Uuid;

use crate::cli::memtree_console::{ConsoleNode, ConsoleNodeType, MemTreeConsole};
use crate::cli::repl_event::ReplEvent;
use crate::memory::NodeId;

/// Maps query IDs to their corresponding tree nodes
pub struct EventHandler {
    /// Map query UUID to root node ID
    query_to_node: HashMap<Uuid, NodeId>,

    /// Map query UUID to current response node (for tool calls)
    query_to_response: HashMap<Uuid, NodeId>,

    /// Pending tool calls (query_id, tool_id) -> node_id
    pending_tools: HashMap<(Uuid, String), NodeId>,
}

impl EventHandler {
    pub fn new() -> Self {
        Self {
            query_to_node: HashMap::new(),
            query_to_response: HashMap::new(),
            pending_tools: HashMap::new(),
        }
    }

    /// Handle a REPL event and update the console tree
    pub fn handle_event(
        &mut self,
        console: &mut MemTreeConsole,
        event: &ReplEvent,
    ) -> Result<()> {
        match event {
            ReplEvent::UserInput { input } => {
                self.handle_user_input(console, input)?;
            }

            ReplEvent::QueryComplete { query_id, response } => {
                self.handle_query_complete(console, *query_id, response)?;
            }

            ReplEvent::QueryFailed { query_id, error } => {
                self.handle_query_failed(console, *query_id, error)?;
            }

            ReplEvent::ToolResult { query_id, tool_id, result } => {
                self.handle_tool_result(console, *query_id, tool_id, result)?;
            }

            ReplEvent::ToolApprovalNeeded { query_id, tool_use, .. } => {
                self.handle_tool_call(console, *query_id, tool_use)?;
            }

            ReplEvent::StatsUpdate { model, input_tokens, output_tokens, latency_ms } => {
                self.handle_stats_update(console, model, *input_tokens, *output_tokens, *latency_ms)?;
            }

            // Ignore these events (they don't affect tree structure)
            ReplEvent::OutputReady { .. } => {}
            ReplEvent::StreamingComplete { .. } => {}
            ReplEvent::CancelQuery => {}
            ReplEvent::Shutdown => {}
        }

        Ok(())
    }

    fn handle_user_input(&mut self, console: &mut MemTreeConsole, input: &str) -> Result<()> {
        let query_id = Uuid::new_v4();

        // Create user message node
        let node_id = console.add_user_message(input.to_string())?;

        // Track query -> node mapping
        self.query_to_node.insert(query_id, node_id);

        Ok(())
    }

    fn handle_query_complete(
        &mut self,
        console: &mut MemTreeConsole,
        query_id: Uuid,
        response: &str,
    ) -> Result<()> {
        if let Some(&parent_id) = self.query_to_node.get(&query_id) {
            // Add assistant response as child of user message
            let response_id = console.add_assistant_response(parent_id, response.to_string())?;

            // Track response node for potential tool calls
            self.query_to_response.insert(query_id, response_id);
        }

        Ok(())
    }

    fn handle_query_failed(
        &mut self,
        console: &mut MemTreeConsole,
        query_id: Uuid,
        error: &str,
    ) -> Result<()> {
        if let Some(&parent_id) = self.query_to_node.get(&query_id) {
            // Add error as assistant response with special styling
            console.add_assistant_response(
                parent_id,
                format!("❌ Error: {}", error),
            )?;
        }

        Ok(())
    }

    fn handle_tool_call(
        &mut self,
        console: &mut MemTreeConsole,
        query_id: Uuid,
        tool_use: &crate::tools::types::ToolUse,
    ) -> Result<()> {
        // Get the response node for this query
        if let Some(&parent_id) = self.query_to_response.get(&query_id) {
            let tool_name = tool_use.name.clone();
            let description = format_tool_description(tool_use);

            // Add tool call node (collapsed by default)
            let tool_node_id = console.add_tool_call(
                parent_id,
                tool_name.clone(),
                description,
            )?;

            // Track pending tool call
            self.pending_tools.insert((query_id, tool_use.id.clone()), tool_node_id);
        }

        Ok(())
    }

    fn handle_tool_result(
        &mut self,
        console: &mut MemTreeConsole,
        query_id: Uuid,
        tool_id: &str,
        result: &Result<String>,
    ) -> Result<()> {
        // Find the tool call node
        if let Some(&tool_node_id) = self.pending_tools.get(&(query_id, tool_id.to_string())) {
            // Add result as child of tool call
            let result_text = match result {
                Ok(output) => {
                    // Truncate long output
                    if output.lines().count() > 10 {
                        let first_lines: Vec<_> = output.lines().take(5).collect();
                        let remaining = output.lines().count() - 5;
                        format!(
                            "{}\n… +{} lines (ctrl+o to expand)",
                            first_lines.join("\n"),
                            remaining
                        )
                    } else {
                        output.clone()
                    }
                }
                Err(e) => format!("✗ Error: {}", e),
            };

            console.add_tool_result(tool_node_id, result.is_ok(), result_text)?;

            // Remove from pending
            self.pending_tools.remove(&(query_id, tool_id.to_string()));
        }

        Ok(())
    }

    fn handle_stats_update(
        &mut self,
        console: &mut MemTreeConsole,
        model: &str,
        input_tokens: Option<u32>,
        output_tokens: Option<u32>,
        latency_ms: Option<u64>,
    ) -> Result<()> {
        // Add thinking/stats node if there's a current query
        if let Some(&parent_id) = self.query_to_node.values().last() {
            let duration_ms = latency_ms.unwrap_or(0);

            let stats_text = format!(
                "{} | {} tokens | {:.1}s",
                model,
                input_tokens.unwrap_or(0) + output_tokens.unwrap_or(0),
                duration_ms as f64 / 1000.0
            );

            console.add_thinking(parent_id, duration_ms, stats_text)?;
        }

        Ok(())
    }
}

/// Format tool use for display
fn format_tool_description(tool_use: &crate::tools::types::ToolUse) -> String {
    match tool_use.name.as_str() {
        "Read" => {
            if let Some(file_path) = tool_use.input.get("file_path") {
                format!("Reading {}", file_path)
            } else {
                "Reading file".to_string()
            }
        }
        "Bash" => {
            if let Some(command) = tool_use.input.get("command") {
                let command_str = command.as_str().unwrap_or("command");
                // Truncate long commands
                if command_str.len() > 60 {
                    format!("{}...", &command_str[..60])
                } else {
                    command_str.to_string()
                }
            } else {
                "Running command".to_string()
            }
        }
        "Glob" => {
            if let Some(pattern) = tool_use.input.get("pattern") {
                format!("Finding {}", pattern)
            } else {
                "Finding files".to_string()
            }
        }
        "Grep" => {
            if let Some(pattern) = tool_use.input.get("pattern") {
                format!("Searching for '{}'", pattern)
            } else {
                "Searching".to_string()
            }
        }
        _ => format!("{} tool", tool_use.name),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::MemTree;
    use std::sync::Arc;
    use tokio::sync::RwLock;

    #[test]
    fn test_user_input_creates_node() {
        let tree = Arc::new(RwLock::new(MemTree::new()));
        let mut console = MemTreeConsole::new(tree);
        let mut handler = EventHandler::new();

        let event = ReplEvent::UserInput {
            input: "Hello".to_string(),
        };

        handler.handle_event(&mut console, &event).unwrap();

        // Should create one user message node
        let visible = console.get_visible_nodes();
        assert_eq!(visible.len(), 1);
    }

    #[test]
    fn test_query_complete_adds_response() {
        let tree = Arc::new(RwLock::new(MemTree::new()));
        let mut console = MemTreeConsole::new(tree);
        let mut handler = EventHandler::new();

        let query_id = Uuid::new_v4();

        // Simulate user input
        handler.handle_user_input(&mut console, "Test").unwrap();

        // Simulate query complete
        handler.query_to_node.insert(query_id, 0); // Map to first node
        handler.handle_query_complete(&mut console, query_id, "Response").unwrap();

        // Should have user message + response
        let visible = console.get_visible_nodes();
        assert_eq!(visible.len(), 2);
    }
}
