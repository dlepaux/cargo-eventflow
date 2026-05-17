//! Internal graph model.
//!
//! `Graph { nodes, edges }` is what the analysis pipeline emits
//! and the emit layer consumes. All lists are pre-sorted at build
//! time so emit is byte-stable across runs
//! (synthesis §P0-H invariant 1).

#![allow(missing_docs)]

pub mod pattern;

pub use pattern::{NatsPattern, ParseError as PatternParseError, Segment};

/// Unique identifier for a service crate.
pub type ServiceId = String;

/// A resolved NATS subject pattern. Dynamic segments render as
/// `*` (single segment) or `>` (tail). Unresolvable segments
/// render as `?`.
///
/// Today still a `String` for backwards compat with the analysis
/// pipeline. P1 commit 3 flips this to [`NatsPattern`] in the same
/// patch that threads the type through `Node::Subject` and
/// `collect_node_sets`. This commit (P1 commit 2) ships the algebra
/// module standalone — no behaviour change here.
pub type SubjectPattern = String;

/// A node in the event-flow graph.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum Node {
    /// A service crate that publishes or consumes.
    Service(ServiceId),
    /// A NATS subject pattern.
    Subject(SubjectPattern),
    /// An external data source feeding the system.
    Ingress(String),
    /// An external sink fed by the system.
    Egress(String),
}

impl Node {
    /// Render-stable identifier for this node. Used by emit to
    /// produce unique Mermaid node ids.
    #[must_use]
    pub fn id(&self) -> String {
        match self {
            Self::Service(s) => format!("svc:{s}"),
            Self::Subject(s) => format!("sub:{s}"),
            Self::Ingress(s) => format!("ing:{s}"),
            Self::Egress(s) => format!("eg:{s}"),
        }
    }

    /// Human-readable label.
    #[must_use]
    pub fn label(&self) -> &str {
        match self {
            Self::Service(s) | Self::Subject(s) | Self::Ingress(s) | Self::Egress(s) => s,
        }
    }
}

/// A directed edge between two nodes.
#[derive(Debug, Clone)]
pub struct Edge {
    /// Source node.
    pub from: Node,
    /// Target node.
    pub to: Node,
    /// Edge category — drives line styling.
    pub kind: EdgeKind,
    /// Optional label (e.g. durable consumer name).
    pub label: Option<String>,
}

/// Edge classification — drives Mermaid line styling.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum EdgeKind {
    Publish,
    Consume,
    Ingress,
    Egress,
}

/// The full event-flow graph.
#[derive(Debug, Clone, Default)]
pub struct Graph {
    /// Nodes sorted by `(variant_discriminator, label)`.
    pub nodes: Vec<Node>,
    /// Edges sorted by `(from.id, to.id, kind, label)`.
    pub edges: Vec<Edge>,
}
