pub mod executor;
pub mod layout;
pub mod renderer;

use std::f32::consts::TAU;
use std::collections::{HashMap, VecDeque};

#[derive(Debug, Clone, PartialEq)]
pub enum NodeKind {
    Task,
    Constraint,
    Question,
    Observation,
}

impl NodeKind {
    pub fn symbol(&self, near: bool) -> char {
        if !near { return '·'; }
        match self {
            NodeKind::Task => '●',
            NodeKind::Constraint => '⊗',
            NodeKind::Question => '?',
            NodeKind::Observation => '◎',
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum NodeStatus {
    Pending,
    Running,
    Done,
    Failed,
}

#[derive(Debug, Clone, PartialEq)]
pub enum NodeAuthor {
    User,
    Ai,
}

#[derive(Debug, Clone)]
pub struct Node {
    pub id: usize,
    pub label: String,
    pub kind: NodeKind,
    pub status: NodeStatus,
    pub result: Option<String>,
    pub pos: [f32; 3],
    pub author: NodeAuthor,
    /// Tool names this node is allowed to use during execution (empty = plain generation).
    pub tools: Vec<String>,
    /// Compiled native code for this word (bash, python, etc.).
    /// When set, execution runs this directly without calling the LLM.
    pub compiled_code: Option<String>,
    /// Language of the compiled code: "bash", "python", etc.
    pub compiled_lang: Option<String>,
}

#[derive(Debug, Clone)]
pub struct Poset {
    pub nodes: Vec<Node>,
    pub edges: Vec<(usize, usize)>,  // (predecessor_id, successor_id)
    pub yaw: f32,
    pub pitch: f32,
    next_id: usize,
}

impl Default for Poset {
    fn default() -> Self { Self::new() }
}

impl Poset {
    pub fn new() -> Self {
        Self { nodes: Vec::new(), edges: Vec::new(), yaw: 0.3, pitch: 0.2, next_id: 0 }
    }

    pub fn is_empty(&self) -> bool { self.nodes.is_empty() }

    pub fn add_node(&mut self, label: String, kind: NodeKind, author: NodeAuthor) -> usize {
        let id = self.next_id;
        self.next_id += 1;
        self.nodes.push(Node {
            id, label, kind, status: NodeStatus::Pending,
            result: None, pos: [0.0, 0.0, 0.0], author, tools: Vec::new(),
            compiled_code: None, compiled_lang: None,
        });
        layout::assign_positions(self);
        id
    }

    /// Add a node that has specific tools available during execution.
    pub fn add_node_with_tools(&mut self, label: String, kind: NodeKind, author: NodeAuthor, tools: Vec<String>) -> usize {
        let id = self.add_node(label, kind, author);
        if let Some(n) = self.node_mut(id) { n.tools = tools; }
        id
    }

    pub fn add_edge(&mut self, before: usize, after: usize) {
        if !self.edges.contains(&(before, after)) && before != after {
            self.edges.push((before, after));
            layout::assign_positions(self);
        }
    }

    pub fn predecessors(&self, id: usize) -> Vec<usize> {
        self.edges.iter().filter(|(_, b)| *b == id).map(|(a, _)| *a).collect()
    }

    pub fn successors(&self, id: usize) -> Vec<usize> {
        self.edges.iter().filter(|(a, _)| *a == id).map(|(_, b)| *b).collect()
    }

    pub fn ready_nodes(&self) -> Vec<usize> {
        self.nodes.iter()
            .filter(|n| n.status == NodeStatus::Pending)
            .filter(|n| self.predecessors(n.id).iter().all(|&pid| {
                self.nodes.iter().find(|m| m.id == pid)
                    .map(|m| m.status == NodeStatus::Done)
                    .unwrap_or(true)
            }))
            .map(|n| n.id)
            .collect()
    }

    pub fn is_complete(&self) -> bool {
        self.nodes.iter().all(|n| matches!(n.status, NodeStatus::Done | NodeStatus::Failed))
    }

    pub fn rotate(&mut self, dyaw: f32, dpitch: f32) {
        self.yaw = (self.yaw + dyaw) % TAU;
        self.pitch = (self.pitch + dpitch).clamp(-1.3, 1.3);
    }

    pub fn node_mut(&mut self, id: usize) -> Option<&mut Node> {
        self.nodes.iter_mut().find(|n| n.id == id)
    }

    pub fn node(&self, id: usize) -> Option<&Node> {
        self.nodes.iter().find(|n| n.id == id)
    }

    pub fn pop(&mut self) -> Option<Node> {
        if let Some(node) = self.nodes.pop() {
            self.edges.retain(|(a, b)| *a != node.id && *b != node.id);
            layout::assign_positions(self);
            Some(node)
        } else {
            None
        }
    }

    pub fn clear(&mut self) {
        self.nodes.clear();
        self.edges.clear();
    }

    pub fn topological_order(&self) -> Vec<usize> {
        let mut in_degree: HashMap<usize, usize> = HashMap::new();
        for n in &self.nodes { in_degree.entry(n.id).or_insert(0); }
        for &(_, b) in &self.edges { *in_degree.entry(b).or_insert(0) += 1; }
        let mut queue: VecDeque<usize> = in_degree.iter()
            .filter(|(_, &d)| d == 0).map(|(&id, _)| id).collect();
        let mut order = Vec::new();
        while let Some(id) = queue.pop_front() {
            order.push(id);
            for succ in self.successors(id) {
                let d = in_degree.entry(succ).or_insert(1);
                *d -= 1;
                if *d == 0 { queue.push_back(succ); }
            }
        }
        order
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_poset_add_node() {
        let mut p = Poset::new();
        assert!(p.is_empty());
        let id = p.add_node("task1".to_string(), NodeKind::Task, NodeAuthor::User);
        assert_eq!(id, 0);
        assert!(!p.is_empty());
        assert_eq!(p.nodes.len(), 1);
    }

    #[test]
    fn test_poset_edges_and_ready() {
        let mut p = Poset::new();
        let a = p.add_node("A".to_string(), NodeKind::Task, NodeAuthor::User);
        let b = p.add_node("B".to_string(), NodeKind::Task, NodeAuthor::Ai);
        p.add_edge(a, b);
        // B has predecessor A which is Pending, so only A is ready
        let ready = p.ready_nodes();
        assert_eq!(ready, vec![a]);
    }

    #[test]
    fn test_poset_topological_order() {
        let mut p = Poset::new();
        let a = p.add_node("A".to_string(), NodeKind::Task, NodeAuthor::User);
        let b = p.add_node("B".to_string(), NodeKind::Task, NodeAuthor::Ai);
        p.add_edge(a, b);
        let order = p.topological_order();
        assert_eq!(order[0], a);
        assert_eq!(order[1], b);
    }

    #[test]
    fn test_poset_rotate() {
        let mut p = Poset::new();
        p.rotate(0.5, 0.1);
        assert!((p.yaw - 0.8).abs() < 1e-5);
        assert!((p.pitch - 0.3).abs() < 1e-5);
    }
}
