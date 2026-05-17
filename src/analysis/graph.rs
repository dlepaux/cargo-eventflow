//! Graph build — combine resolved call sites + symbol index +
//! config-declared ingress/egress into the [`crate::model::Graph`]
//! that the emit layer renders.

use std::collections::{BTreeMap, BTreeSet};

use crate::model::{Edge, EdgeKind, Graph, Node, ServiceId, SubjectPattern};

use super::callsite::{CallKind, RawCallSite};
use super::subject::{ResolveOutcome, Scope};
use super::symbol_index::SymbolIndex;

/// One declared ingress (data source feeding the system).
#[derive(Debug, Clone, serde::Deserialize)]
pub struct Ingress {
    /// Display name (e.g. "Binance WebSocket").
    pub name: String,
    /// Subject patterns this source feeds.
    pub into: Vec<String>,
    /// Owning service crate (for layout grouping).
    #[serde(default, rename = "crate")]
    pub crate_name: Option<String>,
}

/// One declared egress (data sink fed by the system).
#[derive(Debug, Clone, serde::Deserialize)]
pub struct Egress {
    /// Display name (e.g. "Binance REST").
    pub name: String,
    /// Service crate that emits to this sink.
    pub from_crate: String,
    /// Subject patterns whose consumption triggers this fan-out.
    pub triggered_by: Vec<String>,
}

/// Input bundle passed to [`build_graph`].
#[derive(Debug, Clone, Default)]
pub struct GraphInputs {
    /// Resolved call sites with `(crate_name, RawCallSite)`.
    pub call_sites: Vec<(String, RawCallSite)>,
    /// Ingress declarations from config.
    pub ingress: Vec<Ingress>,
    /// Egress declarations from config.
    pub egress: Vec<Egress>,
    /// Helper crates for the resolver.
    pub helper_crates: Vec<String>,
    /// Subject builder methods for the resolver.
    pub subject_builder_methods: Vec<String>,
    /// Subject builder functions for the resolver.
    pub subject_builder_functions: Vec<String>,
}

/// Diagnostic surfaced by graph build.
#[derive(Debug, Clone)]
pub enum GraphDiagnostic {
    /// A call site's subject couldn't be resolved.
    UnresolvedSubject {
        /// Service crate the call lives in.
        crate_name: String,
        /// Where in the source.
        location: String,
        /// Best-effort reason.
        reason: String,
    },
}

/// Run the resolver per call site, group by resolved subject,
/// and produce a [`Graph`] with publish/consume/ingress/egress
/// edges + per-call-site diagnostics.
///
/// All edges sorted deterministically by
/// `(from.id, to.id, kind, label)` so emit is byte-stable across
/// runs (synthesis §P0-H invariant 1).
#[must_use]
pub fn build_graph(inputs: &GraphInputs, index: &SymbolIndex) -> (Graph, Vec<GraphDiagnostic>) {
    let mut diagnostics = Vec::new();
    let resolved = resolve_all(inputs, index, &mut diagnostics);
    let (services, subjects, ingress_names, egress_names) = collect_node_sets(&resolved, inputs);
    let edges = collect_edges(&resolved, inputs);
    let nodes = materialise_nodes(&services, &subjects, &ingress_names, &egress_names);
    let edge_list = materialise_edges(edges, &services, &subjects, &ingress_names, &egress_names);
    (
        Graph {
            nodes,
            edges: edge_list,
        },
        diagnostics,
    )
}

fn resolve_all(
    inputs: &GraphInputs,
    index: &SymbolIndex,
    diagnostics: &mut Vec<GraphDiagnostic>,
) -> Vec<ResolvedSite> {
    let mut resolved: Vec<ResolvedSite> = Vec::with_capacity(inputs.call_sites.len());
    for (crate_name, site) in &inputs.call_sites {
        let mut scope = Scope::new(
            crate_name,
            &inputs.helper_crates,
            &inputs.subject_builder_methods,
            &inputs.subject_builder_functions,
        );
        scope.fn_body_source = site.enclosing_fn_body.as_deref();
        let outcome = super::subject::resolve(&site.subject_expr, index, &scope);
        let subject = match &outcome {
            ResolveOutcome::Resolved(s) | ResolveOutcome::PartiallyResolved(s) => s.clone(),
            ResolveOutcome::Unresolved(reason) => {
                diagnostics.push(GraphDiagnostic::UnresolvedSubject {
                    crate_name: crate_name.clone(),
                    location: format!("{}:{}", site.file.display(), site.line),
                    reason: format!("{reason:?}"),
                });
                "?".to_string()
            }
        };
        resolved.push(ResolvedSite {
            crate_name: crate_name.clone(),
            subject,
            kind: site.kind,
            consumer_name: site.consumer_name_expr.as_deref().map(strip_quotes),
        });
    }
    resolved
}

#[allow(clippy::type_complexity)]
fn collect_node_sets(
    resolved: &[ResolvedSite],
    inputs: &GraphInputs,
) -> (
    BTreeSet<ServiceId>,
    BTreeSet<SubjectPattern>,
    BTreeSet<String>,
    BTreeSet<String>,
) {
    let mut services: BTreeSet<ServiceId> = BTreeSet::new();
    let mut subjects: BTreeSet<SubjectPattern> = BTreeSet::new();
    let mut ingress_names: BTreeSet<String> = BTreeSet::new();
    let mut egress_names: BTreeSet<String> = BTreeSet::new();

    for site in resolved {
        services.insert(site.crate_name.clone());
        subjects.insert(site.subject.clone());
    }
    for ing in &inputs.ingress {
        ingress_names.insert(ing.name.clone());
        for s in &ing.into {
            subjects.insert(s.clone());
        }
        if let Some(c) = &ing.crate_name {
            services.insert(c.clone());
        }
    }
    for eg in &inputs.egress {
        egress_names.insert(eg.name.clone());
        services.insert(eg.from_crate.clone());
        for s in &eg.triggered_by {
            subjects.insert(s.clone());
        }
    }
    (services, subjects, ingress_names, egress_names)
}

fn collect_edges(
    resolved: &[ResolvedSite],
    inputs: &GraphInputs,
) -> BTreeMap<EdgeKey, (EdgeKind, Option<String>)> {
    let mut edges: BTreeMap<EdgeKey, (EdgeKind, Option<String>)> = BTreeMap::new();
    for site in resolved {
        match site.kind {
            CallKind::Publish => {
                edges.insert(
                    EdgeKey {
                        from: Node::Service(site.crate_name.clone()).id(),
                        to: Node::Subject(site.subject.clone()).id(),
                        kind: EdgeKind::Publish,
                    },
                    (EdgeKind::Publish, None),
                );
            }
            CallKind::Subscribe => {
                edges.insert(
                    EdgeKey {
                        from: Node::Subject(site.subject.clone()).id(),
                        to: Node::Service(site.crate_name.clone()).id(),
                        kind: EdgeKind::Consume,
                    },
                    (EdgeKind::Consume, site.consumer_name.clone()),
                );
            }
        }
    }
    for ing in &inputs.ingress {
        for subject in &ing.into {
            edges.insert(
                EdgeKey {
                    from: Node::Ingress(ing.name.clone()).id(),
                    to: Node::Subject(subject.clone()).id(),
                    kind: EdgeKind::Ingress,
                },
                (EdgeKind::Ingress, None),
            );
        }
    }
    for eg in &inputs.egress {
        for subject in &eg.triggered_by {
            edges.insert(
                EdgeKey {
                    from: Node::Subject(subject.clone()).id(),
                    to: Node::Egress(eg.name.clone()).id(),
                    kind: EdgeKind::Egress,
                },
                (EdgeKind::Egress, None),
            );
        }
    }
    edges
}

fn materialise_nodes(
    services: &BTreeSet<ServiceId>,
    subjects: &BTreeSet<SubjectPattern>,
    ingress: &BTreeSet<String>,
    egress: &BTreeSet<String>,
) -> Vec<Node> {
    let mut nodes: Vec<Node> = Vec::new();
    for s in services {
        nodes.push(Node::Service(s.clone()));
    }
    for s in subjects {
        nodes.push(Node::Subject(s.clone()));
    }
    for s in ingress {
        nodes.push(Node::Ingress(s.clone()));
    }
    for s in egress {
        nodes.push(Node::Egress(s.clone()));
    }
    nodes
}

fn materialise_edges(
    edges: BTreeMap<EdgeKey, (EdgeKind, Option<String>)>,
    services: &BTreeSet<String>,
    subjects: &BTreeSet<String>,
    ingress: &BTreeSet<String>,
    egress: &BTreeSet<String>,
) -> Vec<Edge> {
    let mut edge_list: Vec<Edge> = edges
        .into_iter()
        .map(|(key, (kind, label))| Edge {
            from: node_from_id(&key.from, services, subjects, ingress, egress),
            to: node_from_id(&key.to, services, subjects, ingress, egress),
            kind,
            label,
        })
        .collect();
    edge_list.sort_by(|a, b| {
        a.from
            .id()
            .cmp(&b.from.id())
            .then_with(|| a.to.id().cmp(&b.to.id()))
            .then_with(|| (a.kind as u8).cmp(&(b.kind as u8)))
            .then_with(|| a.label.cmp(&b.label))
    });
    edge_list
}

struct ResolvedSite {
    crate_name: String,
    subject: String,
    kind: CallKind,
    consumer_name: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct EdgeKey {
    from: String,
    to: String,
    kind: EdgeKind,
}

fn node_from_id(
    id: &str,
    services: &BTreeSet<String>,
    subjects: &BTreeSet<String>,
    ingress: &BTreeSet<String>,
    egress: &BTreeSet<String>,
) -> Node {
    // The id prefix from Node::id() disambiguates which set to query.
    if let Some(rest) = id.strip_prefix("svc:") {
        if let Some(s) = services.get(rest) {
            return Node::Service(s.clone());
        }
        return Node::Service(rest.to_string());
    }
    if let Some(rest) = id.strip_prefix("sub:") {
        if let Some(s) = subjects.get(rest) {
            return Node::Subject(s.clone());
        }
        return Node::Subject(rest.to_string());
    }
    if let Some(rest) = id.strip_prefix("ing:") {
        if let Some(s) = ingress.get(rest) {
            return Node::Ingress(s.clone());
        }
        return Node::Ingress(rest.to_string());
    }
    if let Some(rest) = id.strip_prefix("eg:") {
        if let Some(s) = egress.get(rest) {
            return Node::Egress(s.clone());
        }
        return Node::Egress(rest.to_string());
    }
    Node::Subject(id.to_string())
}

fn strip_quotes(s: &str) -> String {
    let s = s.trim();
    s.trim_start_matches('&')
        .trim_start_matches('"')
        .trim_end_matches('"')
        .trim()
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::analysis::callsite::CallKind;
    use std::path::PathBuf;

    fn site(crate_name: &str, kind: CallKind, expr: &str) -> (String, RawCallSite) {
        (
            crate_name.to_string(),
            RawCallSite {
                kind,
                file: PathBuf::from(format!("{crate_name}/src/lib.rs")),
                line: 1,
                column: 1,
                subject_expr: expr.to_string(),
                consumer_name_expr: None,
                matched_method: "publish".to_string(),
                enclosing_fn_body: None,
            },
        )
    }

    #[test]
    fn build_graph_from_literal_sites() {
        let inputs = GraphInputs {
            call_sites: vec![
                site("svc-a", CallKind::Publish, "\"foo.bar\""),
                site("svc-b", CallKind::Subscribe, "\"foo.bar\""),
            ],
            ..Default::default()
        };
        let index = SymbolIndex::new();
        let (graph, diags) = build_graph(&inputs, &index);
        assert!(diags.is_empty());
        assert_eq!(graph.edges.len(), 2);
        // 1 publish edge (svc-a → foo.bar), 1 consume edge (foo.bar → svc-b)
        assert!(graph
            .edges
            .iter()
            .any(|e| matches!(e.kind, EdgeKind::Publish)
                && matches!(&e.from, Node::Service(s) if s == "svc-a")
                && matches!(&e.to, Node::Subject(s) if s == "foo.bar")));
        assert!(graph
            .edges
            .iter()
            .any(|e| matches!(e.kind, EdgeKind::Consume)
                && matches!(&e.from, Node::Subject(s) if s == "foo.bar")
                && matches!(&e.to, Node::Service(s) if s == "svc-b")));
    }

    #[test]
    fn ingress_egress_edges() {
        let inputs = GraphInputs {
            call_sites: vec![
                site("data", CallKind::Publish, "\"market.klines\""),
                site("exec", CallKind::Subscribe, "\"intents.executor\""),
            ],
            ingress: vec![Ingress {
                name: "Binance WS".into(),
                into: vec!["market.klines".into()],
                crate_name: Some("data".into()),
            }],
            egress: vec![Egress {
                name: "Binance REST".into(),
                from_crate: "exec".into(),
                triggered_by: vec!["intents.executor".into()],
            }],
            ..Default::default()
        };
        let (graph, _) = build_graph(&inputs, &SymbolIndex::new());
        // 4 edges: 1 ingress, 1 publish, 1 consume, 1 egress
        assert_eq!(graph.edges.len(), 4);
    }

    #[test]
    fn determinism_byte_stable_across_runs() {
        let inputs = GraphInputs {
            call_sites: vec![
                site("z-last", CallKind::Publish, "\"a.b\""),
                site("a-first", CallKind::Publish, "\"a.b\""),
                site("m-mid", CallKind::Subscribe, "\"a.b\""),
            ],
            ..Default::default()
        };
        let index = SymbolIndex::new();
        let (g1, _) = build_graph(&inputs, &index);
        let (g2, _) = build_graph(&inputs, &index);
        let ids1: Vec<_> = g1.edges.iter().map(|e| (e.from.id(), e.to.id())).collect();
        let ids2: Vec<_> = g2.edges.iter().map(|e| (e.from.id(), e.to.id())).collect();
        assert_eq!(ids1, ids2);
    }
}
