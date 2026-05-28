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
    // 1 publish edge (svc-a -> foo.bar), 1 consume edge (foo.bar -> svc-b)
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
    let body = "{ let durable = format!(\"bot-{}-{}\", bot_id, symbol); subscribe(s, &durable); }";
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
    // nor a configured builder -- must hit `?` and emit a
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

/// Edge-kind discriminant ordering is load-bearing -- the sort
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
    // foo.bar (publisher) + foo.* (consumer) -> overlap.
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
    // The case coverage misses: foo.*.baz <-> foo.bar.* overlap on
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
    // Symmetric edge -> canonical (min(id), max(id)). Run twice
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
    // A<->B and C<->B does NOT imply A<->C.
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
    // Expect 2 Matches edges (A<->B, C<->B), not 3.
    assert_eq!(count_matches(&g), 2);
}

#[test]
fn matches_identical_patterns_emit_no_edge() {
    // Two publishers of `foo.bar` -- identical pattern. The
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

// ---- P2: orphan ingress/egress detection ----

fn orphan_ingress_diags(g_diags: &[GraphDiagnostic]) -> Vec<(&str, &str)> {
    g_diags
        .iter()
        .filter_map(|d| match d {
            GraphDiagnostic::OrphanIngress {
                ingress_name,
                pattern,
            } => Some((ingress_name.as_str(), pattern.as_str())),
            _ => None,
        })
        .collect()
}
fn orphan_egress_diags(g_diags: &[GraphDiagnostic]) -> Vec<(&str, &str)> {
    g_diags
        .iter()
        .filter_map(|d| match d {
            GraphDiagnostic::OrphanEgress {
                egress_name,
                pattern,
            } => Some((egress_name.as_str(), pattern.as_str())),
            _ => None,
        })
        .collect()
}

#[test]
fn orphan_ingress_flagged_when_no_consumer_overlaps() {
    // Ingress declares `foo.bar`; no service subscribes to
    // anything overlapping -> orphan (data flows in unread).
    let inputs = GraphInputs {
        ingress: vec![Ingress {
            name: "EXT".into(),
            into: vec!["foo.bar".into()],
            crate_name: None,
        }],
        ..Default::default()
    };
    let (_, diags) = build_graph(&inputs, &SymbolIndex::new());
    assert_eq!(orphan_ingress_diags(&diags), vec![("EXT", "foo.bar")]);
}

#[test]
fn orphan_ingress_quiet_when_consumer_overlaps() {
    // Ingress `foo.bar` + service subscribes `foo.*` -> overlap
    // -> not orphan (something reads the ingressed data).
    let inputs = GraphInputs {
        call_sites: vec![sub_site("svc", "foo.*")],
        ingress: vec![Ingress {
            name: "EXT".into(),
            into: vec!["foo.bar".into()],
            crate_name: None,
        }],
        ..Default::default()
    };
    let (_, diags) = build_graph(&inputs, &SymbolIndex::new());
    assert!(orphan_ingress_diags(&diags).is_empty());
}

#[test]
fn orphan_ingress_quiet_on_star_cross_overlap() {
    // Declared `foo.bar.*`, observed consumer `foo.*.baz`.
    // Neither covers the other but they overlap on `foo.bar.baz`
    // -- partially served, so v0.1 considers it NOT orphan
    // (lenient `overlaps` rule). A future --strict-orphans would
    // flip this.
    let inputs = GraphInputs {
        call_sites: vec![sub_site("svc", "foo.*.baz")],
        ingress: vec![Ingress {
            name: "EXT".into(),
            into: vec!["foo.bar.*".into()],
            crate_name: None,
        }],
        ..Default::default()
    };
    let (_, diags) = build_graph(&inputs, &SymbolIndex::new());
    assert!(orphan_ingress_diags(&diags).is_empty());
}

#[test]
fn orphan_ingress_publisher_only_is_still_orphan() {
    // Ingress feeds `foo.bar`; the *only* matching code is a
    // publisher (`foo.*`). External source's data still flows
    // into a subject nobody subscribes to -> orphan ingress.
    let inputs = GraphInputs {
        call_sites: vec![pub_site("svc", "foo.*")],
        ingress: vec![Ingress {
            name: "EXT".into(),
            into: vec!["foo.bar".into()],
            crate_name: None,
        }],
        ..Default::default()
    };
    let (_, diags) = build_graph(&inputs, &SymbolIndex::new());
    assert_eq!(orphan_ingress_diags(&diags), vec![("EXT", "foo.bar")]);
}

#[test]
fn orphan_egress_flagged_when_no_publisher_overlaps() {
    // Egress sink expects `unrelated.subject` published. The
    // only call site is a subscribe -- nothing publishes ->
    // orphan egress (sink never fires).
    let inputs = GraphInputs {
        call_sites: vec![sub_site("svc", "foo.bar")],
        egress: vec![Egress {
            name: "SINK".into(),
            from_crate: "svc".into(),
            triggered_by: vec!["unrelated.subject".into()],
        }],
        ..Default::default()
    };
    let (_, diags) = build_graph(&inputs, &SymbolIndex::new());
    assert_eq!(
        orphan_egress_diags(&diags),
        vec![("SINK", "unrelated.subject")]
    );
}

#[test]
fn orphan_egress_quiet_when_publisher_overlaps() {
    let inputs = GraphInputs {
        call_sites: vec![pub_site("svc", "foo.bar")],
        egress: vec![Egress {
            name: "SINK".into(),
            from_crate: "svc".into(),
            triggered_by: vec!["foo.*".into()],
        }],
        ..Default::default()
    };
    let (_, diags) = build_graph(&inputs, &SymbolIndex::new());
    assert!(orphan_egress_diags(&diags).is_empty());
}

#[test]
fn orphan_diagnostics_byte_stable_across_runs() {
    let inputs = GraphInputs {
        ingress: vec![
            Ingress {
                name: "A".into(),
                into: vec!["a.x".into(), "a.y".into()],
                crate_name: None,
            },
            Ingress {
                name: "B".into(),
                into: vec!["b.z".into()],
                crate_name: None,
            },
        ],
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
fn matches_determinism_1000_runs_byte_stable() {
    // Synthesis P0-H invariant 1: 1000-run byte-stability on a
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
