//! Graph build -- combine resolved call sites + symbol index +
//! config-declared ingress/egress into the [`crate::model::Graph`]
//! that the emit layer renders.

mod matches;

#[cfg(test)]
mod tests;

use std::collections::{BTreeMap, BTreeSet};

use crate::model::{Edge, EdgeKind, Graph, NatsPattern, Node, ServiceId};

use super::callsite::{CallKind, RawCallSite};
use super::subject::{ResolveOutcome, Scope, UnresolvedReason};
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
    /// A subscribe call site's durable consumer-name argument
    /// couldn't be resolved. The diagram still renders (the edge
    /// label falls back to `?`); this signals an opportunity to
    /// add a `// eventflow:` annotation or fix the resolver
    /// config.
    UnresolvedConsumerName {
        /// Service crate the subscribe call lives in.
        crate_name: String,
        /// Where in the source.
        location: String,
        /// Best-effort reason.
        reason: String,
    },
    /// A subject string (from resolver, ingress, or egress) failed
    /// `NatsPattern::parse`. The subject is replaced with the `?`
    /// fallback so the diagram still renders; check the source for
    /// illegal characters / non-ASCII / mid-pattern `>`.
    MalformedSubject {
        /// Where the subject came from: `"resolver"`, `"ingress:<name>"`,
        /// or `"egress:<name>"`.
        origin: String,
        /// The raw subject string that failed parse.
        raw: String,
        /// The parse error message.
        reason: String,
    },
    /// A declared ingress pattern has no observed publisher
    /// (no `Publish` call site in code overlaps with the declared
    /// pattern). Either the config is stale or the publisher hasn't
    /// been wired yet -- manual review needed.
    OrphanIngress {
        /// Ingress display name (e.g. `"Binance WebSocket"`).
        ingress_name: String,
        /// The declared pattern that found no overlap with any
        /// observed publisher.
        pattern: String,
    },
    /// A declared egress pattern has no observed consumer
    /// (no `Subscribe` call site in code overlaps with the
    /// declared `triggered_by` pattern). Stale config or
    /// not-yet-wired sink.
    OrphanEgress {
        /// Egress display name.
        egress_name: String,
        /// The declared trigger pattern that found no overlap.
        pattern: String,
    },
}

impl std::fmt::Display for GraphDiagnostic {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnresolvedSubject {
                crate_name,
                location,
                reason,
            } => write!(
                f,
                "unresolved subject in {crate_name} at {location}: {reason}"
            ),
            Self::UnresolvedConsumerName {
                crate_name,
                location,
                reason,
            } => write!(
                f,
                "unresolved consumer name in {crate_name} at {location}: {reason}"
            ),
            Self::MalformedSubject {
                origin,
                raw,
                reason,
            } => write!(f, "malformed subject from {origin}: {raw:?} ({reason})"),
            Self::OrphanIngress {
                ingress_name,
                pattern,
            } => write!(
                f,
                "orphan ingress {ingress_name:?}: declared pattern {pattern:?} has no observed consumer in code (data flows in but nothing subscribes)"
            ),
            Self::OrphanEgress {
                egress_name,
                pattern,
            } => write!(
                f,
                "orphan egress {egress_name:?}: declared trigger pattern {pattern:?} has no observed publisher in code (sink declared but nothing fires)"
            ),
        }
    }
}

/// Run the resolver per call site, group by resolved subject,
/// and produce a [`Graph`] with publish/consume/ingress/egress
/// edges + per-call-site diagnostics.
///
/// All edges sorted deterministically by
/// `(from.id, to.id, kind, label)` so emit is byte-stable across
/// runs (synthesis P0-H invariant 1).
#[must_use]
pub fn build_graph(inputs: &GraphInputs, index: &SymbolIndex) -> (Graph, Vec<GraphDiagnostic>) {
    let mut diagnostics = Vec::new();
    let resolved = resolve_all(inputs, index, &mut diagnostics);
    let (services, subjects, ingress_names, egress_names) =
        collect_node_sets(&resolved, inputs, &mut diagnostics);
    let mut edges = collect_edges(&resolved, inputs);
    matches::add_matches_edges(&mut edges, &resolved, inputs);
    detect_orphan_ingress_egress(&resolved, inputs, &mut diagnostics);
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
        let (subject_str, subject_diag) =
            resolve_call_site_expr(&site.subject_expr, site, crate_name, inputs, index);
        if let Some(reason) = subject_diag {
            diagnostics.push(GraphDiagnostic::UnresolvedSubject {
                crate_name: crate_name.clone(),
                location: format!("{}:{}", site.file.display(), site.line),
                reason: format!("{reason:?}"),
            });
        }
        let subject = parse_subject_or_fallback(&subject_str, "resolver", diagnostics);
        let consumer_name = site.consumer_name_expr.as_deref().map(|expr| {
            let (rendered, diag) = resolve_call_site_expr(expr, site, crate_name, inputs, index);
            if let Some(reason) = diag {
                diagnostics.push(GraphDiagnostic::UnresolvedConsumerName {
                    crate_name: crate_name.clone(),
                    location: format!("{}:{}", site.file.display(), site.line),
                    reason: format!("{reason:?}"),
                });
            }
            rendered
        });
        resolved.push(ResolvedSite {
            crate_name: crate_name.clone(),
            subject,
            kind: site.kind,
            consumer_name,
        });
    }
    resolved
}

/// Run the subject resolver against any string-valued runtime
/// expression captured at a call site. Used for both
/// `subject_expr` and `consumer_name_expr` -- both are NATS call
/// arguments resolved against the same scope.
///
/// Returns the rendered pattern (`*` / `>` for dynamic segments,
/// `?` for fully unresolvable) plus an optional reason when
/// resolution gave up so the caller can emit a structured
/// diagnostic.
fn resolve_call_site_expr(
    snippet: &str,
    site: &RawCallSite,
    crate_name: &str,
    inputs: &GraphInputs,
    index: &SymbolIndex,
) -> (String, Option<UnresolvedReason>) {
    let mut scope = Scope::new(
        crate_name,
        &inputs.helper_crates,
        &inputs.subject_builder_methods,
        &inputs.subject_builder_functions,
    );
    scope.fn_body_source = site.enclosing_fn_body.as_deref();
    match super::subject::resolve(snippet, index, &scope) {
        ResolveOutcome::Resolved(s) | ResolveOutcome::PartiallyResolved(s) => (s, None),
        ResolveOutcome::Unresolved(reason) => ("?".to_string(), Some(reason)),
    }
}

/// Try to parse `raw` as a [`NatsPattern`]. On failure, push a
/// `GraphDiagnostic::MalformedSubject` and fall back to the `?`
/// sentinel pattern so the diagram still renders. `origin` is a
/// human-readable label (`"resolver"`, `"ingress:<name>"`,
/// `"egress:<name>"`) for the diagnostic message.
fn parse_subject_or_fallback(
    raw: &str,
    origin: &str,
    diagnostics: &mut Vec<GraphDiagnostic>,
) -> NatsPattern {
    match NatsPattern::parse(raw) {
        Ok(p) => p,
        Err(err) => {
            diagnostics.push(GraphDiagnostic::MalformedSubject {
                origin: origin.to_string(),
                raw: raw.to_string(),
                reason: err.to_string(),
            });
            // `?` is a single legal literal token under the strict
            // parser; safe fallback that produces a visible sentinel
            // in the rendered diagram.
            NatsPattern::parse("?").expect("? is a legal literal token")
        }
    }
}

#[allow(clippy::type_complexity)] // allow: graph builder returns nested map of maps
fn collect_node_sets(
    resolved: &[ResolvedSite],
    inputs: &GraphInputs,
    diagnostics: &mut Vec<GraphDiagnostic>,
) -> (
    BTreeSet<ServiceId>,
    BTreeSet<NatsPattern>,
    BTreeSet<String>,
    BTreeSet<String>,
) {
    let mut services: BTreeSet<ServiceId> = BTreeSet::new();
    let mut subjects: BTreeSet<NatsPattern> = BTreeSet::new();
    let mut ingress_names: BTreeSet<String> = BTreeSet::new();
    let mut egress_names: BTreeSet<String> = BTreeSet::new();

    for site in resolved {
        services.insert(site.crate_name.clone());
        subjects.insert(site.subject.clone());
    }
    for ing in &inputs.ingress {
        ingress_names.insert(ing.name.clone());
        for s in &ing.into {
            let origin = format!("ingress:{}", ing.name);
            subjects.insert(parse_subject_or_fallback(s, &origin, diagnostics));
        }
        if let Some(c) = &ing.crate_name {
            services.insert(c.clone());
        }
    }
    for eg in &inputs.egress {
        egress_names.insert(eg.name.clone());
        services.insert(eg.from_crate.clone());
        for s in &eg.triggered_by {
            let origin = format!("egress:{}", eg.name);
            subjects.insert(parse_subject_or_fallback(s, &origin, diagnostics));
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
    // Ingress/egress subject parsing already produced diagnostics in
    // `collect_node_sets`; here we re-parse silently (the fallback `?`
    // pattern matches what `collect_node_sets` inserted into the
    // subjects set, so the edge endpoints resolve correctly).
    for ing in &inputs.ingress {
        for subject in &ing.into {
            let pat = parse_subject_silent(subject);
            edges.insert(
                EdgeKey {
                    from: Node::Ingress(ing.name.clone()).id(),
                    to: Node::Subject(pat).id(),
                    kind: EdgeKind::Ingress,
                },
                (EdgeKind::Ingress, None),
            );
        }
    }
    for eg in &inputs.egress {
        for subject in &eg.triggered_by {
            let pat = parse_subject_silent(subject);
            edges.insert(
                EdgeKey {
                    from: Node::Subject(pat).id(),
                    to: Node::Egress(eg.name.clone()).id(),
                    kind: EdgeKind::Egress,
                },
                (EdgeKind::Egress, None),
            );
        }
    }
    edges
}

/// Parse without emitting a diagnostic; `collect_node_sets` already
/// surfaced the failure. Returns the same `?` fallback so edge keys
/// resolve to existing subject nodes.
fn parse_subject_silent(raw: &str) -> NatsPattern {
    NatsPattern::parse(raw)
        .unwrap_or_else(|_| NatsPattern::parse("?").expect("? is a legal literal token"))
}

/// Emit `OrphanIngress` / `OrphanEgress` diagnostics for declared
/// patterns that no in-code call site can serve.
///
/// **Semantics** (per the flow direction encoded by `EdgeKind`):
/// - **Ingress** = external source publishes data **into** the
///   system. For the data to be useful, some Rust service must
///   *consume* (subscribe to) the declared subject. Orphan when no
///   observed **consumer** overlaps -- data flows in unread.
/// - **Egress** = external sink consumes data **out of** the
///   system. For the sink to fire, some Rust service must
///   *publish* to the declared subject. Orphan when no observed
///   **publisher** overlaps -- declared sink never receives anything.
///
/// **Why `overlaps` and not `covers`:** if config declares
/// `foo.*.baz` and code subscribes `foo.bar.*`, neither covers the
/// other but they share `foo.bar.baz` at runtime -- so the declared
/// pattern is *partially* served, not orphaned. The strict
/// `observed.covers(declared)` check would over-report. v0.1 uses
/// `overlaps` (lenient); v0.2 can add a `--strict-orphans` flag for
/// the partial-coverage case if user reports justify it.
fn detect_orphan_ingress_egress(
    resolved: &[ResolvedSite],
    inputs: &GraphInputs,
    diagnostics: &mut Vec<GraphDiagnostic>,
) {
    let observed_publishers: BTreeSet<NatsPattern> = resolved
        .iter()
        .filter(|s| matches!(s.kind, CallKind::Publish))
        .map(|s| s.subject.clone())
        .collect();
    let observed_consumers: BTreeSet<NatsPattern> = resolved
        .iter()
        .filter(|s| matches!(s.kind, CallKind::Subscribe))
        .map(|s| s.subject.clone())
        .collect();

    for ing in &inputs.ingress {
        for raw in &ing.into {
            let declared = parse_subject_silent(raw);
            let has_overlap = observed_consumers.iter().any(|c| c.overlaps(&declared));
            if !has_overlap {
                diagnostics.push(GraphDiagnostic::OrphanIngress {
                    ingress_name: ing.name.clone(),
                    pattern: declared.as_str().to_string(),
                });
            }
        }
    }
    for eg in &inputs.egress {
        for raw in &eg.triggered_by {
            let declared = parse_subject_silent(raw);
            let has_overlap = observed_publishers.iter().any(|p| p.overlaps(&declared));
            if !has_overlap {
                diagnostics.push(GraphDiagnostic::OrphanEgress {
                    egress_name: eg.name.clone(),
                    pattern: declared.as_str().to_string(),
                });
            }
        }
    }
}

fn materialise_nodes(
    services: &BTreeSet<ServiceId>,
    subjects: &BTreeSet<NatsPattern>,
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
    subjects: &BTreeSet<NatsPattern>,
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
    subject: NatsPattern,
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
    subjects: &BTreeSet<NatsPattern>,
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
        // Re-parse the subject from the id; on success, look up the
        // canonical instance in the subjects set (preserves
        // structural identity for `BTreeSet::contains` callers).
        if let Ok(pat) = NatsPattern::parse(rest) {
            if let Some(s) = subjects.get(&pat) {
                return Node::Subject(s.clone());
            }
            return Node::Subject(pat);
        }
        // Subject id failed re-parse: should be unreachable because
        // ids only originate from existing NatsPattern instances.
        // Fall back to `?` rather than panic.
        return Node::Subject(parse_subject_silent(rest));
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
    // Unknown id prefix (shouldn't happen -- every Node::id() emits
    // one of the four known prefixes). Treat as a literal subject for
    // resilience; fall back to `?` if the id itself is unparseable.
    Node::Subject(parse_subject_silent(id))
}
