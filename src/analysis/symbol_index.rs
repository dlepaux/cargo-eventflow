//! Workspace-wide symbol table — the merged output of every
//! parse worker's per-file extraction.
//!
//! Holds `pub const` declarations, `impl T { fn nats_subject(...) }`
//! method bodies, and `fn build_breaker_subject(...)`-shaped free
//! functions, all keyed by their fully-qualified path
//! (`crate::module::Item`).
//!
//! Storing **source-string snippets**, not AST nodes, is
//! deliberate per [synthesis §P0-F][synth]: resolve re-parses
//! snippets on demand via `syn::parse_str`, which is ~5–10×
//! cheaper than retaining `syn::File` ASTs across the
//! parse-then-drop boundary.
//!
//! Build phase ordering (per synthesis §P0-F):
//!  1. Per-file workers emit `PerFileSymbols` and drop ASTs.
//!  2. The orchestrator calls [`SymbolIndex::merge`] once per
//!     worker output.
//!  3. Resolve phase queries via
//!     [`SymbolIndex::lookup_in_crate`] (same-crate-first) then
//!     [`SymbolIndex::lookup_in_helpers`].
//!
//! [synth]: see `gordon-workspace/plan/active/cargo-eventflow/synthesis.md`

use std::collections::BTreeMap;
use std::path::PathBuf;

/// Fully-qualified path to a symbol: `crate_name::module::item`.
///
/// v0.1 uses simple string equality. Cross-crate `use` aliases
/// are out of scope (declared limitation in synthesis §P1-A —
/// flagged with a warning, not silently followed).
pub type FqPath = String;

/// Source location for diagnostics.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Span {
    /// File path containing the symbol.
    pub file: PathBuf,
    /// 1-based line number.
    pub line: usize,
    /// 1-based column number.
    pub column: usize,
}

/// A snippet of Rust source, suitable for `syn::parse_str` at
/// resolve time. We store the source string instead of an AST
/// to drop AST memory between phases.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExprSnippet {
    /// Raw source bytes (e.g. the RHS of a `pub const`, or the
    /// body of a `fn`).
    pub source: String,
    /// Where it came from.
    pub span: Span,
}

/// A symbol resolvable from a call-site expression.
#[derive(Debug, Clone)]
pub enum Symbol {
    /// `pub const NAME: TYPE = EXPR;` — `expr` holds the RHS snippet.
    Const(ExprSnippet),
    /// A method on a type: `impl T { fn name(...) -> ... { BODY } }`.
    Method {
        /// The type the method is implemented on (string form, no resolution).
        type_path: String,
        /// Method body as a source snippet.
        body: ExprSnippet,
    },
    /// A free function: `fn name(...) -> ... { BODY }`.
    Function(ExprSnippet),
}

/// Per-file output from the parse phase. The orchestrator merges
/// these into a [`SymbolIndex`] in story 02; this struct is the
/// contract between phases.
#[derive(Debug, Clone, Default)]
pub struct PerFileSymbols {
    /// Crate name this file belongs to (needed for
    /// same-crate-first lookup).
    pub crate_name: String,
    /// `(FqPath, Symbol)` pairs extracted from the file.
    pub symbols: Vec<(FqPath, Symbol)>,
}

/// Workspace-wide symbol table.
///
/// Indexed by `(crate_name, FqPath)` so same-crate lookups don't
/// pollute the helper-crate namespace and vice-versa.
#[derive(Debug, Default)]
pub struct SymbolIndex {
    /// `(crate_name, FqPath) -> Symbol`. `BTreeMap` for deterministic
    /// iteration order — `HashMap` iteration is non-deterministic
    /// and would leak into emit byte-stability tests.
    by_crate: BTreeMap<(String, FqPath), Symbol>,
    /// Diagnostics surfaced during merge.
    diagnostics: Vec<MergeDiagnostic>,
}

/// Diagnostic emitted by [`SymbolIndex::merge`].
#[derive(Debug, Clone)]
pub enum MergeDiagnostic {
    /// Two crates defined the same `FqPath` with different bodies.
    /// First wins; this records the discrepancy for review.
    ///
    /// Real-world example: Gordon's `INTENTS_SUBJECT` is defined
    /// inline in both `gordon-bot` and `gordon-executor` — if the
    /// values diverge, that's a wire-contract bug worth flagging
    /// (see synthesis §H2 #4).
    DuplicateWithDifferentValue {
        /// The path that was defined twice.
        path: FqPath,
        /// Crate that defined it first (winning entry).
        first_crate: String,
        /// First-seen location.
        first_span: Span,
        /// Crate that re-defined it.
        second_crate: String,
        /// Second-seen location.
        second_span: Span,
    },
}

impl SymbolIndex {
    /// Construct an empty index.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Merge one parse worker's output into the index.
    ///
    /// Duplicate paths within the same crate use first-wins
    /// semantics (parse order is deterministic because the
    /// orchestrator sorts files before dispatching). Duplicates
    /// **across** crates with *different* values raise a
    /// [`MergeDiagnostic::DuplicateWithDifferentValue`] — same
    /// values are silently skipped (legitimate re-export pattern).
    pub fn merge(&mut self, per_file: PerFileSymbols) {
        for (path, sym) in per_file.symbols {
            let key = (per_file.crate_name.clone(), path.clone());

            // First, check for cross-crate dupes with diverging values.
            // We probe by FqPath ignoring the crate key, so duplicate
            // paths in *different* crates surface even when the
            // current `key` is novel.
            if let Some((existing_crate, existing_sym)) = self.find_by_path(&path) {
                if existing_crate != per_file.crate_name && !symbols_equal(&sym, existing_sym) {
                    self.diagnostics
                        .push(MergeDiagnostic::DuplicateWithDifferentValue {
                            path: path.clone(),
                            first_crate: existing_crate.to_string(),
                            first_span: symbol_span(existing_sym).clone(),
                            second_crate: per_file.crate_name.clone(),
                            second_span: symbol_span(&sym).clone(),
                        });
                }
            }

            // First-wins on (crate, path) keys; second insert is a no-op.
            self.by_crate.entry(key).or_insert(sym);
        }
    }

    /// Look up a symbol within a specific crate first (callsite's
    /// own crate).
    #[must_use]
    pub fn lookup_in_crate(&self, crate_name: &str, path: &str) -> Option<&Symbol> {
        self.by_crate
            .get(&(crate_name.to_string(), path.to_string()))
    }

    /// Look up across a set of helper crates (after same-crate
    /// lookup misses). Returns the first match in iteration order
    /// of `helpers` (so the user's config order is the
    /// precedence).
    #[must_use]
    pub fn lookup_in_helpers(&self, helpers: &[String], path: &str) -> Option<&Symbol> {
        for helper in helpers {
            if let Some(sym) = self.lookup_in_crate(helper, path) {
                return Some(sym);
            }
        }
        None
    }

    /// Snapshot of accumulated merge diagnostics.
    #[must_use]
    pub fn diagnostics(&self) -> &[MergeDiagnostic] {
        &self.diagnostics
    }

    /// Total symbol count — sanity check + metric.
    #[must_use]
    pub fn len(&self) -> usize {
        self.by_crate.len()
    }

    /// True if no symbols were merged.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.by_crate.is_empty()
    }

    /// Iterate over every `(FqPath, Symbol)` pair. Used by the
    /// resolver to scan for builder methods/functions by suffix
    /// match. Deterministic via [`BTreeMap`] iteration order.
    pub fn iter(&self) -> impl Iterator<Item = (&String, &Symbol)> {
        self.by_crate.iter().map(|((_, path), sym)| (path, sym))
    }

    fn find_by_path(&self, path: &str) -> Option<(&str, &Symbol)> {
        self.by_crate.iter().find_map(|((k_crate, k_path), sym)| {
            if k_path == path {
                Some((k_crate.as_str(), sym))
            } else {
                None
            }
        })
    }
}

fn symbol_span(sym: &Symbol) -> &Span {
    match sym {
        Symbol::Const(s) | Symbol::Function(s) => &s.span,
        Symbol::Method { body, .. } => &body.span,
    }
}

fn symbols_equal(a: &Symbol, b: &Symbol) -> bool {
    match (a, b) {
        (Symbol::Const(x), Symbol::Const(y)) | (Symbol::Function(x), Symbol::Function(y)) => {
            x.source == y.source
        }
        (
            Symbol::Method {
                type_path: tx,
                body: bx,
            },
            Symbol::Method {
                type_path: ty,
                body: by,
            },
        ) => tx == ty && bx.source == by.source,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn snip(source: &str, file: &str, line: usize) -> ExprSnippet {
        ExprSnippet {
            source: source.to_string(),
            span: Span {
                file: PathBuf::from(file),
                line,
                column: 1,
            },
        }
    }

    #[test]
    fn merge_then_lookup_same_crate() {
        let mut idx = SymbolIndex::new();
        idx.merge(PerFileSymbols {
            crate_name: "gordon-bot".into(),
            symbols: vec![(
                "gordon_bot::strategy_loop::emission::INTENTS_SUBJECT".into(),
                Symbol::Const(snip("\"intents.executor\"", "emission.rs", 12)),
            )],
        });

        let sym = idx
            .lookup_in_crate(
                "gordon-bot",
                "gordon_bot::strategy_loop::emission::INTENTS_SUBJECT",
            )
            .expect("same-crate lookup");
        match sym {
            Symbol::Const(s) => assert_eq!(s.source, "\"intents.executor\""),
            _ => panic!("expected Const"),
        }
    }

    #[test]
    fn lookup_misses_other_crate() {
        let mut idx = SymbolIndex::new();
        idx.merge(PerFileSymbols {
            crate_name: "gordon-bot".into(),
            symbols: vec![(
                "gordon_bot::s::INTENTS_SUBJECT".into(),
                Symbol::Const(snip("\"intents.executor\"", "emission.rs", 12)),
            )],
        });

        assert!(idx
            .lookup_in_crate("gordon-executor", "gordon_bot::s::INTENTS_SUBJECT")
            .is_none());
    }

    #[test]
    fn helper_lookup_searches_in_order() {
        let mut idx = SymbolIndex::new();
        idx.merge(PerFileSymbols {
            crate_name: "gordon-protocol".into(),
            symbols: vec![(
                "TRADING_FILLS_SUBJECT_PREFIX".into(),
                Symbol::Const(snip("\"trading.fills\"", "trading.rs", 10)),
            )],
        });
        idx.merge(PerFileSymbols {
            crate_name: "gordon-domain".into(),
            symbols: vec![(
                "OTHER_PREFIX".into(),
                Symbol::Const(snip("\"other\"", "domain.rs", 5)),
            )],
        });

        let helpers = vec!["gordon-protocol".to_string(), "gordon-domain".to_string()];

        let sym = idx
            .lookup_in_helpers(&helpers, "TRADING_FILLS_SUBJECT_PREFIX")
            .expect("helper lookup");
        match sym {
            Symbol::Const(s) => assert_eq!(s.source, "\"trading.fills\""),
            _ => panic!("expected Const"),
        }
    }

    #[test]
    fn duplicate_within_crate_first_wins_no_diagnostic() {
        let mut idx = SymbolIndex::new();
        idx.merge(PerFileSymbols {
            crate_name: "gordon-bot".into(),
            symbols: vec![
                ("DUP".into(), Symbol::Const(snip("\"first\"", "a.rs", 1))),
                ("DUP".into(), Symbol::Const(snip("\"second\"", "a.rs", 2))),
            ],
        });

        let sym = idx
            .lookup_in_crate("gordon-bot", "DUP")
            .expect("dup lookup");
        match sym {
            Symbol::Const(s) => assert_eq!(s.source, "\"first\""),
            _ => panic!("expected Const"),
        }
        assert!(idx.diagnostics().is_empty(), "no cross-crate dup");
    }

    #[test]
    fn duplicate_cross_crate_diverging_value_diagnostic() {
        let mut idx = SymbolIndex::new();
        idx.merge(PerFileSymbols {
            crate_name: "gordon-bot".into(),
            symbols: vec![(
                "INTENTS_SUBJECT".into(),
                Symbol::Const(snip("\"intents.executor\"", "emission.rs", 12)),
            )],
        });
        idx.merge(PerFileSymbols {
            crate_name: "gordon-executor".into(),
            symbols: vec![(
                "INTENTS_SUBJECT".into(),
                Symbol::Const(snip("\"intents.wrong\"", "consumer.rs", 7)),
            )],
        });

        let diags = idx.diagnostics();
        assert_eq!(diags.len(), 1);
        match &diags[0] {
            MergeDiagnostic::DuplicateWithDifferentValue {
                path,
                first_crate,
                second_crate,
                ..
            } => {
                assert_eq!(path, "INTENTS_SUBJECT");
                assert_eq!(first_crate, "gordon-bot");
                assert_eq!(second_crate, "gordon-executor");
            }
        }
    }

    #[test]
    fn duplicate_cross_crate_same_value_silent() {
        let mut idx = SymbolIndex::new();
        let snippet = || Symbol::Const(snip("\"intents.executor\"", "x.rs", 1));
        idx.merge(PerFileSymbols {
            crate_name: "gordon-bot".into(),
            symbols: vec![("INTENTS_SUBJECT".into(), snippet())],
        });
        idx.merge(PerFileSymbols {
            crate_name: "gordon-executor".into(),
            symbols: vec![("INTENTS_SUBJECT".into(), snippet())],
        });

        assert!(idx.diagnostics().is_empty(), "same value is fine");
    }

    #[test]
    fn iteration_order_deterministic() {
        // BTreeMap-backed: same insertion order, same iteration order.
        // (Property-test-shaped: build the index two ways, compare keys.)
        let build = |order: &[&str]| {
            let mut idx = SymbolIndex::new();
            for c in order {
                idx.merge(PerFileSymbols {
                    crate_name: (*c).to_string(),
                    symbols: vec![(format!("{c}::FOO"), Symbol::Const(snip("\"x\"", "a.rs", 1)))],
                });
            }
            idx.by_crate.keys().cloned().collect::<Vec<_>>()
        };

        let forward = build(&["alpha", "beta", "gamma"]);
        let reverse = build(&["gamma", "beta", "alpha"]);
        assert_eq!(
            forward, reverse,
            "BTreeMap key order is insertion-independent"
        );
    }

    #[test]
    fn len_and_is_empty() {
        let mut idx = SymbolIndex::new();
        assert!(idx.is_empty());
        idx.merge(PerFileSymbols {
            crate_name: "k".into(),
            symbols: vec![("p".into(), Symbol::Const(snip("\"v\"", "f.rs", 1)))],
        });
        assert_eq!(idx.len(), 1);
        assert!(!idx.is_empty());
    }
}
