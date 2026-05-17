// Graph model. Real types land in stories 01-03.

#![allow(missing_docs)] // re-enabled when model stabilises

/// Unique identifier for a service crate.
pub type ServiceId = String;

/// A resolved NATS subject pattern, possibly carrying `*` / `>` / `?` markers.
pub type SubjectPattern = String;

/// A node in the event-flow graph.
#[derive(Debug, Clone)]
pub enum Node {
    Service(ServiceId),
    Subject(SubjectPattern),
    Ingress(String),
    Egress(String),
}

/// A directed edge between two nodes.
#[derive(Debug, Clone)]
pub struct Edge {
    pub from: Node,
    pub to: Node,
    pub kind: EdgeKind,
    pub label: Option<String>,
}

/// Edge classification — drives Mermaid line styling.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EdgeKind {
    Publish,
    Consume,
    Ingress,
    Egress,
}

/// The full event-flow graph.
#[derive(Debug, Clone, Default)]
pub struct Graph {
    pub nodes: Vec<Node>,
    pub edges: Vec<Edge>,
}
