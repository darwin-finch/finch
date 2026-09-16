//! The view model for the live graph panel.
//!
//! The renderer draws nodes and edges. It does not know what produces them — a Co-Forth poset,
//! a plan, or anything a future embedder invents — because a terminal framework that holds
//! Finch's `Poset` cannot be used by anything that has no poset.
//!
//! Callers translate their own types into [`GraphView`] at the injection boundary, which is the
//! only place the two vocabularies meet.

/// What a graph node is for. The renderer chooses a glyph from this and nothing else.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GraphNodeKind {
    /// A unit of work.
    Task,
    /// A restriction the graph records, not a runnable step.
    Constraint,
    /// An unanswered question.
    Question,
    /// A note, not work.
    Observation,
}

/// How far along a node's work is. The renderer chooses colour from this and nothing else.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GraphNodeStatus {
    /// Not started.
    Pending,
    /// In flight.
    Running,
    /// Finished successfully.
    Done,
    /// Finished unsuccessfully.
    Failed,
}

/// Who created the node. Display-only; the renderer does not assign work.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GraphNodeAuthor {
    /// The person at the terminal.
    User,
    /// The assistant.
    Ai,
}

/// One node of the live graph panel.
#[derive(Debug, Clone, PartialEq)]
pub struct GraphNode {
    /// Stable identity used by edges.
    pub id: usize,
    /// The row's own text, truncated by the renderer to fit the panel.
    pub label: String,
    pub kind: GraphNodeKind,
    pub status: GraphNodeStatus,
    /// Layout position in the panel's 3-space. The renderer projects it; it does not layout.
    pub pos: [f32; 3],
    pub author: GraphNodeAuthor,
}

impl GraphNode {
    /// A pending user task at the origin. Tests and callers fill in the rest.
    pub fn new(id: usize, label: impl Into<String>) -> Self {
        Self {
            id,
            label: label.into(),
            kind: GraphNodeKind::Task,
            status: GraphNodeStatus::Pending,
            pos: [0.0, 0.0, 0.0],
            author: GraphNodeAuthor::User,
        }
    }
}

/// Nodes and edges the graph widget can draw, plus the camera the overlay uses.
#[derive(Debug, Clone, PartialEq)]
pub struct GraphView {
    pub nodes: Vec<GraphNode>,
    /// `(predecessor_id, successor_id)` pairs. Unknown ids are omitted at render time.
    pub edges: Vec<(usize, usize)>,
    /// Horizontal camera angle in radians.
    pub yaw: f32,
    /// Vertical camera angle in radians.
    pub pitch: f32,
}

impl Default for GraphView {
    fn default() -> Self {
        Self::new()
    }
}

impl GraphView {
    /// An empty graph with the default camera.
    pub fn new() -> Self {
        Self {
            nodes: Vec::new(),
            edges: Vec::new(),
            yaw: 0.3,
            pitch: 0.2,
        }
    }

    /// True when there is nothing to draw.
    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_a_graph_node_defaults_to_a_pending_user_task_at_the_origin() {
        let node = GraphNode::new(3, "compile");
        assert_eq!(node.id, 3);
        assert_eq!(node.label, "compile");
        assert_eq!(node.kind, GraphNodeKind::Task);
        assert_eq!(node.status, GraphNodeStatus::Pending);
        assert_eq!(node.pos, [0.0, 0.0, 0.0]);
        assert_eq!(node.author, GraphNodeAuthor::User);
    }
}
