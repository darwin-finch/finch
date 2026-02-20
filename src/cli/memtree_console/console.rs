// MemTree Console - Tree-structured conversation interface
//
// Organizes conversation history as a navigable tree where:
// - User messages are parent nodes
// - Assistant responses are children
// - Tool calls are children of responses
// - Can expand/collapse and navigate the tree

use anyhow::Result;
use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    widgets::{Block, Borders, List, ListItem, Paragraph},
    Frame,
};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;

use crate::memory::{MemTree, NodeId};

/// A node in the console tree view
#[derive(Debug, Clone)]
pub struct ConsoleNode {
    /// Unique node ID
    pub id: NodeId,

    /// Node type (user message, assistant response, tool call, etc.)
    pub node_type: ConsoleNodeType,

    /// Display text for this node
    pub text: String,

    /// Child node IDs
    pub children: Vec<NodeId>,

    /// Whether this node is expanded in the view
    pub expanded: bool,

    /// Indentation level (depth in tree)
    pub depth: usize,

    /// Timestamp
    pub timestamp: std::time::SystemTime,
}

/// Types of nodes in the console tree
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConsoleNodeType {
    /// User message/query
    UserMessage,

    /// Assistant response
    AssistantResponse,

    /// Tool call (Read, Bash, etc.)
    ToolCall { tool_name: String },

    /// Tool result
    ToolResult { success: bool },

    /// System message
    System,

    /// Thinking/processing indicator
    Thinking { duration_ms: u64 },
}

/// MemTree Console state
pub struct MemTreeConsole {
    /// The underlying memory tree
    tree: Arc<RwLock<MemTree>>,

    /// Mapping from NodeId to ConsoleNode for quick lookup
    nodes: HashMap<NodeId, ConsoleNode>,

    /// Current root node for display (can navigate into branches)
    display_root: Option<NodeId>,

    /// Currently selected node
    selected: Option<NodeId>,

    /// Scroll offset for viewport
    scroll_offset: usize,

    /// Input buffer for new messages
    input_buffer: String,
}

impl MemTreeConsole {
    /// Create a new MemTree console
    pub fn new(tree: Arc<RwLock<MemTree>>) -> Self {
        Self {
            tree,
            nodes: HashMap::new(),
            display_root: None,
            selected: None,
            scroll_offset: 0,
            input_buffer: String::new(),
        }
    }

    /// Generate a new unique node ID
    fn next_id(&mut self) -> NodeId {
        let id = self.nodes.len() as NodeId;
        id
    }

    /// Add a user message node
    pub fn add_user_message(&mut self, text: String) -> Result<NodeId> {
        let node_id = self.next_id();

        let console_node = ConsoleNode {
            id: node_id,
            node_type: ConsoleNodeType::UserMessage,
            text,
            children: Vec::new(),
            expanded: true,
            depth: 0,
            timestamp: std::time::SystemTime::now(),
        };

        self.nodes.insert(node_id, console_node);

        // Set as display root if this is the first message
        if self.display_root.is_none() {
            self.display_root = Some(node_id);
        }

        Ok(node_id)
    }

    /// Add an assistant response node as child of current message
    pub fn add_assistant_response(&mut self, parent_id: NodeId, text: String) -> Result<NodeId> {
        let node_id = self.next_id();

        let parent_depth = self.nodes.get(&parent_id).map(|n| n.depth).unwrap_or(0);

        let console_node = ConsoleNode {
            id: node_id,
            node_type: ConsoleNodeType::AssistantResponse,
            text,
            children: Vec::new(),
            expanded: true,
            depth: parent_depth + 1,
            timestamp: std::time::SystemTime::now(),
        };

        self.nodes.insert(node_id, console_node);

        // Add to parent's children
        if let Some(parent) = self.nodes.get_mut(&parent_id) {
            parent.children.push(node_id);
        }

        Ok(node_id)
    }

    /// Add a tool call node
    pub fn add_tool_call(&mut self, parent_id: NodeId, tool_name: String, description: String) -> Result<NodeId> {
        let node_id = self.next_id();

        let parent_depth = self.nodes.get(&parent_id).map(|n| n.depth).unwrap_or(0);

        let console_node = ConsoleNode {
            id: node_id,
            node_type: ConsoleNodeType::ToolCall { tool_name: tool_name.clone() },
            text: description,
            children: Vec::new(),
            expanded: false, // Collapsed by default
            depth: parent_depth + 1,
            timestamp: std::time::SystemTime::now(),
        };

        self.nodes.insert(node_id, console_node);

        if let Some(parent) = self.nodes.get_mut(&parent_id) {
            parent.children.push(node_id);
        }

        Ok(node_id)
    }

    /// Add a tool result node
    pub fn add_tool_result(&mut self, parent_id: NodeId, success: bool, text: String) -> Result<NodeId> {
        let node_id = self.next_id();

        let parent_depth = self.nodes.get(&parent_id).map(|n| n.depth).unwrap_or(0);

        let console_node = ConsoleNode {
            id: node_id,
            node_type: ConsoleNodeType::ToolResult { success },
            text,
            children: Vec::new(),
            expanded: true,
            depth: parent_depth + 1,
            timestamp: std::time::SystemTime::now(),
        };

        self.nodes.insert(node_id, console_node);

        if let Some(parent) = self.nodes.get_mut(&parent_id) {
            parent.children.push(node_id);
        }

        Ok(node_id)
    }

    /// Add a thinking/stats node
    pub fn add_thinking(&mut self, parent_id: NodeId, duration_ms: u64, text: String) -> Result<NodeId> {
        let node_id = self.next_id();

        let parent_depth = self.nodes.get(&parent_id).map(|n| n.depth).unwrap_or(0);

        let console_node = ConsoleNode {
            id: node_id,
            node_type: ConsoleNodeType::Thinking { duration_ms },
            text,
            children: Vec::new(),
            expanded: true,
            depth: parent_depth + 1,
            timestamp: std::time::SystemTime::now(),
        };

        self.nodes.insert(node_id, console_node);

        if let Some(parent) = self.nodes.get_mut(&parent_id) {
            parent.children.push(node_id);
        }

        Ok(node_id)
    }

    /// Toggle expansion of a node
    pub fn toggle_expand(&mut self, node_id: NodeId) {
        if let Some(node) = self.nodes.get_mut(&node_id) {
            node.expanded = !node.expanded;
        }
    }

    /// Get all visible nodes for rendering (respects expanded/collapsed state)
    pub fn get_visible_nodes(&self) -> Vec<NodeId> {
        let mut visible = Vec::new();

        if let Some(root_id) = self.display_root {
            self.collect_visible_recursive(root_id, &mut visible);
        }

        visible
    }

    /// Recursively collect visible nodes
    fn collect_visible_recursive(&self, node_id: NodeId, visible: &mut Vec<NodeId>) {
        visible.push(node_id);

        if let Some(node) = self.nodes.get(&node_id) {
            if node.expanded {
                for child_id in &node.children {
                    self.collect_visible_recursive(*child_id, visible);
                }
            }
        }
    }

    /// Render the tree view
    pub fn render(&self, f: &mut Frame, area: Rect) {
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Min(3),      // Tree view
                Constraint::Length(3),   // Input area
            ])
            .split(area);

        // Render tree
        self.render_tree(f, chunks[0]);

        // Render input
        self.render_input(f, chunks[1]);
    }

    /// Render the tree view
    fn render_tree(&self, f: &mut Frame, area: Rect) {
        let visible_nodes = self.get_visible_nodes();

        let items: Vec<ListItem> = visible_nodes
            .iter()
            .skip(self.scroll_offset)
            .filter_map(|node_id| {
                self.nodes.get(node_id).map(|node| {
                    self.render_node(node, Some(*node_id) == self.selected)
                })
            })
            .collect();

        let list = List::new(items)
            .block(Block::default()
                .borders(Borders::ALL)
                .title("Conversation Tree"));

        f.render_widget(list, area);
    }

    /// Render a single node
    fn render_node(&self, node: &ConsoleNode, is_selected: bool) -> ListItem<'_> {
        let indent = "  ".repeat(node.depth);
        let expand_marker = if !node.children.is_empty() {
            if node.expanded { "▼ " } else { "▶ " }
        } else {
            "  "
        };

        let (icon, color) = match &node.node_type {
            ConsoleNodeType::UserMessage => ("❯", Color::Cyan),
            ConsoleNodeType::AssistantResponse => ("⏺", Color::Blue),
            ConsoleNodeType::ToolCall { tool_name: _ } => ("⏵", Color::Yellow),
            ConsoleNodeType::ToolResult { success } => {
                if *success { ("✓", Color::Green) } else { ("✗", Color::Red) }
            }
            ConsoleNodeType::System => ("ℹ", Color::DarkGray),
            ConsoleNodeType::Thinking { .. } => ("✻", Color::Magenta),
        };

        let style = if is_selected {
            Style::default().bg(Color::Black).fg(Color::White).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(color)
        };

        let text = format!("{}{}{} {}", indent, expand_marker, icon, node.text);

        ListItem::new(text).style(style)
    }

    /// Render input area
    fn render_input(&self, f: &mut Frame, area: Rect) {
        let input = Paragraph::new(self.input_buffer.as_str())
            .block(Block::default().borders(Borders::ALL).title("Input"))
            .style(Style::default().fg(Color::White));

        f.render_widget(input, area);
    }

    /// Navigate selection up
    pub fn select_previous(&mut self) {
        let visible = self.get_visible_nodes();
        if visible.is_empty() {
            return;
        }

        if let Some(current) = self.selected {
            if let Some(pos) = visible.iter().position(|id| *id == current) {
                if pos > 0 {
                    self.selected = Some(visible[pos - 1]);
                }
            }
        } else {
            self.selected = visible.first().copied();
        }
    }

    /// Navigate selection down
    pub fn select_next(&mut self) {
        let visible = self.get_visible_nodes();
        if visible.is_empty() {
            return;
        }

        if let Some(current) = self.selected {
            if let Some(pos) = visible.iter().position(|id| *id == current) {
                if pos < visible.len() - 1 {
                    self.selected = Some(visible[pos + 1]);
                }
            }
        } else {
            self.selected = visible.first().copied();
        }
    }

    /// Toggle expansion of selected node
    pub fn toggle_selected(&mut self) {
        if let Some(selected_id) = self.selected {
            self.toggle_expand(selected_id);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_basic_tree_structure() {
        let tree = Arc::new(RwLock::new(MemTree::new()));
        let mut console = MemTreeConsole::new(tree);

        // Add user message
        let user_id = console.add_user_message("Hello".to_string()).unwrap();

        // Add assistant response
        let response_id = console.add_assistant_response(user_id, "Hi there!".to_string()).unwrap();

        // Add tool call
        let tool_id = console.add_tool_call(response_id, "Read".to_string(), "file.txt".to_string()).unwrap();

        // Verify structure
        assert_eq!(console.nodes.len(), 3);
        assert_eq!(console.nodes.get(&user_id).unwrap().children.len(), 1);
        assert_eq!(console.nodes.get(&response_id).unwrap().children.len(), 1);
    }

    #[test]
    fn test_expand_collapse() {
        let tree = Arc::new(RwLock::new(MemTree::new()));
        let mut console = MemTreeConsole::new(tree);

        let user_id = console.add_user_message("Test".to_string()).unwrap();
        let response_id = console.add_assistant_response(user_id, "Response".to_string()).unwrap();

        // Initially expanded
        assert!(console.nodes.get(&user_id).unwrap().expanded);

        // Toggle
        console.toggle_expand(user_id);
        assert!(!console.nodes.get(&user_id).unwrap().expanded);

        // When collapsed, children shouldn't be visible
        let visible = console.get_visible_nodes();
        assert_eq!(visible.len(), 1); // Only root
        assert_eq!(visible[0], user_id);
    }
}
