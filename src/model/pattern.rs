//! NATS subject pattern algebra.
//!
//! `NatsPattern` is the structural representation of a NATS subject:
//! a sequence of `.`-separated segments where each segment is either
//! a literal token, the single-segment wildcard `*`, or the tail
//! wildcard `>` (which must appear last and only once).
//!
//! # API contract
//!
//! - **Construction is fallible** ([`NatsPattern::parse`] /
//!   [`NatsPattern::from_segments`]). Every `NatsPattern` in
//!   existence is well-formed by construction — downstream code does
//!   not need to re-validate.
//! - **Fields are private.** `Segment::Literal(String)` never holds
//!   `*`, `>`, `.`, whitespace, or non-ASCII bytes; callers cannot
//!   bypass that invariant.
//! - **Equality is structural** ([`PartialEq`]/[`Eq`]/[`Hash`]).
//!   `foo.>` and `foo.*.>` cover identical concrete subjects but
//!   are kept distinct — the audit value of the diagram depends on
//!   preserving the source-form distinction.
//! - **Ordering matches the pre-`NatsPattern` `String` byte order**
//!   ([`Ord`] via `as_str().cmp(...)`). Without this guarantee,
//!   threading `NatsPattern` through the graph would shift every
//!   existing Mermaid snapshot (`*` < `>` < letters byte-wise, but a
//!   naive `Vec<Segment>` `Ord` would put `Literal < Star < Gt`).
//! - **`Display` round-trips**: `parse(p.to_string()).unwrap() == p`
//!   holds for every well-formed pattern. Property-tested below.
//!
//! Two distinct matching relations live on this type:
//!
//! - [`NatsPattern::overlaps`] (**symmetric**) — "is there at least
//!   one concrete subject that both patterns accept?" Used by P1's
//!   `EdgeKind::Matches` sweep where both sides may carry wildcards.
//! - [`NatsPattern::covers`] (**asymmetric**) — "does `self` accept
//!   every concrete subject `other` accepts?" Used by P2's orphan
//!   ingress/egress detection where one side is a declared
//!   ingress/egress pattern and the other is the observed
//!   publisher/consumer set.
//!
//! Both relations run in O(min(|a|, |b|)) — the recursion never
//! branches (no exponential blow-up).

use std::fmt;
use std::str::FromStr;

/// A single segment of a NATS subject pattern.
///
/// `Literal(String)` only holds well-formed literal tokens — no `.`,
/// `*`, `>`, whitespace, or non-ASCII bytes. The parser rejects
/// illegal characters with [`ParseError::IllegalCharInToken`].
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Segment {
    /// A literal subject token (e.g. `foo`, `market`, `1m`).
    Literal(String),
    /// The single-segment wildcard `*`. Matches exactly one segment.
    Star,
    /// The tail wildcard `>`. Matches one-or-more segments; must
    /// appear last in the pattern.
    Gt,
}

/// A parsed, well-formed NATS subject pattern.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct NatsPattern {
    segments: Vec<Segment>,
    /// Canonical surface form. Cached on construction so `as_str()`
    /// and `cmp` are O(1) instead of re-rendering.
    canonical: String,
}

/// Parse / validation errors for [`NatsPattern`].
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ParseError {
    /// Input was empty or contained only dot separators.
    #[error("empty pattern")]
    Empty,

    /// A segment was empty (e.g. `foo..bar` or leading/trailing `.`).
    #[error("empty segment at position {0}")]
    EmptySegment(usize),

    /// A `>` appeared in a non-tail position.
    #[error("`>` (tail wildcard) must appear last; found at position {0}")]
    GtNotAtTail(usize),

    /// A literal token contained `*`, `>`, `.`, whitespace, or non-ASCII.
    /// Position is the segment index (0-based).
    #[error("illegal character in literal token at position {position}: {token:?}")]
    IllegalCharInToken {
        /// 0-based segment index.
        position: usize,
        /// The rejected token (verbatim).
        token: String,
    },
}

impl NatsPattern {
    /// Parse a NATS subject string into a structural pattern.
    ///
    /// Strict: rejects empty input, empty segments (`foo..bar`,
    /// leading/trailing `.`), `>` in non-tail positions, and
    /// literal tokens containing `*`, `>`, `.`, whitespace, or
    /// non-ASCII bytes.
    ///
    /// # Errors
    /// Returns [`ParseError`] on any of the above.
    pub fn parse(s: &str) -> Result<Self, ParseError> {
        if s.is_empty() {
            return Err(ParseError::Empty);
        }
        let raw_segments: Vec<&str> = s.split('.').collect();
        let mut segments = Vec::with_capacity(raw_segments.len());
        for (i, raw) in raw_segments.iter().enumerate() {
            if raw.is_empty() {
                return Err(ParseError::EmptySegment(i));
            }
            let seg = match *raw {
                "*" => Segment::Star,
                ">" => {
                    if i != raw_segments.len() - 1 {
                        return Err(ParseError::GtNotAtTail(i));
                    }
                    Segment::Gt
                }
                token => {
                    if !is_legal_literal(token) {
                        return Err(ParseError::IllegalCharInToken {
                            position: i,
                            token: token.to_string(),
                        });
                    }
                    Segment::Literal(token.to_string())
                }
            };
            segments.push(seg);
        }
        let canonical = render_segments(&segments);
        Ok(Self {
            segments,
            canonical,
        })
    }

    /// Construct from a pre-built segment list. Validates the same
    /// invariants as [`NatsPattern::parse`].
    ///
    /// # Errors
    /// Returns [`ParseError`] on empty input, `>` not at tail, or
    /// any literal token containing illegal characters.
    pub fn from_segments(segments: Vec<Segment>) -> Result<Self, ParseError> {
        if segments.is_empty() {
            return Err(ParseError::Empty);
        }
        for (i, seg) in segments.iter().enumerate() {
            match seg {
                Segment::Gt if i != segments.len() - 1 => {
                    return Err(ParseError::GtNotAtTail(i));
                }
                Segment::Literal(token) if !is_legal_literal(token) => {
                    return Err(ParseError::IllegalCharInToken {
                        position: i,
                        token: token.clone(),
                    });
                }
                _ => {}
            }
        }
        let canonical = render_segments(&segments);
        Ok(Self {
            segments,
            canonical,
        })
    }

    /// Borrow the segment list (read-only).
    #[must_use]
    pub fn segments(&self) -> &[Segment] {
        &self.segments
    }

    /// Canonical string form (round-trips through [`NatsPattern::parse`]).
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.canonical
    }

    /// `true` iff `self` accepts every concrete subject `other` accepts.
    ///
    /// Asymmetric. `foo.*.covers(foo.bar)` = true; reverse = false.
    /// Used by P2 orphan ingress/egress detection.
    #[must_use]
    pub fn covers(&self, other: &Self) -> bool {
        covers_segments(&self.segments, &other.segments)
    }

    /// `true` iff there exists at least one concrete subject that
    /// both `self` and `other` accept.
    ///
    /// Symmetric: `a.overlaps(b) == b.overlaps(a)`. Used by P1's
    /// `EdgeKind::Matches` sweep — catches the "neither covers the
    /// other but they share a routing target" case
    /// (e.g. `foo.*.baz` ↔ `foo.bar.*` overlap on `foo.bar.baz`).
    #[must_use]
    pub fn overlaps(&self, other: &Self) -> bool {
        overlaps_segments(&self.segments, &other.segments)
    }
}

impl fmt::Display for NatsPattern {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.canonical)
    }
}

impl FromStr for NatsPattern {
    type Err = ParseError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::parse(s)
    }
}

impl PartialOrd for NatsPattern {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

/// Ordering matches the pre-`NatsPattern` `String` byte order. Without
/// this, threading `NatsPattern` through `BTreeSet`-backed graph
/// dedupe would shift every existing Mermaid snapshot.
impl Ord for NatsPattern {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.canonical.cmp(&other.canonical)
    }
}

// ---- internals ----

fn is_legal_literal(token: &str) -> bool {
    if token.is_empty() {
        return false;
    }
    token.chars().all(|c| {
        c.is_ascii()
            && !c.is_ascii_whitespace()
            && c != '.'
            && c != '*'
            && c != '>'
            && !c.is_ascii_control()
    })
}

fn render_segments(segments: &[Segment]) -> String {
    let mut out = String::new();
    for (i, seg) in segments.iter().enumerate() {
        if i > 0 {
            out.push('.');
        }
        match seg {
            Segment::Literal(s) => out.push_str(s),
            Segment::Star => out.push('*'),
            Segment::Gt => out.push('>'),
        }
    }
    out
}

/// `true` iff every concrete subject matching `a` also matches `b`.
///
/// (`b` covers `a` — i.e. `b` is more-or-equal-permissive.) Arm
/// ordering encodes the asymmetric `Gt`/`Star` logic; identical-body
/// arms are merged per clippy `match_same_arms`.
fn covers_segments(b: &[Segment], a: &[Segment]) -> bool {
    use Segment::{Gt, Literal, Star};
    match (b.first(), a.first()) {
        // Both exhausted (matches empty extension) or `Gt` on `b`
        // absorbing a non-empty tail of `a` → covered.
        (None, None) | (Some(Gt), Some(_)) => true,
        // Any exhaustion mismatch (extra segments on `a`, missing
        // segments on `a`, `Gt`/`Star` on `a` with stricter `b`)
        // → not covered.
        (None, Some(_)) | (Some(_), None | Some(Gt)) | (Some(Literal(_)), Some(Star)) => false,
        // `*` on `b` accepts any single concrete segment from `a`.
        (Some(Star), Some(_)) => covers_segments(&b[1..], &a[1..]),
        // Two literals: must match exactly.
        (Some(Literal(x)), Some(Literal(y))) => x == y && covers_segments(&b[1..], &a[1..]),
    }
}

/// `true` iff there exists at least one concrete subject accepted by
/// both `a` and `b`. Symmetric.
fn overlaps_segments(a: &[Segment], b: &[Segment]) -> bool {
    use Segment::{Gt, Literal, Star};
    match (a.first(), b.first()) {
        // Both exhausted, or `Gt` on either side absorbing the
        // (non-empty) other side → overlap.
        (None, None) | (Some(Gt), Some(_)) | (Some(_), Some(Gt)) => true,
        // Any exhaustion mismatch (including `Gt` vs empty —
        // `Gt` requires one-or-more, so no common concrete subject):
        // no overlap.
        (None, Some(_)) | (Some(_), None) => false,
        // `*` matches any single segment.
        (Some(Star), Some(_)) | (Some(_), Some(Star)) => overlaps_segments(&a[1..], &b[1..]),
        // Two literals: must match exactly.
        (Some(Literal(x)), Some(Literal(y))) => x == y && overlaps_segments(&a[1..], &b[1..]),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(s: &str) -> NatsPattern {
        NatsPattern::parse(s).expect(s)
    }

    // ---- parse ----

    #[test]
    fn parse_literal() {
        let pat = p("foo.bar.baz");
        assert_eq!(pat.segments().len(), 3);
        assert_eq!(pat.as_str(), "foo.bar.baz");
    }

    #[test]
    fn parse_star() {
        let pat = p("foo.*.baz");
        assert_eq!(pat.segments()[1], Segment::Star);
    }

    #[test]
    fn parse_gt_at_tail() {
        let pat = p("foo.>");
        assert_eq!(pat.segments().last(), Some(&Segment::Gt));
    }

    #[test]
    fn parse_bare_gt() {
        // A bare `>` is a legal NATS subscription (subscribes to
        // everything). Empty input is rejected; `>` alone is not empty.
        let pat = p(">");
        assert_eq!(pat.segments(), &[Segment::Gt]);
    }

    #[test]
    fn parse_rejects_empty() {
        assert_eq!(NatsPattern::parse(""), Err(ParseError::Empty));
    }

    #[test]
    fn parse_rejects_empty_segment() {
        assert!(matches!(
            NatsPattern::parse("foo..bar"),
            Err(ParseError::EmptySegment(1))
        ));
        assert!(matches!(
            NatsPattern::parse(".foo"),
            Err(ParseError::EmptySegment(0))
        ));
        assert!(matches!(
            NatsPattern::parse("foo."),
            Err(ParseError::EmptySegment(1))
        ));
    }

    #[test]
    fn parse_rejects_gt_not_at_tail() {
        assert!(matches!(
            NatsPattern::parse("foo.>.bar"),
            Err(ParseError::GtNotAtTail(1))
        ));
    }

    #[test]
    fn parse_rejects_whitespace_in_token() {
        assert!(matches!(
            NatsPattern::parse("foo bar.baz"),
            Err(ParseError::IllegalCharInToken { position: 0, .. })
        ));
    }

    #[test]
    fn parse_rejects_non_ascii_in_token() {
        assert!(matches!(
            NatsPattern::parse("market.klines.btç.1m"),
            Err(ParseError::IllegalCharInToken { position: 2, .. })
        ));
    }

    #[test]
    fn parse_accepts_star_prefix_literal_unchanged() {
        // `*x` is a typo waiting to happen — NATS itself treats it
        // as a literal token starting with `*` (the wildcard rule
        // only applies to single-char `*`). Our strict parser rejects
        // it because `*` is not legal in a literal token.
        assert!(matches!(
            NatsPattern::parse("foo.*x.bar"),
            Err(ParseError::IllegalCharInToken { position: 1, .. })
        ));
    }

    // ---- from_segments ----

    #[test]
    fn from_segments_validates_gt_at_tail() {
        let bad = vec![
            Segment::Literal("foo".into()),
            Segment::Gt,
            Segment::Literal("bar".into()),
        ];
        assert!(matches!(
            NatsPattern::from_segments(bad),
            Err(ParseError::GtNotAtTail(1))
        ));
    }

    #[test]
    fn from_segments_validates_literal_chars() {
        let bad = vec![Segment::Literal("foo.bar".into())];
        assert!(matches!(
            NatsPattern::from_segments(bad),
            Err(ParseError::IllegalCharInToken { position: 0, .. })
        ));
    }

    #[test]
    fn from_segments_rejects_empty() {
        assert_eq!(NatsPattern::from_segments(vec![]), Err(ParseError::Empty));
    }

    // ---- Display round-trip ----

    #[test]
    fn display_round_trips_for_all_fixtures() {
        let fixtures = &[
            "foo",
            "foo.bar",
            "foo.bar.baz",
            "foo.*",
            "foo.*.baz",
            "foo.>",
            ">",
            "market.klines.binance.spot.*.*",
            "service.deploy.*.commands",
            "a.b.c.d.e.f.g.h.i.j",
        ];
        for s in fixtures {
            let pat = p(s);
            assert_eq!(pat.to_string(), *s, "Display round-trip broke for {s:?}");
            let reparsed = NatsPattern::parse(&pat.to_string()).unwrap();
            assert_eq!(reparsed, pat, "reparse differs for {s:?}");
        }
    }

    // ---- Ord matches String byte order ----

    #[test]
    fn ord_matches_string_byte_order() {
        // Critical: BTreeSet<NatsPattern> must produce the same
        // iteration order as the pre-fix BTreeSet<String>. Otherwise
        // every Mermaid snapshot shifts. See challenge-01 §F1.3.
        let mut as_strings: Vec<String> = vec![
            "foo.*".into(),
            "foo.>".into(),
            "foo.a".into(),
            "foo.bar".into(),
            "foo.baz".into(),
            "*.bar".into(),
            ">.alpha".into(), // illegal — won't make a pattern
        ];
        as_strings.retain(|s| NatsPattern::parse(s).is_ok());
        as_strings.sort();

        let mut as_pats: Vec<NatsPattern> = as_strings.iter().map(|s| p(s)).collect();
        as_pats.sort();

        let from_pats: Vec<String> = as_pats.iter().map(NatsPattern::to_string).collect();
        assert_eq!(
            as_strings, from_pats,
            "NatsPattern: Ord disagrees with String: Ord on canonical form"
        );
    }

    // ---- overlaps (symmetric) ----

    #[test]
    fn overlaps_lit_vs_star() {
        assert!(p("foo.bar").overlaps(&p("foo.*")));
        assert!(p("foo.*").overlaps(&p("foo.bar")));
    }

    #[test]
    fn overlaps_lit_vs_gt() {
        assert!(p("foo.bar.baz").overlaps(&p("foo.>")));
        assert!(p("foo.>").overlaps(&p("foo.bar.baz")));
    }

    #[test]
    fn overlaps_star_cross_position() {
        // The category coverage misses: neither side covers the other
        // but they share `foo.bar.baz`.
        assert!(p("foo.*.baz").overlaps(&p("foo.bar.*")));
    }

    #[test]
    fn overlaps_gt_vs_star_prefix() {
        // foo.>  vs  *.bar.baz  → overlap on `foo.bar.baz`.
        assert!(p("foo.>").overlaps(&p("*.bar.baz")));
    }

    #[test]
    fn overlaps_no_overlap_when_literals_differ() {
        assert!(!p("foo.bar").overlaps(&p("foo.baz")));
        assert!(!p("foo.*.baz").overlaps(&p("foo.*.qux")));
    }

    #[test]
    fn overlaps_no_overlap_when_heads_differ() {
        assert!(!p("foo.*").overlaps(&p("bar.*")));
    }

    #[test]
    fn overlaps_anti_overlap_positional_literal() {
        // foo.*.baz vs foo.x.qux — position-3 literals differ.
        assert!(!p("foo.*.baz").overlaps(&p("foo.x.qux")));
    }

    #[test]
    fn overlaps_self_is_reflexive() {
        for s in &["foo.bar", "foo.*", "foo.>", "*.bar.>", ">", "a.b.c"] {
            let pat = p(s);
            assert!(pat.overlaps(&pat), "{s} should overlap with itself");
        }
    }

    #[test]
    fn overlaps_is_symmetric_property_test() {
        // Brute-force "property test" over a hand-picked set.
        // (Avoiding the proptest dev-dep per challenge-02 §N6.)
        let patterns = [
            "foo",
            "foo.bar",
            "foo.*",
            "foo.>",
            "*",
            "*.bar",
            "foo.bar.baz",
            "foo.*.baz",
            "foo.bar.*",
            "*.bar.*",
            "*.*.baz",
            "foo.*.*",
            "a.b.c",
            ">",
            "foo.>",
        ];
        for a_s in &patterns {
            for b_s in &patterns {
                let a = p(a_s);
                let b = p(b_s);
                assert_eq!(
                    a.overlaps(&b),
                    b.overlaps(&a),
                    "overlaps not symmetric for ({a_s}, {b_s})"
                );
            }
        }
    }

    #[test]
    fn overlaps_gt_requires_one_or_more() {
        // `foo.>` requires at least one segment after `foo` — it does
        // NOT overlap with the bare `foo`. Both interpretations would
        // be defensible per NATS docs; we choose strict "one-or-more".
        assert!(!p("foo.>").overlaps(&p("foo")));
        assert!(!p("foo").overlaps(&p("foo.>")));
    }

    #[test]
    fn overlaps_transitive_non_closure() {
        // A↔B and C↔B does NOT imply A↔C.
        // A = foo.bar.baz, B = foo.*.baz, C = foo.qux.baz.
        // A & B overlap (foo.bar.baz). C & B overlap (foo.qux.baz).
        // A & C do NOT overlap.
        let a = p("foo.bar.baz");
        let b = p("foo.*.baz");
        let c = p("foo.qux.baz");
        assert!(a.overlaps(&b));
        assert!(c.overlaps(&b));
        assert!(!a.overlaps(&c));
    }

    #[test]
    fn overlaps_deep_no_backtrack() {
        // 10-segment pattern × 10 literal segments. Should finish
        // instantly — the algorithm is O(min(|a|,|b|)) with no
        // exponential blow-up.
        let stars = p("*.*.*.*.*.*.*.*.*.*");
        let literals = p("a.b.c.d.e.f.g.h.i.j");
        assert!(stars.overlaps(&literals));
    }

    // ---- covers (asymmetric) ----

    #[test]
    fn covers_star_over_literal() {
        assert!(p("foo.*").covers(&p("foo.bar")));
        assert!(!p("foo.bar").covers(&p("foo.*")));
    }

    #[test]
    fn covers_gt_over_anything() {
        assert!(p("foo.>").covers(&p("foo.bar.baz.qux")));
        assert!(p(">").covers(&p("anything.at.all")));
    }

    #[test]
    fn covers_does_not_emit_when_only_overlap() {
        // The case that distinguishes covers from overlaps.
        assert!(!p("foo.*.baz").covers(&p("foo.bar.*")));
        assert!(!p("foo.bar.*").covers(&p("foo.*.baz")));
        // But they overlap.
        assert!(p("foo.*.baz").overlaps(&p("foo.bar.*")));
    }

    #[test]
    fn covers_self() {
        for s in &["foo.bar", "foo.*", "foo.>", ">"] {
            let pat = p(s);
            assert!(pat.covers(&pat));
        }
    }

    #[test]
    fn covers_implies_overlaps() {
        // If `a.covers(b)`, then `a` and `b` share at least one
        // concrete subject (specifically: every concrete subject of
        // `b`). So `a.overlaps(b)` must also hold.
        let pairs = [
            ("foo.*", "foo.bar"),
            ("foo.>", "foo.bar.baz"),
            (">", "x.y.z"),
            ("foo.*.*", "foo.bar.baz"),
        ];
        for (a_s, b_s) in &pairs {
            let a = p(a_s);
            let b = p(b_s);
            assert!(a.covers(&b), "{a_s} should cover {b_s}");
            assert!(
                a.overlaps(&b),
                "if {a_s} covers {b_s}, they must overlap too"
            );
        }
    }

    // ---- FromStr ----

    #[test]
    fn from_str_works() {
        let pat: NatsPattern = "foo.bar".parse().unwrap();
        assert_eq!(pat.as_str(), "foo.bar");
    }
}
