//! Pattern-overlap ("Matches") sweep for the graph builder.
//!
//! Sweeps all subject patterns in the graph for pairwise overlap
//! and emits `EdgeKind::Matches` edges where NATS would route at
//! least one concrete subject through both endpoints.

use std::collections::{BTreeMap, BTreeSet};

use crate::model::{EdgeKind, NatsPattern, Node, Segment};

use super::{parse_subject_silent, EdgeKey, GraphInputs, ResolvedSite};

/// Sweep `publisher_subjects x consumer_subjects` for pattern
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
/// produce identical edge sets -- required by synthesis P0-H
/// invariant 1 (1000-run byte-stability).
///
/// Mermaid renders `Matches` as undirected (`---`) because the
/// relation has no flow direction -- see `emit::mermaid::write_edges`.
///
/// Optimisation: pre-bucket by head-literal token. Two patterns
/// whose head literals differ can never overlap unless one of them
/// starts with `*` or `>` ("wild head"). The wild-head bucket
/// cross-multiplies against every literal-head bucket; literal-head
/// buckets compare only within themselves.
pub(super) fn add_matches_edges(
    edges: &mut BTreeMap<EdgeKey, (EdgeKind, Option<String>)>,
    resolved: &[ResolvedSite],
    inputs: &GraphInputs,
) {
    // All-pairs sweep over the union of every subject that appears
    // anywhere in the graph. The challenge-synthesis originally
    // specified `publisher_subjects x consumer_subjects`, but that
    // shape misses publisher-publisher overlaps (e.g. gordon-data
    // publishes `market.klines.binance.*.*.*` while the Binance
    // ingress declares `market.klines.binance.spot.*.1m` -- both
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
    // Wild x wild internal pairs (e.g. two `*.foo` patterns or `>`
    // alongside `*.bar` -- none on Gordon today, but the algorithm
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
