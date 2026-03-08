/// Execution graph — records the causal trace of one query turn.
///
/// Each query produces a directed sequence of nodes:
///   UserInput → LlmCall → ToolExecution* → LlmCall → … → FinalResponse
///
/// The graph is persisted to `~/.finch/graphs/` as JSON and can be
/// inspected interactively with `/graph`.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

// ── Node types ────────────────────────────────────────────────────────────────

/// A single step in the execution trace.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum NodeKind {
    UserInput {
        text: String,
    },
    LlmCall {
        model: String,
        input_tokens: Option<u32>,
        output_tokens: Option<u32>,
    },
    ToolExecution {
        name: String,
        /// First 120 chars of the serialised input JSON.
        input_preview: String,
        /// First 200 chars of the tool output (or error message).
        output_preview: String,
        is_error: bool,
    },
    FinalResponse {
        /// First 300 chars of the response text.
        preview: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GraphNode {
    pub id: u32,
    #[serde(flatten)]
    pub kind: NodeKind,
    pub timestamp: DateTime<Utc>,
}

// ── Graph ─────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutionGraph {
    pub query_id: Option<Uuid>,
    pub session_label: String,
    pub nodes: Vec<GraphNode>,
    #[serde(skip)]
    next_id: u32,
}

impl Default for ExecutionGraph {
    fn default() -> Self {
        Self {
            query_id: None,
            session_label: String::new(),
            nodes: Vec::new(),
            next_id: 0,
        }
    }
}

impl ExecutionGraph {
    pub fn new() -> Self {
        Self::default()
    }

    /// Reset for a new query.
    pub fn reset(&mut self, query_id: Uuid, session_label: &str) {
        self.query_id = Some(query_id);
        self.session_label = session_label.to_string();
        self.nodes.clear();
        self.next_id = 0;
    }

    /// Add a node and return its id.
    pub fn add_node(&mut self, kind: NodeKind) -> u32 {
        let id = self.next_id;
        self.next_id += 1;
        self.nodes.push(GraphNode {
            id,
            kind,
            timestamp: Utc::now(),
        });
        id
    }

    /// True when there are no nodes yet.
    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }

    // ── Persistence ───────────────────────────────────────────────────────────

    /// Save the graph to `~/.finch/graphs/<session>-<query_id_short>.json`.
    /// Silently ignored if `query_id` is None or home dir cannot be determined.
    pub fn save(&self) -> anyhow::Result<()> {
        let query_id = match self.query_id {
            Some(id) => id,
            None => return Ok(()),
        };
        let home = match dirs::home_dir() {
            Some(h) => h,
            None => return Ok(()),
        };
        let graphs_dir = home.join(".finch").join("graphs");
        std::fs::create_dir_all(&graphs_dir)?;

        let short_id = &query_id.to_string()[..8];
        let filename = format!("{}-{}.json", self.session_label, short_id);
        let path = graphs_dir.join(filename);

        let json = serde_json::to_string_pretty(self)?;
        std::fs::write(path, json)?;
        Ok(())
    }

    // ── Display ───────────────────────────────────────────────────────────────

    /// Render the graph as a human-readable text trace.
    pub fn format_display(&self) -> String {
        use std::fmt::Write as _;

        let mut out = String::new();

        // Header
        let query_id_str = self
            .query_id
            .map(|id| id.to_string()[..8].to_string())
            .unwrap_or_else(|| "?".to_string());

        let _ = writeln!(
            &mut out,
            "╔══════════════════════════════════════════════════════════╗"
        );
        let _ = writeln!(
            &mut out,
            "║  Execution Graph  ·  {}  ·  {}  ║",
            query_id_str,
            self.session_label
        );
        let _ = writeln!(
            &mut out,
            "╚══════════════════════════════════════════════════════════╝"
        );

        if self.nodes.is_empty() {
            let _ = writeln!(&mut out, "\n  (no nodes recorded)");
            return out;
        }

        for (i, node) in self.nodes.iter().enumerate() {
            if i > 0 {
                let _ = writeln!(&mut out, "         │");
                let _ = writeln!(&mut out, "         ▼");
            }
            match &node.kind {
                NodeKind::UserInput { text } => {
                    let _ = writeln!(&mut out, "  [{}] 👤 User", node.id + 1);
                    let _ = writeln!(&mut out, "      \"{}\"", truncate(text, 200));
                }
                NodeKind::LlmCall {
                    model,
                    input_tokens,
                    output_tokens,
                } => {
                    let tokens = match (input_tokens, output_tokens) {
                        (Some(i), Some(o)) => format!("  ({} in → {} out tokens)", i, o),
                        (Some(i), None) => format!("  ({} in tokens)", i),
                        _ => String::new(),
                    };
                    let _ = writeln!(&mut out, "  [{}] 🤖 LLM · {}{}", node.id + 1, model, tokens);
                }
                NodeKind::ToolExecution {
                    name,
                    input_preview,
                    output_preview,
                    is_error,
                } => {
                    let _ = writeln!(&mut out, "  [{}] ⚙  Tool: {}", node.id + 1, name);
                    if !input_preview.is_empty() {
                        let _ = writeln!(&mut out, "      input:  {}", input_preview);
                    }
                    let icon = if *is_error { "✗" } else { "✓" };
                    let _ = writeln!(&mut out, "      {}  {}", icon, truncate(output_preview, 200));
                }
                NodeKind::FinalResponse { preview } => {
                    let _ = writeln!(&mut out, "  [{}] 💬 Response", node.id + 1);
                    let _ = writeln!(&mut out, "      \"{}\"", truncate(preview, 300));
                }
            }
        }

        out
    }
}

fn truncate(s: &str, max: usize) -> &str {
    if s.len() <= max {
        s
    } else {
        // Find a char boundary
        let mut end = max;
        while end > 0 && !s.is_char_boundary(end) {
            end -= 1;
        }
        &s[..end]
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn make_graph() -> ExecutionGraph {
        let mut g = ExecutionGraph::new();
        let id = Uuid::new_v4();
        g.reset(id, "test-session");
        g
    }

    #[test]
    fn test_new_graph_is_empty() {
        let g = make_graph();
        assert!(g.is_empty());
        assert_eq!(g.nodes.len(), 0);
    }

    #[test]
    fn test_add_nodes_increments_ids() {
        let mut g = make_graph();
        let id0 = g.add_node(NodeKind::UserInput { text: "hello".into() });
        let id1 = g.add_node(NodeKind::LlmCall {
            model: "claude".into(),
            input_tokens: None,
            output_tokens: None,
        });
        assert_eq!(id0, 0);
        assert_eq!(id1, 1);
        assert_eq!(g.nodes.len(), 2);
    }

    #[test]
    fn test_reset_clears_nodes() {
        let mut g = make_graph();
        g.add_node(NodeKind::UserInput { text: "hi".into() });
        assert!(!g.is_empty());

        let id2 = Uuid::new_v4();
        g.reset(id2, "new-session");
        assert!(g.is_empty());
        assert_eq!(g.session_label, "new-session");
    }

    #[test]
    fn test_format_display_non_empty() {
        let mut g = make_graph();
        g.add_node(NodeKind::UserInput { text: "list files".into() });
        g.add_node(NodeKind::LlmCall {
            model: "claude-sonnet-4-6".into(),
            input_tokens: Some(100),
            output_tokens: Some(50),
        });
        g.add_node(NodeKind::ToolExecution {
            name: "glob".into(),
            input_preview: r#"{"pattern":"**/*.rs"}"#.into(),
            output_preview: "42 matches".into(),
            is_error: false,
        });
        g.add_node(NodeKind::FinalResponse {
            preview: "Found 42 Rust files.".into(),
        });

        let display = g.format_display();
        assert!(display.contains("User"));
        assert!(display.contains("list files"));
        assert!(display.contains("LLM"));
        assert!(display.contains("glob"));
        assert!(display.contains("42 matches"));
        assert!(display.contains("Response"));
    }

    #[test]
    fn test_serialization_roundtrip() {
        let mut g = make_graph();
        g.add_node(NodeKind::ToolExecution {
            name: "read".into(),
            input_preview: r#"{"file_path":"/foo"}"#.into(),
            output_preview: "line1\nline2".into(),
            is_error: false,
        });

        let json = serde_json::to_string(&g).unwrap();
        let g2: ExecutionGraph = serde_json::from_str(&json).unwrap();
        assert_eq!(g2.nodes.len(), 1);
    }

    #[test]
    fn test_truncate_short_strings_unchanged() {
        assert_eq!(truncate("hello", 100), "hello");
    }

    #[test]
    fn test_truncate_long_strings() {
        let s = "a".repeat(300);
        let t = truncate(&s, 100);
        assert_eq!(t.len(), 100);
    }
}
