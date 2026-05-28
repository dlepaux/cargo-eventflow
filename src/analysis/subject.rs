//! Subject resolution — turn an un-resolved expression snippet
//! (the `subject_expr` field on [`super::callsite::RawCallSite`])
//! into a concrete [`SubjectPattern`].
//!
//! Implements the bounded recursive
//! `normalize-and-resolve(expr, scope, depth_budget=3)` algorithm
//! from synthesis §H2 (review 01 finding F4). Five resolvers,
//! tried in order: literal, const path, builder method, builder
//! free-function, `format!` template.
//!
//! Per synthesis §P0-F: this module operates on **source-string
//! snippets** stored in the [`super::symbol_index::SymbolIndex`],
//! not on ASTs. Resolve re-parses snippets on demand via
//! `syn::parse_str`. Cost: ~1 ms per resolve call.

use std::collections::HashSet;

use syn::{Expr, ExprPath, Macro, Stmt};

use super::symbol_index::{Symbol, SymbolIndex};

/// Resolved subject pattern. Dynamic segments render as `*`
/// (single-segment) or `>` (tail). Fully-unresolvable segments
/// render as `?`.
pub type SubjectPattern = String;

/// Outcome of resolving one expression.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResolveOutcome {
    /// Subject fully recovered as a literal-shaped pattern.
    Resolved(SubjectPattern),
    /// At least one segment is dynamic (field access, runtime
    /// binding); rendered as `*` / `>` per
    /// [`SubjectPattern`] semantics.
    PartiallyResolved(SubjectPattern),
    /// The resolver gave up. Diagnostic carries the reason and
    /// the giving-up location.
    Unresolved(UnresolvedReason),
}

/// Why a resolve attempt failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UnresolvedReason {
    /// Recursion budget exhausted (chained consts > depth limit).
    DepthExhausted,
    /// Symbol not found in current crate or helper crates.
    SymbolNotFound {
        /// Name that was looked up.
        path: String,
    },
    /// Expression shape isn't one of the supported resolvers
    /// (closure, await, async block, struct literal, etc.).
    UnsupportedShape {
        /// Best-effort discriminator.
        kind: &'static str,
    },
    /// `syn::parse_str` rejected the snippet.
    ParseError {
        /// Stored snippet that failed to parse.
        snippet: String,
        /// Underlying error.
        message: String,
    },
}

/// Resolver scope — config-driven helper crates, methods, free
/// functions, plus the in-progress recursion guard.
#[derive(Debug, Clone)]
pub struct Scope<'a> {
    /// Crate the call site lives in (for same-crate-first lookup).
    pub current_crate: &'a str,
    /// Additional crates to search after the current one.
    pub helper_crates: &'a [String],
    /// Method names treated as subject builders
    /// (`["nats_subject"]`).
    pub subject_builder_methods: &'a [String],
    /// Free-function names treated as subject builders
    /// (`["build_breaker_subject"]`).
    pub subject_builder_functions: &'a [String],
    /// Optional local-scope source text for binding follow
    /// (the body of the function the call site sits inside).
    /// `None` disables binding follow.
    pub fn_body_source: Option<&'a str>,
}

impl<'a> Scope<'a> {
    /// Build a scope with no local fn body.
    #[must_use]
    pub const fn new(
        current_crate: &'a str,
        helper_crates: &'a [String],
        builder_methods: &'a [String],
        builder_functions: &'a [String],
    ) -> Self {
        Self {
            current_crate,
            helper_crates,
            subject_builder_methods: builder_methods,
            subject_builder_functions: builder_functions,
            fn_body_source: None,
        }
    }
}

/// Default recursion budget. Bounds worst-case work per call site
/// and prevents pathological loops on mutually-referencing consts.
///
/// A typical Gordon chain is 5 hops:
/// `call site → let binding → builder fn → format! arg → const → literal`.
/// Budget of 6 gives one hop of headroom for an extra adapter.
pub const DEFAULT_DEPTH: u8 = 6;

/// Resolve a subject expression snippet into a [`ResolveOutcome`].
///
/// # Errors
///
/// This function does not return `Result` — partial / failed
/// resolutions are encoded as [`ResolveOutcome::PartiallyResolved`]
/// / [`ResolveOutcome::Unresolved`]. The caller decides whether
/// to surface a diagnostic.
#[must_use]
pub fn resolve(snippet: &str, index: &SymbolIndex, scope: &Scope<'_>) -> ResolveOutcome {
    resolve_with_depth(snippet, index, scope, DEFAULT_DEPTH, &mut HashSet::new())
}

fn resolve_with_depth(
    snippet: &str,
    index: &SymbolIndex,
    scope: &Scope<'_>,
    depth: u8,
    visited: &mut HashSet<String>,
) -> ResolveOutcome {
    if depth == 0 {
        return ResolveOutcome::Unresolved(UnresolvedReason::DepthExhausted);
    }
    let expr = match syn::parse_str::<Expr>(snippet) {
        Ok(e) => e,
        Err(err) => {
            return ResolveOutcome::Unresolved(UnresolvedReason::ParseError {
                snippet: snippet.to_string(),
                message: err.to_string(),
            });
        }
    };
    resolve_expr(&expr, index, scope, depth, visited)
}

fn resolve_expr(
    expr: &Expr,
    index: &SymbolIndex,
    scope: &Scope<'_>,
    depth: u8,
    visited: &mut HashSet<String>,
) -> ResolveOutcome {
    // Normalisation pass: strip transparent adapters.
    let normalised = normalise(expr);

    match normalised {
        Expr::Lit(lit) => match &lit.lit {
            syn::Lit::Str(s) => ResolveOutcome::Resolved(s.value()),
            syn::Lit::ByteStr(b) => {
                ResolveOutcome::Resolved(String::from_utf8_lossy(&b.value()).to_string())
            }
            _ => ResolveOutcome::Unresolved(UnresolvedReason::UnsupportedShape {
                kind: "non-string literal",
            }),
        },
        Expr::Path(p) => resolve_path(p, index, scope, depth, visited),
        Expr::Macro(m) => resolve_macro(&m.mac, index, scope, depth, visited),
        Expr::MethodCall(mc) => {
            // Builder method (`event.nats_subject()`).
            let method_name = mc.method.to_string();
            if scope.subject_builder_methods.contains(&method_name) {
                resolve_builder_method(&method_name, index, scope, depth, visited)
            } else {
                ResolveOutcome::Unresolved(UnresolvedReason::UnsupportedShape {
                    kind: "non-builder method call",
                })
            }
        }
        Expr::Call(c) => {
            // Free-function call. If callee is a path, try to
            // resolve via subject_builder_functions (configured
            // whitelist) OR by looking the function up in the
            // symbol index directly. The latter catches local
            // helpers like `commands_subject(...)` without
            // forcing every project to enumerate them.
            let Expr::Path(p) = &*c.func else {
                return ResolveOutcome::Unresolved(UnresolvedReason::UnsupportedShape {
                    kind: "non-path function call",
                });
            };
            let last_segment = p
                .path
                .segments
                .last()
                .map(|s| s.ident.to_string())
                .unwrap_or_default();
            resolve_builder_function(&last_segment, index, scope, depth, visited)
        }
        Expr::Field(_) => {
            // Field access (`self.field`, `ctx.symbol`): runtime
            // value, render as `*` (single-segment dynamic).
            ResolveOutcome::PartiallyResolved("*".to_string())
        }
        _ => ResolveOutcome::Unresolved(UnresolvedReason::UnsupportedShape {
            kind: "other expression shape",
        }),
    }
}

/// Strip transparent adapters: `&inner`, `inner.to_owned()`,
/// `inner.to_string()`, `inner.clone()`, `inner.into()`,
/// `inner.as_str()`, `inner.as_ref()`. Tail-recursive.
fn normalise(expr: &Expr) -> &Expr {
    match expr {
        Expr::Reference(r) => normalise(&r.expr),
        Expr::Paren(p) => normalise(&p.expr),
        Expr::Group(g) => normalise(&g.expr),
        Expr::MethodCall(mc)
            if matches!(
                mc.method.to_string().as_str(),
                "to_owned" | "to_string" | "clone" | "into" | "as_str" | "as_ref"
            ) =>
        {
            normalise(&mc.receiver)
        }
        other => other,
    }
}

fn resolve_path(
    path_expr: &ExprPath,
    index: &SymbolIndex,
    scope: &Scope<'_>,
    depth: u8,
    visited: &mut HashSet<String>,
) -> ResolveOutcome {
    let path_string = path_to_string(&path_expr.path);
    let ident = path_expr
        .path
        .segments
        .last()
        .map(|s| s.ident.to_string())
        .unwrap_or_default();

    // 1. Try local binding in the current function body.
    if let Some(body) = scope.fn_body_source {
        if let Some(rhs) = find_let_binding(body, &ident) {
            let mut key = format!("binding:{ident}");
            if visited.insert(key.clone()) {
                let out = resolve_with_depth(&rhs, index, scope, depth - 1, visited);
                visited.remove(&key);
                if !matches!(
                    out,
                    ResolveOutcome::Unresolved(UnresolvedReason::SymbolNotFound { .. })
                ) {
                    return out;
                }
            }
            key = format!("binding:{ident}");
            visited.remove(&key);
        }
    }

    // 2. Same-crate const lookup.
    let same_crate_fq = format!("{}::{}", scope.current_crate.replace('-', "_"), path_string);
    let mut key = format!("const:{same_crate_fq}");
    if !visited.contains(&key) {
        if let Some(Symbol::Const(snippet)) =
            index.lookup_in_crate(scope.current_crate, &same_crate_fq)
        {
            visited.insert(key.clone());
            let out = resolve_with_depth(&snippet.source, index, scope, depth - 1, visited);
            visited.remove(&key);
            return out;
        }
        if let Some(Symbol::Const(snippet)) =
            index.lookup_in_crate(scope.current_crate, &path_string)
        {
            visited.insert(key.clone());
            let out = resolve_with_depth(&snippet.source, index, scope, depth - 1, visited);
            visited.remove(&key);
            return out;
        }
        // Also try lookups within nested modules of the current crate.
        if let Some((_, Symbol::Const(snippet))) =
            find_const_by_ident(index, scope.current_crate, &ident)
        {
            visited.insert(key.clone());
            let out = resolve_with_depth(&snippet.source, index, scope, depth - 1, visited);
            visited.remove(&key);
            return out;
        }
    }
    key = format!("const:{same_crate_fq}");
    visited.remove(&key);

    // 3. Helper-crate const lookup.
    for helper in scope.helper_crates {
        if let Some(Symbol::Const(snippet)) = index.lookup_in_crate(helper, &path_string) {
            let key = format!("helperconst:{helper}::{path_string}");
            if visited.insert(key.clone()) {
                let out = resolve_with_depth(&snippet.source, index, scope, depth - 1, visited);
                visited.remove(&key);
                return out;
            }
        }
        if let Some((_, Symbol::Const(snippet))) = find_const_by_ident(index, helper, &ident) {
            let key = format!("helperconst:{helper}::{ident}");
            if visited.insert(key.clone()) {
                let out = resolve_with_depth(&snippet.source, index, scope, depth - 1, visited);
                visited.remove(&key);
                return out;
            }
        }
    }

    ResolveOutcome::Unresolved(UnresolvedReason::SymbolNotFound { path: path_string })
}

fn resolve_macro(
    m: &Macro,
    index: &SymbolIndex,
    scope: &Scope<'_>,
    depth: u8,
    visited: &mut HashSet<String>,
) -> ResolveOutcome {
    let name = m
        .path
        .segments
        .last()
        .map(|s| s.ident.to_string())
        .unwrap_or_default();

    if name == "format" {
        return resolve_format_macro(&m.tokens.to_string(), index, scope, depth, visited);
    }
    if name == "concat" {
        return resolve_concat_macro(&m.tokens.to_string());
    }
    ResolveOutcome::Unresolved(UnresolvedReason::UnsupportedShape {
        kind: "non-format macro",
    })
}

/// Parse a `format!` token stream like
/// `"{}.{}", PREFIX, name.to_ascii_lowercase()` → resolve each
/// hole. Positional holes consume positional args in order;
/// named holes (`{ident}`) read the captured-identifier value
/// (Rust 2021 implicit capture). Resolved args splice their
/// value into the output; un-resolvable args render as `*`.
fn resolve_format_macro(
    tokens: &str,
    index: &SymbolIndex,
    scope: &Scope<'_>,
    depth: u8,
    visited: &mut HashSet<String>,
) -> ResolveOutcome {
    let tokens = tokens.trim_start();
    let Some(rest) = tokens.strip_prefix('"') else {
        return ResolveOutcome::Unresolved(UnresolvedReason::UnsupportedShape {
            kind: "format! without literal template",
        });
    };
    let Some(end) = rest.find('"') else {
        return ResolveOutcome::Unresolved(UnresolvedReason::UnsupportedShape {
            kind: "format! template unclosed",
        });
    };
    let template = &rest[..end];
    let after_template = rest[end + 1..].trim();

    // Split positional args by top-level commas (respecting paren/bracket nesting).
    // The `,` separator may be followed by arbitrary whitespace
    // because `quote::ToTokens` stringifies with single-space
    // padding between tokens. Trim aggressively.
    let arg_snippets =
        after_template
            .trim_start()
            .strip_prefix(',')
            .map_or_else(Vec::new, |args_str| {
                split_top_level_commas(args_str.trim())
                    .into_iter()
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .collect()
            });

    let mut out = String::with_capacity(template.len());
    let mut chars = template.chars().peekable();
    let mut in_hole = false;
    let mut hole_buf = String::new();
    let mut positional_idx = 0usize;
    let mut had_dynamic = false;

    while let Some(c) = chars.next() {
        match c {
            '{' if chars.peek() == Some(&'{') => {
                chars.next();
                out.push('{');
            }
            '}' if chars.peek() == Some(&'}') => {
                chars.next();
                out.push('}');
            }
            '{' => {
                in_hole = true;
                hole_buf.clear();
            }
            '}' if in_hole => {
                in_hole = false;
                // Strip format-spec: `{name:?}` → `name`.
                let spec_start = hole_buf.find(':');
                let name_part = spec_start.map_or(hole_buf.as_str(), |i| &hole_buf[..i]);
                let resolved = if name_part.is_empty() {
                    // Positional: consume next arg.
                    let arg = arg_snippets.get(positional_idx).map(String::as_str);
                    positional_idx += 1;
                    arg.map(|s| resolve_arg_snippet(s, index, scope, depth, visited))
                } else if name_part.chars().all(|c| c.is_ascii_digit()) {
                    // Numeric index: `{0}`, `{1}`.
                    let idx: usize = name_part.parse().unwrap_or(usize::MAX);
                    arg_snippets
                        .get(idx)
                        .map(|s| resolve_arg_snippet(s, index, scope, depth, visited))
                } else {
                    // Named: try the named-arg form `name = expr` in args list,
                    // then fall back to implicit capture (resolve as a path).
                    let named = arg_snippets
                        .iter()
                        .find_map(|s| {
                            let s = s.trim();
                            if let Some(eq) = s.find('=') {
                                let (lhs, rhs) = s.split_at(eq);
                                if lhs.trim() == name_part {
                                    return Some(rhs.trim_start_matches('=').to_string());
                                }
                            }
                            None
                        })
                        .or_else(|| Some(name_part.to_string()));
                    named.map(|s| resolve_arg_snippet(&s, index, scope, depth, visited))
                };

                match resolved {
                    Some(ResolveOutcome::Resolved(s)) => out.push_str(&s),
                    Some(ResolveOutcome::PartiallyResolved(s)) => {
                        had_dynamic = true;
                        out.push_str(&s);
                    }
                    _ => {
                        had_dynamic = true;
                        out.push('*');
                    }
                }
            }
            _ if in_hole => hole_buf.push(c),
            _ => out.push(c),
        }
    }

    if had_dynamic {
        ResolveOutcome::PartiallyResolved(out)
    } else {
        ResolveOutcome::Resolved(out)
    }
}

fn resolve_arg_snippet(
    snippet: &str,
    index: &SymbolIndex,
    scope: &Scope<'_>,
    depth: u8,
    visited: &mut HashSet<String>,
) -> ResolveOutcome {
    let trimmed = snippet.trim();
    if trimmed.is_empty() {
        return ResolveOutcome::PartiallyResolved("*".into());
    }
    resolve_with_depth(trimmed, index, scope, depth, visited)
}

/// Split `s` on top-level commas (depth-0 paren/bracket/brace nesting).
fn split_top_level_commas(s: &str) -> Vec<String> {
    let mut parts = Vec::new();
    let mut depth = 0i32;
    let mut start = 0usize;
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let c = bytes[i] as char;
        match c {
            '(' | '[' | '{' => depth += 1,
            ')' | ']' | '}' => depth -= 1,
            ',' if depth == 0 => {
                parts.push(s[start..i].to_string());
                start = i + 1;
            }
            _ => {}
        }
        i += 1;
    }
    if start < bytes.len() {
        parts.push(s[start..].to_string());
    }
    parts
}

fn resolve_concat_macro(tokens: &str) -> ResolveOutcome {
    // `concat!("a", ".", "b")` → "a.b". String literals only.
    let mut out = String::new();
    let mut chars = tokens.chars();
    while let Some(c) = chars.next() {
        if c == '"' {
            for next in chars.by_ref() {
                if next == '"' {
                    break;
                }
                out.push(next);
            }
        }
    }
    if out.is_empty() {
        ResolveOutcome::Unresolved(UnresolvedReason::UnsupportedShape {
            kind: "concat! with non-literal args",
        })
    } else {
        ResolveOutcome::Resolved(out)
    }
}

fn resolve_builder_method(
    method_name: &str,
    index: &SymbolIndex,
    scope: &Scope<'_>,
    depth: u8,
    visited: &mut HashSet<String>,
) -> ResolveOutcome {
    // Find any impl method matching the name in helper crates
    // or current crate. The first body's format! template wins.
    let candidates = collect_methods_named(index, method_name);
    for body_snippet in candidates {
        let key = format!("method:{method_name}");
        if !visited.insert(key.clone()) {
            continue;
        }
        let outcome = extract_format_from_body(&body_snippet, index, scope, depth - 1, visited);
        visited.remove(&key);
        if !matches!(outcome, ResolveOutcome::Unresolved(_)) {
            return outcome;
        }
    }
    ResolveOutcome::Unresolved(UnresolvedReason::SymbolNotFound {
        path: method_name.to_string(),
    })
}

fn resolve_builder_function(
    fn_name: &str,
    index: &SymbolIndex,
    scope: &Scope<'_>,
    depth: u8,
    visited: &mut HashSet<String>,
) -> ResolveOutcome {
    let candidates = collect_functions_named(index, fn_name);
    for body_snippet in candidates {
        let key = format!("fn:{fn_name}");
        if !visited.insert(key.clone()) {
            continue;
        }
        let outcome = extract_format_from_body(&body_snippet, index, scope, depth - 1, visited);
        visited.remove(&key);
        if !matches!(outcome, ResolveOutcome::Unresolved(_)) {
            return outcome;
        }
    }
    ResolveOutcome::Unresolved(UnresolvedReason::SymbolNotFound {
        path: fn_name.to_string(),
    })
}

/// Scan a method/fn body's source for the first `format!` macro
/// or string literal that looks like a subject template, and
/// resolve it. Heuristic but matches every Gordon builder shape
/// (`format!("{}.{}", PREFIX, name)`).
fn extract_format_from_body(
    body: &str,
    index: &SymbolIndex,
    scope: &Scope<'_>,
    depth: u8,
    visited: &mut HashSet<String>,
) -> ResolveOutcome {
    let Ok(block) = syn::parse_str::<syn::Block>(body) else {
        return ResolveOutcome::Unresolved(UnresolvedReason::ParseError {
            snippet: body.to_string(),
            message: "body not a Block".into(),
        });
    };

    // Walk statements: the last expression (or first `format!`)
    // is the body's return value.
    let mut last_expr_resolution: Option<ResolveOutcome> = None;
    for stmt in &block.stmts {
        if let Stmt::Expr(expr, _) = stmt {
            let outcome = resolve_expr(expr, index, scope, depth, visited);
            last_expr_resolution = Some(outcome);
        }
    }

    last_expr_resolution.unwrap_or(ResolveOutcome::Unresolved(
        UnresolvedReason::UnsupportedShape {
            kind: "body without return expression",
        },
    ))
}

fn collect_methods_named(index: &SymbolIndex, name: &str) -> Vec<String> {
    let mut out = Vec::new();
    for (_path, sym) in iter_symbols(index) {
        if let Symbol::Method { body, .. } = sym {
            // The FqPath ends in `::TypeName::method_name`.
            // We don't have direct access to method-name parts
            // from Symbol, so we filter by checking the path's
            // last segment matches `name`.
            // (Skipped here — handled at iter level by path suffix.)
            let _ = body;
        }
    }
    for (path, sym) in iter_symbols(index) {
        if let Symbol::Method { body, .. } = sym {
            if path.ends_with(&format!("::{name}")) {
                out.push(body.source.clone());
            }
        }
    }
    out
}

fn collect_functions_named(index: &SymbolIndex, name: &str) -> Vec<String> {
    let mut out = Vec::new();
    for (path, sym) in iter_symbols(index) {
        if let Symbol::Function(body) = sym {
            if path.ends_with(&format!("::{name}")) {
                out.push(body.source.clone());
            }
        }
    }
    out
}

fn find_const_by_ident<'a>(
    index: &'a SymbolIndex,
    crate_name: &str,
    ident: &str,
) -> Option<(&'a str, &'a Symbol)> {
    for (path, sym) in iter_symbols(index) {
        if let Symbol::Const(_) = sym {
            if path.ends_with(&format!("::{ident}")) {
                if let Some(sym_in_crate) = index.lookup_in_crate(crate_name, path) {
                    return Some((path, sym_in_crate));
                }
            }
        }
    }
    None
}

/// Iterate all symbols. Exposed via a public method on `SymbolIndex`
/// would be cleaner; this is a workspace-internal helper.
fn iter_symbols(index: &SymbolIndex) -> impl Iterator<Item = (&String, &Symbol)> {
    index.iter()
}

fn path_to_string(path: &syn::Path) -> String {
    path.segments
        .iter()
        .map(|s| s.ident.to_string())
        .collect::<Vec<_>>()
        .join("::")
}

/// Find `let <ident> = <rhs>;` in a function body source string.
/// Best-effort scan: splits on `;` then matches the `let` prefix.
/// Handles both single-line and multi-line bodies. v0.2 could
/// parse to `syn::Block` for accuracy + nested-block awareness.
fn find_let_binding(body: &str, ident: &str) -> Option<String> {
    let prefix = format!("let {ident} =");
    let prefix_mut = format!("let mut {ident} =");
    for stmt in body.split(';') {
        let trimmed = stmt.trim_start_matches(|c: char| c.is_whitespace() || c == '{');
        let trimmed = trimmed.trim();
        let rhs = if let Some(r) = trimmed.strip_prefix(&prefix) {
            r
        } else if let Some(r) = trimmed.strip_prefix(&prefix_mut) {
            r
        } else {
            continue;
        };
        return Some(rhs.trim().to_string());
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::analysis::symbol_index::{ExprSnippet, PerFileSymbols, Span};
    use std::path::PathBuf;

    fn snip(source: &str) -> ExprSnippet {
        ExprSnippet {
            source: source.to_string(),
            span: Span {
                file: PathBuf::from("test.rs"),
                line: 1,
                column: 1,
            },
        }
    }

    fn build_index_with(symbols: Vec<(&str, &str, Symbol)>) -> SymbolIndex {
        let mut idx = SymbolIndex::new();
        for (crate_name, path, sym) in symbols {
            idx.merge(PerFileSymbols {
                crate_name: crate_name.to_string(),
                symbols: vec![(path.to_string(), sym)],
            });
        }
        idx
    }

    fn helpers() -> Vec<String> {
        vec!["gordon-protocol".to_string()]
    }

    fn scope_no_body<'a>(
        helpers: &'a [String],
        builders: &'a [String],
        fns: &'a [String],
    ) -> Scope<'a> {
        Scope::new("test-crate", helpers, builders, fns)
    }

    #[test]
    fn resolve_literal() {
        let idx = SymbolIndex::new();
        let h = helpers();
        let bm = vec!["nats_subject".to_string()];
        let bf = vec![];
        let scope = scope_no_body(&h, &bm, &bf);
        let out = resolve("\"foo.bar\"", &idx, &scope);
        assert_eq!(out, ResolveOutcome::Resolved("foo.bar".into()));
    }

    #[test]
    fn resolve_reference_strips_amp() {
        let idx = SymbolIndex::new();
        let h = helpers();
        let bm = vec![];
        let bf = vec![];
        let scope = scope_no_body(&h, &bm, &bf);
        let out = resolve("&\"foo.bar\"", &idx, &scope);
        assert_eq!(out, ResolveOutcome::Resolved("foo.bar".into()));
    }

    #[test]
    fn resolve_to_owned_strips() {
        let idx = SymbolIndex::new();
        let h = helpers();
        let bm = vec![];
        let bf = vec![];
        let scope = scope_no_body(&h, &bm, &bf);
        let out = resolve("\"foo\".to_owned()", &idx, &scope);
        assert_eq!(out, ResolveOutcome::Resolved("foo".into()));
    }

    #[test]
    fn resolve_const_in_helper_crate() {
        let idx = build_index_with(vec![(
            "gordon-protocol",
            "TRADING_FILLS_SUBJECT",
            Symbol::Const(snip("\"trading.fills.bot\"")),
        )]);
        let h = helpers();
        let bm = vec![];
        let bf = vec![];
        let scope = scope_no_body(&h, &bm, &bf);
        let out = resolve("TRADING_FILLS_SUBJECT", &idx, &scope);
        assert_eq!(out, ResolveOutcome::Resolved("trading.fills.bot".into()));
    }

    #[test]
    fn resolve_const_in_same_crate_first() {
        // Same const name in both crates — same-crate wins.
        let idx = build_index_with(vec![
            (
                "test-crate",
                "X",
                Symbol::Const(snip("\"same-crate.value\"")),
            ),
            (
                "gordon-protocol",
                "X",
                Symbol::Const(snip("\"helper.value\"")),
            ),
        ]);
        let h = helpers();
        let bm = vec![];
        let bf = vec![];
        let scope = scope_no_body(&h, &bm, &bf);
        let out = resolve("X", &idx, &scope);
        assert_eq!(
            out,
            ResolveOutcome::Resolved("same-crate.value".into()),
            "same-crate-first per synthesis §H2 #3"
        );
    }

    #[test]
    fn resolve_format_macro_with_holes() {
        let idx = SymbolIndex::new();
        let h = helpers();
        let bm = vec![];
        let bf = vec![];
        let scope = scope_no_body(&h, &bm, &bf);
        let out = resolve(
            "format!(\"market.klines.binance.{market}.{symbol_lc}.1m\")",
            &idx,
            &scope,
        );
        assert_eq!(
            out,
            ResolveOutcome::PartiallyResolved("market.klines.binance.*.*.1m".into())
        );
    }

    #[test]
    fn resolve_format_macro_literal_only() {
        let idx = SymbolIndex::new();
        let h = helpers();
        let bm = vec![];
        let bf = vec![];
        let scope = scope_no_body(&h, &bm, &bf);
        let out = resolve("format!(\"foo.bar\")", &idx, &scope);
        assert_eq!(out, ResolveOutcome::Resolved("foo.bar".into()));
    }

    #[test]
    fn resolve_builder_function() {
        let idx = build_index_with(vec![(
            "gordon-risk",
            "gordon_risk::bus::build_breaker_subject",
            Symbol::Function(snip(
                "{ format!(\"{}.{}\", RISK_EVENTS_SUBJECT_PREFIX, breaker_name.to_ascii_lowercase()) }",
            )),
        )]);
        let h = vec!["gordon-risk".to_string()];
        let bm = vec![];
        let bf = vec!["build_breaker_subject".to_string()];
        let scope = scope_no_body(&h, &bm, &bf);
        let out = resolve("build_breaker_subject(name)", &idx, &scope);
        // 2 holes → 2 wildcards joined by literal dot.
        assert_eq!(out, ResolveOutcome::PartiallyResolved("*.*".into()));
    }

    #[test]
    fn resolve_builder_method() {
        let idx = build_index_with(vec![(
            "gordon-domain",
            "FillEvent::nats_subject",
            Symbol::Method {
                type_path: "FillEvent".into(),
                body: snip("{ format!(\"trading.fills.{}\", self.bot_id) }"),
            },
        )]);
        let h = vec!["gordon-domain".to_string()];
        let bm = vec!["nats_subject".to_string()];
        let bf = vec![];
        let scope = scope_no_body(&h, &bm, &bf);
        let out = resolve("event.nats_subject()", &idx, &scope);
        assert_eq!(
            out,
            ResolveOutcome::PartiallyResolved("trading.fills.*".into())
        );
    }

    #[test]
    fn resolve_depth_exhaustion_on_cycle() {
        // Cycle: A → B → A. Bounded budget terminates with
        // DepthExhausted, not infinite loop.
        let idx = build_index_with(vec![
            ("test-crate", "A", Symbol::Const(snip("B"))),
            ("test-crate", "B", Symbol::Const(snip("A"))),
        ]);
        let h = helpers();
        let bm = vec![];
        let bf = vec![];
        let scope = scope_no_body(&h, &bm, &bf);
        let out = resolve("A", &idx, &scope);
        // Either visited-set short-circuits to SymbolNotFound,
        // or depth runs out. Both are acceptable terminators.
        assert!(
            matches!(out, ResolveOutcome::Unresolved(_)),
            "cycle must terminate as Unresolved, got {out:?}"
        );
    }

    #[test]
    fn resolve_unknown_symbol_returns_not_found() {
        let idx = SymbolIndex::new();
        let h = helpers();
        let bm = vec![];
        let bf = vec![];
        let scope = scope_no_body(&h, &bm, &bf);
        let out = resolve("UNKNOWN_SUBJECT", &idx, &scope);
        assert!(matches!(
            out,
            ResolveOutcome::Unresolved(UnresolvedReason::SymbolNotFound { .. })
        ));
    }

    #[test]
    fn resolve_field_access_is_partial_star() {
        let idx = SymbolIndex::new();
        let h = helpers();
        let bm = vec![];
        let bf = vec![];
        let scope = scope_no_body(&h, &bm, &bf);
        let out = resolve("self.subject_field", &idx, &scope);
        assert_eq!(out, ResolveOutcome::PartiallyResolved("*".into()));
    }

    #[test]
    fn local_binding_follow_via_fn_body() {
        let idx = build_index_with(vec![(
            "test-crate",
            "INTENTS_SUBJECT",
            Symbol::Const(snip("\"intents.executor\"")),
        )]);
        let h = vec![];
        let bm = vec![];
        let bf = vec![];
        let body = "{ let subject = INTENTS_SUBJECT; let trace_uuid = ok(); publish(&subject); }";
        let mut scope = scope_no_body(&h, &bm, &bf);
        scope.fn_body_source = Some(body);
        let out = resolve("&subject", &idx, &scope);
        assert_eq!(out, ResolveOutcome::Resolved("intents.executor".into()));
    }
}
