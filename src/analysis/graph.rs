//! Graph build — combine resolved call sites + symbol index +
//! config-declared ingress/egress into the [`crate::model::Graph`]
//! that the emit layer renders.

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
        }
    }
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
    let (services, subjects, ingress_names, egress_names) =
        collect_node_sets(&resolved, inputs, &mut diagnostics);
    let mut edges = collect_edges(&resolved, inputs);
    add_matches_edges(&mut edges, &resolved, inputs);
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
/// `subject_expr` and `consumer_name_expr` — both are NATS call
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

#[allow(clippy::type_complexity)]
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

/// Sweep `publisher_subjects × consumer_subjects` for pattern
/// overlap and emit `EdgeKind::Matches` edges where NATS would route
/// at least one concrete subject through both endpoints.
///
/// Definitions (per [04-challenge-synthesis.md, decision #3]):
/// - `publisher_subjects` = subjects that appear as the target of a
///   `Publish` edge **or** as the target of an `Ingress` edge.
/// - `consumer_subjects` = subjects that appear as the source of a
///   `Consume` edge **or** as the source of an `Egress` edge.
///
/// Edge direction is **canonical**: for each overlapping pair we
/// store the edge with `(min(id), max(id))` so two iteration orders
/// produce identical edge sets — required by synthesis §P0-H
/// invariant 1 (1000-run byte-stability).
///
/// Mermaid renders `Matches` as undirected (`---`) because the
/// relation has no flow direction — see `emit::mermaid::write_edges`.
///
/// Optimisation: pre-bucket by head-literal token. Two patterns
/// whose head literals differ can never overlap unless one of them
/// starts with `*` or `>` ("wild head"). The wild-head bucket
/// cross-multiplies against every literal-head bucket; literal-head
/// buckets compare only within themselves.
fn add_matches_edges(
    edges: &mut BTreeMap<EdgeKey, (EdgeKind, Option<String>)>,
    resolved: &[ResolvedSite],
    inputs: &GraphInputs,
) {
    // All-pairs sweep over the union of every subject that appears
    // anywhere in the graph. The challenge-synthesis originally
    // specified `publisher_subjects × consumer_subjects`, but that
    // shape misses publisher-publisher overlaps (e.g. gordon-data
    // publishes `market.klines.binance.*.*.*` while the Binance
    // ingress declares `market.klines.binance.spot.*.1m` — both
    // publishers, structurally overlapping, audit-worthy). All-pairs
    // matches challenge-02's hand-counted 8 expected edges on Gordon
    // and gives a complete view of subject-pattern relationships.
    let mut all_subjects: BTreeSet<NatsPattern> = BTreeSet::new();
    for site in resolved {
        all_subjects.insert(site.subject.clone());
    }
    for ing in &inputs.ingress {
        for s in &ing.into {
            all_subjects.insert(parse_subject_silent(s));
        }
    }
    for eg in &inputs.egress {
        for s in &eg.triggered_by {
            all_subjects.insert(parse_subject_silent(s));
        }
    }

    let buckets = head_buckets(&all_subjects);
    sweep_buckets(&buckets, edges);
}

/// Head-literal bucketing: `BTreeMap<Option<String>, Vec<&NatsPattern>>`.
/// The `None` key holds patterns whose first segment is `*` or `>`
/// ("wild head"); these cross-multiply against every literal bucket.
fn head_buckets(set: &BTreeSet<NatsPattern>) -> BTreeMap<Option<String>, Vec<&NatsPattern>> {
    use crate::model::Segment;
    let mut buckets: BTreeMap<Option<String>, Vec<&NatsPattern>> = BTreeMap::new();
    for pat in set {
        let key = match pat.segments().first() {
            Some(Segment::Literal(head)) => Some(head.clone()),
            _ => None,
        };
        buckets.entry(key).or_default().push(pat);
    }
    buckets
}

fn sweep_buckets(
    buckets: &BTreeMap<Option<String>, Vec<&NatsPattern>>,
    edges: &mut BTreeMap<EdgeKey, (EdgeKind, Option<String>)>,
) {
    let empty: Vec<&NatsPattern> = Vec::new();
    let wild = buckets.get(&None).unwrap_or(&empty);

    // Same-head literal buckets: compare each unordered pair within
    // the bucket once; then cross-multiply each literal bucket
    // against the wild-head bucket.
    for (key, members) in buckets {
        if key.is_none() {
            continue;
        }
        emit_within(members, edges);
        emit_cross(members, wild, edges);
    }
    // Wild × wild internal pairs (e.g. two `*.foo` patterns or `>`
    // alongside `*.bar` — none on Gordon today, but the algorithm
    // must handle it).
    emit_within(wild, edges);
}

/// Emit Matches edges for every unordered pair within `members`
/// whose patterns overlap (and are not identical).
fn emit_within(
    members: &[&NatsPattern],
    edges: &mut BTreeMap<EdgeKey, (EdgeKind, Option<String>)>,
) {
    for (i, a) in members.iter().enumerate() {
        for b in &members[i + 1..] {
            if a == b || !a.overlaps(b) {
                continue;
            }
            insert_matches_edge(a, b, edges);
        }
    }
}

/// Emit Matches edges across two disjoint groups (every member of
/// `left` against every member of `right`).
fn emit_cross(
    left: &[&NatsPattern],
    right: &[&NatsPattern],
    edges: &mut BTreeMap<EdgeKey, (EdgeKind, Option<String>)>,
) {
    for a in left {
        for b in right {
            if a == b || !a.overlaps(b) {
                continue;
            }
            insert_matches_edge(a, b, edges);
        }
    }
}

fn insert_matches_edge(
    a: &NatsPattern,
    b: &NatsPattern,
    edges: &mut BTreeMap<EdgeKey, (EdgeKind, Option<String>)>,
) {
    let (lo, hi) = canonical_pair(a, b);
    edges.insert(
        EdgeKey {
            from: Node::Subject(lo.clone()).id(),
            to: Node::Subject(hi.clone()).id(),
            kind: EdgeKind::Matches,
        },
        (EdgeKind::Matches, None),
    );
}

/// Canonical edge direction for symmetric Matches edges: smaller
/// `Node::id()` first. Ensures `sweep_buckets` produces identical
/// edge set regardless of iteration order.
fn canonical_pair<'a>(
    a: &'a NatsPattern,
    b: &'a NatsPattern,
) -> (&'a NatsPattern, &'a NatsPattern) {
    if Node::Subject(a.clone()).id() <= Node::Subject(b.clone()).id() {
        (a, b)
    } else {
        (b, a)
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
    // Unknown id prefix (shouldn't happen — every Node::id() emits
    // one of the four known prefixes). Treat as a literal subject for
    // resilience; fall back to `?` if the id itself is unparseable.
    Node::Subject(parse_subject_silent(id))
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
                && matches!(&e.to, Node::Subject(s) if s.as_str() == "foo.bar")));
        assert!(graph
            .edges
            .iter()
            .any(|e| matches!(e.kind, EdgeKind::Consume)
                && matches!(&e.from, Node::Subject(s) if s.as_str() == "foo.bar")
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

    fn subscribe_site(
        crate_name: &str,
        subject_expr: &str,
        consumer_name_expr: Option<&str>,
        fn_body: Option<&str>,
    ) -> (String, RawCallSite) {
        (
            crate_name.to_string(),
            RawCallSite {
                kind: CallKind::Subscribe,
                file: PathBuf::from(format!("{crate_name}/src/lib.rs")),
                line: 1,
                column: 1,
                subject_expr: subject_expr.to_string(),
                consumer_name_expr: consumer_name_expr.map(String::from),
                matched_method: "subscribe".to_string(),
                enclosing_fn_body: fn_body.map(String::from),
            },
        )
    }

    fn consume_label(g: &Graph) -> Option<&str> {
        g.edges
            .iter()
            .find(|e| matches!(e.kind, EdgeKind::Consume))
            .and_then(|e| e.label.as_deref())
    }

    #[test]
    fn consumer_name_literal_resolves_to_self() {
        let inputs = GraphInputs {
            call_sites: vec![subscribe_site(
                "svc",
                "\"foo.bar\"",
                Some("\"executor-default\""),
                None,
            )],
            ..Default::default()
        };
        let (g, diags) = build_graph(&inputs, &SymbolIndex::new());
        assert!(diags.is_empty(), "literal resolved cleanly");
        assert_eq!(consume_label(&g), Some("executor-default"));
    }

    #[test]
    fn consumer_name_reference_to_literal_strips_amp() {
        let inputs = GraphInputs {
            call_sites: vec![subscribe_site(
                "svc",
                "\"foo.bar\"",
                Some("&\"my-durable\""),
                None,
            )],
            ..Default::default()
        };
        let (g, _) = build_graph(&inputs, &SymbolIndex::new());
        assert_eq!(consume_label(&g), Some("my-durable"));
    }

    #[test]
    fn consumer_name_local_binding_followed_via_fn_body() {
        // Mirrors the Gordon shape: `let consumer_name_owned =
        // BUS_CONSUMER_NAME.to_owned(); subscribe(&s, &consumer_name_owned)`.
        let body = "{ let consumer_name_owned = \"executor-default\".to_owned(); subscribe(s, &consumer_name_owned); }";
        let inputs = GraphInputs {
            call_sites: vec![subscribe_site(
                "svc",
                "\"intents.executor\"",
                Some("&consumer_name_owned"),
                Some(body),
            )],
            ..Default::default()
        };
        let (g, diags) = build_graph(&inputs, &SymbolIndex::new());
        assert!(diags.is_empty(), "local binding resolved");
        assert_eq!(consume_label(&g), Some("executor-default"));
    }

    #[test]
    fn consumer_name_format_macro_renders_wildcards() {
        let body =
            "{ let durable = format!(\"bot-{}-{}\", bot_id, symbol); subscribe(s, &durable); }";
        let inputs = GraphInputs {
            call_sites: vec![subscribe_site(
                "svc",
                "\"foo.bar\"",
                Some("&durable"),
                Some(body),
            )],
            ..Default::default()
        };
        let (g, _) = build_graph(&inputs, &SymbolIndex::new());
        assert_eq!(consume_label(&g), Some("bot-*-*"));
    }

    #[test]
    fn consumer_name_unresolvable_emits_diagnostic() {
        // `something_runtime` is neither a let-binding nor a const
        // nor a configured builder — must hit `?` and emit a
        // structured diagnostic.
        let inputs = GraphInputs {
            call_sites: vec![subscribe_site(
                "svc",
                "\"foo.bar\"",
                Some("&something_runtime"),
                None,
            )],
            ..Default::default()
        };
        let (g, diags) = build_graph(&inputs, &SymbolIndex::new());
        assert_eq!(consume_label(&g), Some("?"));
        assert!(
            diags
                .iter()
                .any(|d| matches!(d, GraphDiagnostic::UnresolvedConsumerName { .. })),
            "expected UnresolvedConsumerName diagnostic, got {diags:?}"
        );
    }

    #[test]
    fn diagnostics_are_byte_stable_across_runs() {
        // Determinism check including the new diagnostic kind.
        let inputs = GraphInputs {
            call_sites: vec![subscribe_site(
                "svc",
                "\"foo.bar\"",
                Some("&missing_var"),
                None,
            )],
            ..Default::default()
        };
        let index = SymbolIndex::new();
        let (_, d1) = build_graph(&inputs, &index);
        let (_, d2) = build_graph(&inputs, &index);
        let s1: Vec<String> = d1.iter().map(GraphDiagnostic::to_string).collect();
        let s2: Vec<String> = d2.iter().map(GraphDiagnostic::to_string).collect();
        assert_eq!(s1, s2);
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

    // ---- P1 commit 4: Matches sweep tests ----

    /// Edge-kind discriminant ordering is load-bearing — the sort
    /// comparator in `materialise_edges` casts to `u8`. Lock the
    /// values so future variants can't accidentally reorder.
    #[test]
    fn edge_kind_discriminants_locked() {
        assert_eq!(EdgeKind::Publish as u8, 0);
        assert_eq!(EdgeKind::Consume as u8, 1);
        assert_eq!(EdgeKind::Ingress as u8, 2);
        assert_eq!(EdgeKind::Egress as u8, 3);
        assert_eq!(EdgeKind::Matches as u8, 4);
    }

    fn pub_site(crate_name: &str, expr: &str) -> (String, RawCallSite) {
        site(crate_name, CallKind::Publish, &format!("\"{expr}\""))
    }
    fn sub_site(crate_name: &str, expr: &str) -> (String, RawCallSite) {
        site(crate_name, CallKind::Subscribe, &format!("\"{expr}\""))
    }
    fn count_matches(g: &Graph) -> usize {
        g.edges
            .iter()
            .filter(|e| matches!(e.kind, EdgeKind::Matches))
            .count()
    }
    fn matches_pairs(g: &Graph) -> Vec<(String, String)> {
        let mut out: Vec<_> = g
            .edges
            .iter()
            .filter(|e| matches!(e.kind, EdgeKind::Matches))
            .map(|e| (e.from.id(), e.to.id()))
            .collect();
        out.sort();
        out
    }

    #[test]
    fn matches_overlap_star_vs_literal() {
        // foo.bar (publisher) + foo.* (consumer) → overlap.
        let inputs = GraphInputs {
            call_sites: vec![pub_site("a", "foo.bar"), sub_site("b", "foo.*")],
            ..Default::default()
        };
        let (g, _) = build_graph(&inputs, &SymbolIndex::new());
        assert_eq!(count_matches(&g), 1);
    }

    #[test]
    fn matches_no_overlap_when_unrelated() {
        let inputs = GraphInputs {
            call_sites: vec![pub_site("a", "foo.bar"), sub_site("b", "baz.qux")],
            ..Default::default()
        };
        let (g, _) = build_graph(&inputs, &SymbolIndex::new());
        assert_eq!(count_matches(&g), 0);
    }

    #[test]
    fn matches_star_cross_position() {
        // The case coverage misses: foo.*.baz ↔ foo.bar.* overlap on
        // foo.bar.baz. Neither covers the other.
        let inputs = GraphInputs {
            call_sites: vec![pub_site("a", "foo.*.baz"), sub_site("b", "foo.bar.*")],
            ..Default::default()
        };
        let (g, _) = build_graph(&inputs, &SymbolIndex::new());
        assert_eq!(count_matches(&g), 1);
    }

    #[test]
    fn matches_publisher_publisher_overlap() {
        // Two publishers with overlapping subject families: a real
        // drift smell, not just routing surface. Our all-pairs sweep
        // catches it (matches challenge-02 hand-count of 8 on Gordon).
        let inputs = GraphInputs {
            call_sites: vec![
                pub_site("a", "market.klines.binance.*.*.*"),
                pub_site("b", "market.klines.binance.spot.*.1m"),
            ],
            ..Default::default()
        };
        let (g, _) = build_graph(&inputs, &SymbolIndex::new());
        assert_eq!(count_matches(&g), 1);
    }

    #[test]
    fn matches_canonical_direction_min_id_first() {
        // Symmetric edge → canonical (min(id), max(id)). Run twice
        // with reversed insertion order, assert identical edge set.
        let order_a = GraphInputs {
            call_sites: vec![pub_site("a", "foo.bar"), sub_site("b", "foo.*")],
            ..Default::default()
        };
        let order_b = GraphInputs {
            call_sites: vec![pub_site("b", "foo.*"), sub_site("a", "foo.bar")],
            ..Default::default()
        };
        let (g1, _) = build_graph(&order_a, &SymbolIndex::new());
        let (g2, _) = build_graph(&order_b, &SymbolIndex::new());
        assert_eq!(matches_pairs(&g1), matches_pairs(&g2));

        // And verify the from-id is the lexicographically smaller one.
        let match_edge = g1
            .edges
            .iter()
            .find(|e| matches!(e.kind, EdgeKind::Matches))
            .expect("expected Matches edge");
        assert!(
            match_edge.from.id() <= match_edge.to.id(),
            "canonical pair: from.id ({}) must <= to.id ({})",
            match_edge.from.id(),
            match_edge.to.id()
        );
    }

    #[test]
    fn matches_transitive_non_closure() {
        // A↔B and C↔B does NOT imply A↔C.
        // A = foo.bar.baz, B = foo.*.baz, C = foo.qux.baz.
        let inputs = GraphInputs {
            call_sites: vec![
                pub_site("a", "foo.bar.baz"),
                pub_site("c", "foo.qux.baz"),
                sub_site("b", "foo.*.baz"),
            ],
            ..Default::default()
        };
        let (g, _) = build_graph(&inputs, &SymbolIndex::new());
        // Expect 2 Matches edges (A↔B, C↔B), not 3.
        assert_eq!(count_matches(&g), 2);
    }

    #[test]
    fn matches_identical_patterns_emit_no_edge() {
        // Two publishers of `foo.bar` — identical pattern. The
        // sweep skips `a == b` so no Matches edge is emitted.
        let inputs = GraphInputs {
            call_sites: vec![pub_site("a", "foo.bar"), pub_site("b", "foo.bar")],
            ..Default::default()
        };
        let (g, _) = build_graph(&inputs, &SymbolIndex::new());
        assert_eq!(count_matches(&g), 0);
    }

    #[test]
    fn matches_ingress_target_participates_in_sweep() {
        // Ingress targets are publisher-side subjects per the sweep
        // definition. Verify they trigger Matches edges.
        let inputs = GraphInputs {
            call_sites: vec![sub_site("svc", "foo.*")],
            ingress: vec![Ingress {
                name: "EXT".into(),
                into: vec!["foo.bar".into()],
                crate_name: None,
            }],
            ..Default::default()
        };
        let (g, _) = build_graph(&inputs, &SymbolIndex::new());
        assert_eq!(count_matches(&g), 1);
    }

    #[test]
    fn matches_egress_source_participates_in_sweep() {
        let inputs = GraphInputs {
            call_sites: vec![pub_site("svc", "foo.bar")],
            egress: vec![Egress {
                name: "SINK".into(),
                from_crate: "svc".into(),
                triggered_by: vec!["foo.*".into()],
            }],
            ..Default::default()
        };
        let (g, _) = build_graph(&inputs, &SymbolIndex::new());
        assert_eq!(count_matches(&g), 1);
    }

    #[test]
    fn matches_determinism_1000_runs_byte_stable() {
        // Synthesis §P0-H invariant 1: 1000-run byte-stability on a
        // Matches-edge-producing fixture. The brute-force loop is
        // cheap (build is sub-ms) and locks the canonical sort.
        let inputs = GraphInputs {
            call_sites: vec![
                pub_site("a", "market.klines.binance.*.*.*"),
                pub_site("b", "market.klines.binance.spot.*.1m"),
                sub_site("c", "market.klines.binance.spot.*.*"),
                sub_site("d", "risk.events.>"),
                pub_site("e", "risk.events.*"),
            ],
            ..Default::default()
        };
        let index = SymbolIndex::new();
        let (baseline, _) = build_graph(&inputs, &index);
        let baseline_pairs = matches_pairs(&baseline);
        for run in 1..1000 {
            let (g, _) = build_graph(&inputs, &index);
            assert_eq!(
                matches_pairs(&g),
                baseline_pairs,
                "run #{run} diverged from baseline"
            );
        }
    }
}
