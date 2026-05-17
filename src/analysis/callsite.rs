//! Call-site extraction — walk a parsed `syn::File`, find every
//! `publish` / `subscribe` call that matches a configured
//! [`PublisherSpec`] / [`ConsumerSpec`], filter aggressively to
//! avoid false positives (Gordon-shaped codebases see ~92%
//! false-positive rate with bare name matching — see synthesis
//! §H1), and emit raw `RawCallSite` records plus per-file
//! symbols for [`super::symbol_index::SymbolIndex`].
//!
//! AST is **dropped** at end of [`extract`]; output is pure
//! source-string snippets + spans. This is phase 2 of the
//! two-phase parse-then-drop architecture (synthesis §P0-F).
//!
//! Subject expressions are returned as **un-resolved snippets**
//! — story 03 ([`super::subject`]) consumes the snippets and
//! turns them into `SubjectPattern`s.

use std::path::{Path, PathBuf};

use syn::visit::Visit;
use syn::{Expr, ExprCall, ExprMethodCall, ImplItem, Item, ItemConst, ItemFn, ItemImpl, ItemUse};

use super::symbol_index::{ExprSnippet, FqPath, PerFileSymbols, Span, Symbol};

/// How loosely we match method names.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Deserialize)]
pub enum MethodMatch {
    /// Match by method name only. False-positive prone; use for
    /// rare cases where trait imports are obscured by re-exports.
    #[serde(rename = "name")]
    Name,
    /// Method name + a matching `use` statement somewhere in the
    /// file. **Default per synthesis §H1.** Without this, Gordon
    /// emits 195 false positives against 11 ground-truth sites.
    #[default]
    #[serde(rename = "name+trait_path_hint")]
    NameTraitPathHint,
}

/// Trait vs inherent method scoping.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PublisherKind {
    /// Trait method: receiver is `&self` (implicitly typed).
    /// Match via trait `use` statement.
    Trait,
    /// Inherent method on a concrete type. Match via the
    /// type's last path segment on the receiver.
    Inherent,
}

fn default_subject_arg_index() -> usize {
    0
}

/// Configured publish call shape.
#[derive(Debug, Clone, serde::Deserialize)]
pub struct PublisherSpec {
    /// Trait or inherent scoping.
    pub kind: PublisherKind,
    /// Trait path (for `Trait` kind): e.g. `"my_bus::Publisher"`.
    #[serde(default)]
    pub path: Option<String>,
    /// Type path (for `Inherent` kind): e.g.
    /// `"my_bus::nats::NatsPublisher"`. Renamed to `type` in TOML
    /// to keep the config keys terse.
    #[serde(default, rename = "type")]
    pub type_path: Option<String>,
    /// Method name: e.g. `"publish"`, `"publish_within"`.
    pub method: String,
    /// 0-based index of the subject argument
    /// (after the receiver). Default 0; inherent outbox
    /// methods taking `&mut Tx` first use 1.
    #[serde(default = "default_subject_arg_index")]
    pub subject_arg_index: usize,
}

/// Configured subscribe call shape.
#[derive(Debug, Clone, serde::Deserialize)]
pub struct ConsumerSpec {
    /// Trait or inherent scoping.
    pub kind: PublisherKind,
    /// Trait path (for `Trait` kind).
    #[serde(default)]
    pub path: Option<String>,
    /// Type path (for `Inherent` kind).
    #[serde(default, rename = "type")]
    pub type_path: Option<String>,
    /// Method name: e.g. `"subscribe"`.
    pub method: String,
    /// 0-based index of the subject argument.
    #[serde(default = "default_subject_arg_index")]
    pub subject_arg_index: usize,
    /// Optional 0-based index of the consumer-name argument.
    #[serde(default)]
    pub consumer_name_arg_index: Option<usize>,
}

/// Composite config for the extractor.
#[derive(Debug, Clone, Default)]
pub struct CallSiteConfig {
    /// How aggressively to filter method-name matches.
    pub method_match: MethodMatch,
    /// Configured publishers.
    pub publishers: Vec<PublisherSpec>,
    /// Configured consumers.
    pub consumers: Vec<ConsumerSpec>,
}

/// Distinguishes publish from subscribe in [`RawCallSite`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CallKind {
    /// Publish call (one of the [`PublisherSpec`]s matched).
    Publish,
    /// Subscribe call (one of the [`ConsumerSpec`]s matched).
    Subscribe,
}

/// One un-resolved call site: a publish/subscribe call we matched
/// in source, with its subject-arg expression snippet for the
/// resolver to crack open in story 03.
#[derive(Debug, Clone)]
pub struct RawCallSite {
    /// Publish or subscribe.
    pub kind: CallKind,
    /// Path to the source file.
    pub file: PathBuf,
    /// 1-based line number of the call expression.
    pub line: usize,
    /// 1-based column number.
    pub column: usize,
    /// Source string of the subject-arg expression
    /// (e.g. `"&subject"`, `"INTENTS_SUBJECT"`,
    /// `"event.nats_subject()"`).
    pub subject_expr: String,
    /// Source string of the durable consumer-name arg (subscribe
    /// only), if the spec declared the arg index.
    pub consumer_name_expr: Option<String>,
    /// Which configured method matched (for diagnostics).
    pub matched_method: String,
    /// Source of the enclosing function body, if any. Used by the
    /// resolver to follow `let subject = ...` bindings. `None`
    /// when the call site is at module scope.
    pub enclosing_fn_body: Option<String>,
}

/// Output of [`extract`]: call sites + symbol-index contributions.
#[derive(Debug, Clone, Default)]
pub struct PerFileCallSites {
    /// Detected call sites, sorted by `(line, column)`.
    pub sites: Vec<RawCallSite>,
    /// Per-file symbol contributions to merge into the workspace
    /// [`super::symbol_index::SymbolIndex`].
    pub symbols: PerFileSymbols,
}

/// Parse errors surfaced by [`extract`].
#[derive(Debug, thiserror::Error)]
pub enum ParseError {
    /// `syn::parse_file` failed.
    #[error("syn parse error in {file}: {message}")]
    Syn {
        /// File path that failed.
        file: PathBuf,
        /// Underlying syn error message.
        message: String,
    },
}

/// Walk `source` (Rust source code from `file`, owned by
/// `crate_name`), extract publish/subscribe call sites + symbols
/// per `config`. **The parsed AST is dropped before this returns.**
///
/// # Errors
///
/// Returns [`ParseError::Syn`] if `syn::parse_file` rejects the
/// source. Per synthesis §P0-E, the orchestrator's policy is
/// "warn + skip" on per-file parse errors; `--strict-parse`
/// escalates.
pub fn extract(
    file: &Path,
    source: &str,
    crate_name: &str,
    config: &CallSiteConfig,
) -> Result<PerFileCallSites, ParseError> {
    let parsed = syn::parse_file(source).map_err(|err| ParseError::Syn {
        file: file.to_path_buf(),
        message: err.to_string(),
    })?;

    let imports = collect_imports(&parsed.items);

    let mut visitor = Visitor {
        file: file.to_path_buf(),
        crate_name: crate_name.to_string(),
        config,
        imports: &imports,
        module_stack: Vec::new(),
        impl_type_stack: Vec::new(),
        fn_body_stack: Vec::new(),
        sites: Vec::new(),
        symbols: PerFileSymbols {
            crate_name: crate_name.to_string(),
            symbols: Vec::new(),
        },
    };
    visitor.visit_file(&parsed);

    let mut sites = visitor.sites;
    sites.sort_by_key(|s| (s.line, s.column));

    let symbols = visitor.symbols;

    // `parsed` drops here.
    Ok(PerFileCallSites { sites, symbols })
}

/// Collect every `use foo::bar::Baz;` path string in the file.
/// Used by the import-hint matcher.
fn collect_imports(items: &[Item]) -> Vec<String> {
    let mut out = Vec::new();
    for item in items {
        if let Item::Use(item_use) = item {
            collect_use_paths(item_use, &mut out);
        }
    }
    out
}

fn collect_use_paths(item: &ItemUse, out: &mut Vec<String>) {
    let mut prefix = String::new();
    walk_use_tree(&item.tree, &mut prefix, out);
}

fn walk_use_tree(tree: &syn::UseTree, prefix: &mut String, out: &mut Vec<String>) {
    use syn::UseTree;
    match tree {
        UseTree::Path(p) => {
            let saved = prefix.len();
            if !prefix.is_empty() {
                prefix.push_str("::");
            }
            prefix.push_str(&p.ident.to_string());
            walk_use_tree(&p.tree, prefix, out);
            prefix.truncate(saved);
        }
        UseTree::Name(n) => {
            let mut full = prefix.clone();
            if !full.is_empty() {
                full.push_str("::");
            }
            full.push_str(&n.ident.to_string());
            out.push(full);
        }
        UseTree::Rename(r) => {
            let mut full = prefix.clone();
            if !full.is_empty() {
                full.push_str("::");
            }
            full.push_str(&r.ident.to_string());
            out.push(full);
        }
        UseTree::Glob(_) => {
            let mut full = prefix.clone();
            full.push_str("::*");
            out.push(full);
        }
        UseTree::Group(g) => {
            for item in &g.items {
                walk_use_tree(item, prefix, out);
            }
        }
    }
}

struct Visitor<'a> {
    file: PathBuf,
    crate_name: String,
    config: &'a CallSiteConfig,
    imports: &'a [String],
    module_stack: Vec<String>,
    impl_type_stack: Vec<String>,
    fn_body_stack: Vec<String>,
    sites: Vec<RawCallSite>,
    symbols: PerFileSymbols,
}

impl Visitor<'_> {
    /// Match a method-call site `recv.method(args)`.
    fn try_match_method_call(&mut self, call: &ExprMethodCall) {
        let method_name = call.method.to_string();

        // Trait-shape matchers: `recv.publish(args)` where the
        // trait was imported in this file.
        for spec in &self.config.publishers {
            if matches_publisher_method(spec, &method_name, call, self.imports) {
                if let Some(site) = self.build_publish_site(spec, &call.args) {
                    self.sites.push(site);
                    return;
                }
            }
        }
        for spec in &self.config.consumers {
            if matches_consumer_method(spec, &method_name, call, self.imports) {
                if let Some(site) = self.build_subscribe_site(spec, &call.args) {
                    self.sites.push(site);
                    return;
                }
            }
        }

        // Inherent-shape via method call: `publisher.publish_full(args)`
        // where `publisher: NatsPublisher` and `use ...NatsPublisher`
        // is present. Receiver type isn't statically known without
        // type resolution; the import hint is the discriminator.
        for spec in &self.config.publishers {
            if spec.kind == PublisherKind::Inherent
                && spec.method == method_name
                && matches_trait_import(spec.type_path.as_deref(), self.imports)
            {
                if let Some(site) = self.build_publish_site(spec, &call.args) {
                    self.sites.push(site);
                    return;
                }
            }
        }
        for spec in &self.config.consumers {
            if spec.kind == PublisherKind::Inherent
                && spec.method == method_name
                && matches_trait_import(spec.type_path.as_deref(), self.imports)
            {
                if let Some(site) = self.build_subscribe_site(spec, &call.args) {
                    self.sites.push(site);
                    return;
                }
            }
        }
    }

    /// Match an associated-function call site
    /// `Type::method(recv, args)` (the inherent-method-with-explicit-receiver
    /// shape that `NatsPublisher::publish_within(&mut tx, ...)` uses).
    fn try_match_call(&mut self, call: &ExprCall) {
        let Some((type_segment, method_name)) = associated_fn_segments(&call.func) else {
            return;
        };

        for spec in &self.config.publishers {
            if spec.kind == PublisherKind::Inherent
                && spec.method == method_name
                && matches_type_path(
                    spec.type_path.as_deref(),
                    &type_segment,
                    self.imports,
                    self.config.method_match,
                )
            {
                if let Some(site) = self.build_publish_call_site(spec, &call.args) {
                    self.sites.push(site);
                    return;
                }
            }
        }
        for spec in &self.config.consumers {
            if spec.kind == PublisherKind::Inherent
                && spec.method == method_name
                && matches_type_path(
                    spec.type_path.as_deref(),
                    &type_segment,
                    self.imports,
                    self.config.method_match,
                )
            {
                if let Some(site) = self.build_subscribe_call_site(spec, &call.args) {
                    self.sites.push(site);
                    return;
                }
            }
        }
    }

    fn build_publish_site(
        &self,
        spec: &PublisherSpec,
        args: &syn::punctuated::Punctuated<Expr, syn::Token![,]>,
    ) -> Option<RawCallSite> {
        // For method-call shape, `&self` is implicit — args list starts
        // at user-supplied arg 0.
        let subject_expr = args.iter().nth(spec.subject_arg_index)?;
        if !is_string_shaped(subject_expr) {
            return None;
        }
        let (line, column) = expr_span(subject_expr);
        Some(RawCallSite {
            kind: CallKind::Publish,
            file: self.file.clone(),
            line,
            column,
            subject_expr: expr_to_source(subject_expr),
            consumer_name_expr: None,
            matched_method: spec.method.clone(),
            enclosing_fn_body: self.fn_body_stack.last().cloned(),
        })
    }

    fn build_subscribe_site(
        &self,
        spec: &ConsumerSpec,
        args: &syn::punctuated::Punctuated<Expr, syn::Token![,]>,
    ) -> Option<RawCallSite> {
        let subject_expr = args.iter().nth(spec.subject_arg_index)?;
        if !is_string_shaped(subject_expr) {
            return None;
        }
        let consumer_name_expr = spec
            .consumer_name_arg_index
            .and_then(|idx| args.iter().nth(idx))
            .map(expr_to_source);
        let (line, column) = expr_span(subject_expr);
        Some(RawCallSite {
            kind: CallKind::Subscribe,
            file: self.file.clone(),
            line,
            column,
            subject_expr: expr_to_source(subject_expr),
            consumer_name_expr,
            matched_method: spec.method.clone(),
            enclosing_fn_body: self.fn_body_stack.last().cloned(),
        })
    }

    fn build_publish_call_site(
        &self,
        spec: &PublisherSpec,
        args: &syn::punctuated::Punctuated<Expr, syn::Token![,]>,
    ) -> Option<RawCallSite> {
        // For associated-function shape, the receiver (if any) is
        // arg 0 in the source. `subject_arg_index` is the index in
        // the full arg list (as the config writer sees them).
        let subject_expr = args.iter().nth(spec.subject_arg_index)?;
        if !is_string_shaped(subject_expr) {
            return None;
        }
        let (line, column) = expr_span(subject_expr);
        Some(RawCallSite {
            kind: CallKind::Publish,
            file: self.file.clone(),
            line,
            column,
            subject_expr: expr_to_source(subject_expr),
            consumer_name_expr: None,
            matched_method: spec.method.clone(),
            enclosing_fn_body: self.fn_body_stack.last().cloned(),
        })
    }

    fn build_subscribe_call_site(
        &self,
        spec: &ConsumerSpec,
        args: &syn::punctuated::Punctuated<Expr, syn::Token![,]>,
    ) -> Option<RawCallSite> {
        let subject_expr = args.iter().nth(spec.subject_arg_index)?;
        if !is_string_shaped(subject_expr) {
            return None;
        }
        let consumer_name_expr = spec
            .consumer_name_arg_index
            .and_then(|idx| args.iter().nth(idx))
            .map(expr_to_source);
        let (line, column) = expr_span(subject_expr);
        Some(RawCallSite {
            kind: CallKind::Subscribe,
            file: self.file.clone(),
            line,
            column,
            subject_expr: expr_to_source(subject_expr),
            consumer_name_expr,
            matched_method: spec.method.clone(),
            enclosing_fn_body: self.fn_body_stack.last().cloned(),
        })
    }

    fn current_module_path(&self) -> String {
        let mut p = self.crate_name.replace('-', "_");
        for seg in &self.module_stack {
            p.push_str("::");
            p.push_str(seg);
        }
        p
    }

    fn fq_for(&self, name: &str) -> FqPath {
        let mut p = self.current_module_path();
        p.push_str("::");
        p.push_str(name);
        p
    }
}

impl<'ast> Visit<'ast> for Visitor<'_> {
    fn visit_item_mod(&mut self, item: &'ast syn::ItemMod) {
        let ident = item.ident.to_string();
        self.module_stack.push(ident);
        syn::visit::visit_item_mod(self, item);
        self.module_stack.pop();
    }

    fn visit_item_impl(&mut self, item: &'ast ItemImpl) {
        let type_path = type_to_string(&item.self_ty);
        self.impl_type_stack.push(type_path.clone());

        // Record builder methods on this impl (consumed by resolver).
        for impl_item in &item.items {
            if let ImplItem::Fn(method) = impl_item {
                let name = method.sig.ident.to_string();
                let body_source = format!("{}", quote_method_body(method));
                let (line, column) = ident_span(&method.sig.ident);
                let span = Span {
                    file: self.file.clone(),
                    line,
                    column,
                };
                let fq = format!("{type_path}::{name}");
                self.symbols.symbols.push((
                    fq,
                    Symbol::Method {
                        type_path: type_path.clone(),
                        body: ExprSnippet {
                            source: body_source,
                            span,
                        },
                    },
                ));
            }
        }

        // Push impl-method bodies on the fn-body stack so calls
        // inside them can follow `let X = ...` bindings.
        for impl_item in &item.items {
            if let ImplItem::Fn(method) = impl_item {
                self.fn_body_stack
                    .push(format!("{}", quote_method_body(method)));
                syn::visit::visit_impl_item_fn(self, method);
                self.fn_body_stack.pop();
            }
        }

        self.impl_type_stack.pop();
    }

    fn visit_item_const(&mut self, item: &'ast ItemConst) {
        let name = item.ident.to_string();
        let fq = self.fq_for(&name);
        let (line, column) = ident_span(&item.ident);
        let span = Span {
            file: self.file.clone(),
            line,
            column,
        };
        let source = expr_to_source(&item.expr);
        self.symbols
            .symbols
            .push((fq, Symbol::Const(ExprSnippet { source, span })));
        syn::visit::visit_item_const(self, item);
    }

    fn visit_item_fn(&mut self, item: &'ast ItemFn) {
        let name = item.sig.ident.to_string();
        let fq = self.fq_for(&name);
        let (line, column) = ident_span(&item.sig.ident);
        let span = Span {
            file: self.file.clone(),
            line,
            column,
        };
        let source = block_to_source(&item.block);
        self.symbols.symbols.push((
            fq,
            Symbol::Function(ExprSnippet {
                source: source.clone(),
                span,
            }),
        ));
        // Push fn body so child calls can follow `let X = ...`
        // bindings via Scope::fn_body_source.
        self.fn_body_stack.push(source);
        syn::visit::visit_item_fn(self, item);
        self.fn_body_stack.pop();
    }

    fn visit_expr_method_call(&mut self, call: &'ast ExprMethodCall) {
        self.try_match_method_call(call);
        syn::visit::visit_expr_method_call(self, call);
    }

    fn visit_expr_call(&mut self, call: &'ast ExprCall) {
        self.try_match_call(call);
        syn::visit::visit_expr_call(self, call);
    }
}

// -------- matchers --------

fn matches_publisher_method(
    spec: &PublisherSpec,
    method_name: &str,
    _call: &ExprMethodCall,
    imports: &[String],
) -> bool {
    if spec.kind != PublisherKind::Trait {
        return false;
    }
    if spec.method != method_name {
        return false;
    }
    matches_trait_import(spec.path.as_deref(), imports)
}

fn matches_consumer_method(
    spec: &ConsumerSpec,
    method_name: &str,
    _call: &ExprMethodCall,
    imports: &[String],
) -> bool {
    if spec.kind != PublisherKind::Trait {
        return false;
    }
    if spec.method != method_name {
        return false;
    }
    matches_trait_import(spec.path.as_deref(), imports)
}

fn matches_trait_import(trait_path: Option<&str>, imports: &[String]) -> bool {
    let Some(path) = trait_path else {
        return false;
    };
    imports.iter().any(|imp| {
        imp == path || imp.ends_with(&format!("::{path}")) || trait_segment_matches(imp, path)
    })
}

/// True if `imp` ends with the same final segment as `trait_path`.
/// Catches `use my_bus::Publisher` matching `my_bus::Publisher`
/// when the config uses a longer module path than the import.
fn trait_segment_matches(imp: &str, trait_path: &str) -> bool {
    let imp_last = imp.rsplit("::").next().unwrap_or(imp);
    let trait_last = trait_path.rsplit("::").next().unwrap_or(trait_path);
    imp_last == trait_last && (imp.contains("::") || trait_path.contains("::"))
}

fn matches_type_path(
    expected_type: Option<&str>,
    receiver_path: &str,
    imports: &[String],
    method_match: MethodMatch,
) -> bool {
    let Some(expected) = expected_type else {
        return false;
    };
    let expected_last = expected.rsplit("::").next().unwrap_or(expected);

    // Three shapes accepted:
    // 1. Fully-qualified call: receiver path == configured type path.
    //    No import required — the path is explicit at the call site.
    // 2. Short call: receiver is the type's last segment AND the full
    //    type path was imported (`use my_bus::nats::NatsPublisher;`).
    // 3. Suffix call: receiver path is a suffix of the configured type
    //    path AND that suffix was imported
    //    (`use my_bus::nats; nats::NatsPublisher::publish_within(...)`).
    if receiver_path == expected {
        return true;
    }
    if receiver_path == expected_last {
        return match method_match {
            MethodMatch::Name => true,
            MethodMatch::NameTraitPathHint => matches_trait_import(Some(expected), imports),
        };
    }
    if expected.ends_with(&format!("::{receiver_path}")) {
        return match method_match {
            MethodMatch::Name => true,
            MethodMatch::NameTraitPathHint => matches_trait_import(Some(expected), imports),
        };
    }
    false
}

/// Extract `(full_receiver_path, method_ident)` from an
/// associated-function expression like
/// `my_bus::nats::NatsPublisher::publish_within`.
///
/// `full_receiver_path` is the joined path **without** the final
/// method segment. For `gordon_bus::nats::NatsPublisher::publish_within`
/// this returns `("gordon_bus::nats::NatsPublisher", "publish_within")`.
/// For the un-qualified shape `NatsPublisher::publish_within`
/// it returns `("NatsPublisher", "publish_within")`.
fn associated_fn_segments(func: &Expr) -> Option<(String, String)> {
    let Expr::Path(p) = func else { return None };
    if p.path.segments.len() < 2 {
        return None;
    }
    let last = p.path.segments.last()?.ident.to_string();
    let receiver_segments: Vec<String> = p
        .path
        .segments
        .iter()
        .take(p.path.segments.len() - 1)
        .map(|s| s.ident.to_string())
        .collect();
    Some((receiver_segments.join("::"), last))
}

// -------- arg-shape filter --------

/// True if `expr` is a string-shaped subject argument.
///
/// Accepts:
/// - String literals: `"foo.bar"`
/// - Format macros: `format!("...")`
/// - Path expressions naming an identifier: `INTENTS_SUBJECT`,
///   `module::SUBJECT`
/// - References to any of the above: `&subject`,
///   `&INTENTS_SUBJECT`, `&format!("...")`
/// - Method calls returning a String (heuristic): `.to_owned()`,
///   `.to_string()`, `.into()`, `.clone()`
/// - Function-call returning String:
///   `build_breaker_subject(...)` (heuristic — any function call
///   with a string-shaped result is accepted; the resolver in
///   story 03 narrows further)
///
/// Rejects:
/// - Bare path expressions to **enum variants**:
///   `PublishableChannel::BotEvents`. Multi-segment path with
///   `PascalCase` final segment heuristic.
/// - Numeric literals, bool literals, struct literals.
fn is_string_shaped(expr: &Expr) -> bool {
    match expr {
        Expr::Lit(lit) => matches!(lit.lit, syn::Lit::Str(_) | syn::Lit::ByteStr(_)),
        Expr::Macro(m) => {
            let last = m.mac.path.segments.last();
            matches!(
                last.map(|s| s.ident.to_string()).as_deref(),
                Some("format" | "concat" | "include_str")
            )
        }
        Expr::Path(p) => is_string_shaped_path(&p.path),
        Expr::Reference(r) => is_string_shaped(&r.expr),
        Expr::MethodCall(mc) => {
            let m = mc.method.to_string();
            matches!(
                m.as_str(),
                "to_owned" | "to_string" | "into" | "clone" | "as_str" | "as_ref"
            )
        }
        Expr::Call(c) => {
            // Function calls returning String: accept if the
            // callee is a path (not a closure / variable). Resolver
            // narrows further by inspecting the function body.
            matches!(*c.func, Expr::Path(_))
        }
        Expr::Paren(p) => is_string_shaped(&p.expr),
        Expr::Group(g) => is_string_shaped(&g.expr),
        _ => false,
    }
}

/// A bare path is string-shaped when it's a single ASCII-uppercase
/// identifier (`INTENTS_SUBJECT`) or a multi-segment path whose
/// final segment is `SCREAMING_SNAKE_CASE` (`crate::s::INTENTS_SUBJECT`).
/// Rejects `PascalCase` final segments
/// (`PublishableChannel::BotEvents`, which is an enum variant —
/// the false-positive trap from synthesis §H1).
fn is_string_shaped_path(path: &syn::Path) -> bool {
    let Some(last) = path.segments.last() else {
        return false;
    };
    let ident = last.ident.to_string();
    // SCREAMING_SNAKE_CASE: starts with uppercase, contains underscore
    // or is all-uppercase letters.
    let is_screaming = ident.chars().next().is_some_and(|c| c.is_ascii_uppercase())
        && ident
            .chars()
            .all(|c| c.is_ascii_uppercase() || c == '_' || c.is_ascii_digit());

    // OR snake_case (local binding heuristic — `subject`, `runtime_subject`):
    let is_snake = ident.chars().next().is_some_and(|c| c.is_ascii_lowercase())
        && ident
            .chars()
            .all(|c| c.is_ascii_lowercase() || c == '_' || c.is_ascii_digit());

    is_screaming || is_snake
}

// -------- span + source-snippet helpers --------

fn expr_span(expr: &Expr) -> (usize, usize) {
    use syn::spanned::Spanned;
    let span = expr.span().start();
    (span.line, span.column.saturating_add(1))
}

fn ident_span(ident: &proc_macro2::Ident) -> (usize, usize) {
    let span = ident.span().start();
    (span.line, span.column.saturating_add(1))
}

fn expr_to_source(expr: &Expr) -> String {
    let tokens = quote_to_tokens(expr);
    tokens.to_string()
}

fn quote_to_tokens<T: quote_compat::ToTokensCompat>(t: &T) -> proc_macro2::TokenStream {
    t.to_tokens_compat()
}

fn block_to_source(block: &syn::Block) -> String {
    use quote_compat::ToTokensCompat;
    block.to_tokens_compat().to_string()
}

fn quote_method_body(method: &syn::ImplItemFn) -> proc_macro2::TokenStream {
    use quote_compat::ToTokensCompat;
    method.block.to_tokens_compat()
}

fn type_to_string(ty: &syn::Type) -> String {
    use quote_compat::ToTokensCompat;
    let mut s = ty.to_tokens_compat().to_string();
    // Strip whitespace introduced by quote, then collapse.
    s.retain(|c| !c.is_whitespace());
    s
}

// `syn` doesn't pull `quote` by default in our trimmed feature set.
// Local mini-shim: implement `to_tokens` via the `syn::__private`
// path that syn re-exports. Avoids the `quote` dep for now.
//
// Note: `proc_macro2::TokenStream::to_string()` round-trips reasonably
// for our purposes (subject snippets, function bodies). It's not
// pretty-formatted but it's deterministic.
mod quote_compat {
    use proc_macro2::TokenStream;

    pub trait ToTokensCompat {
        fn to_tokens_compat(&self) -> TokenStream;
    }

    impl<T: quote::ToTokens> ToTokensCompat for T {
        fn to_tokens_compat(&self) -> TokenStream {
            let mut ts = TokenStream::new();
            self.to_tokens(&mut ts);
            ts
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn pub_trait_spec(path: &str, method: &str) -> PublisherSpec {
        PublisherSpec {
            kind: PublisherKind::Trait,
            path: Some(path.to_string()),
            type_path: None,
            method: method.to_string(),
            subject_arg_index: 0,
        }
    }

    fn pub_inherent_spec(type_path: &str, method: &str, arg_idx: usize) -> PublisherSpec {
        PublisherSpec {
            kind: PublisherKind::Inherent,
            path: None,
            type_path: Some(type_path.to_string()),
            method: method.to_string(),
            subject_arg_index: arg_idx,
        }
    }

    fn sub_trait_spec(path: &str, method: &str) -> ConsumerSpec {
        ConsumerSpec {
            kind: PublisherKind::Trait,
            path: Some(path.to_string()),
            type_path: None,
            method: method.to_string(),
            subject_arg_index: 0,
            consumer_name_arg_index: Some(1),
        }
    }

    fn extract_with(spec: &CallSiteConfig, src: &str) -> PerFileCallSites {
        extract(&PathBuf::from("test.rs"), src, "test-crate", spec).expect("parse")
    }

    #[test]
    fn matches_trait_publish_with_import() {
        let src = r#"
            use my_bus::Publisher;
            async fn run(p: &dyn Publisher) {
                p.publish("foo.bar", &[]).await.unwrap();
            }
        "#;
        let cfg = CallSiteConfig {
            method_match: MethodMatch::NameTraitPathHint,
            publishers: vec![pub_trait_spec("my_bus::Publisher", "publish")],
            consumers: vec![],
        };
        let out = extract_with(&cfg, src);
        assert_eq!(out.sites.len(), 1);
        assert_eq!(out.sites[0].kind, CallKind::Publish);
        assert!(out.sites[0].subject_expr.contains("foo.bar"));
    }

    #[test]
    fn rejects_trait_publish_without_import() {
        let src = r#"
            async fn run(p: &dyn SomethingElse) {
                p.publish("foo.bar", &[]).await.unwrap();
            }
        "#;
        let cfg = CallSiteConfig {
            method_match: MethodMatch::NameTraitPathHint,
            publishers: vec![pub_trait_spec("my_bus::Publisher", "publish")],
            consumers: vec![],
        };
        let out = extract_with(&cfg, src);
        assert!(out.sites.is_empty(), "no use → no match under hint");
    }

    /// Critical regression for synthesis §H1: two `.publish()` methods
    /// in different crates, only the configured one should match.
    #[test]
    fn false_positive_method_name_filtered_by_import_hint() {
        // Mimics Gordon's gordon_platform::ipc::PublishableChannel
        // (wrong trait) vs gordon_bus::Publisher (right trait).
        let src = r"
            use gordon_platform::ipc::Publisher as IpcPublisher;

            async fn run(p: &dyn IpcPublisher) {
                p.publish(SomeChannel::Foo, record).await.unwrap();
            }
        ";
        let cfg = CallSiteConfig {
            method_match: MethodMatch::NameTraitPathHint,
            publishers: vec![pub_trait_spec("gordon_bus::Publisher", "publish")],
            consumers: vec![],
        };
        let out = extract_with(&cfg, src);
        assert!(
            out.sites.is_empty(),
            "ipc::Publisher publish must not match gordon_bus::Publisher"
        );
    }

    /// Path-expression subject (enum variant) gets rejected by
    /// the arg-shape filter even when method name matches.
    #[test]
    fn enum_variant_subject_arg_rejected() {
        let src = r"
            use my_bus::Publisher;
            async fn run(p: &dyn Publisher) {
                p.publish(Channel::BotEvents, &[]).await.unwrap();
            }
        ";
        let cfg = CallSiteConfig {
            method_match: MethodMatch::NameTraitPathHint,
            publishers: vec![pub_trait_spec("my_bus::Publisher", "publish")],
            consumers: vec![],
        };
        let out = extract_with(&cfg, src);
        assert!(out.sites.is_empty(), "PascalCase variant rejected");
    }

    #[test]
    fn matches_inherent_publish_with_arg_index_one() {
        let src = r"
            use my_bus::nats::NatsPublisher;
            async fn run() {
                NatsPublisher::publish_within(&mut tx, &subject, &[]).await.unwrap();
            }
        ";
        let cfg = CallSiteConfig {
            method_match: MethodMatch::NameTraitPathHint,
            publishers: vec![pub_inherent_spec(
                "my_bus::nats::NatsPublisher",
                "publish_within",
                1,
            )],
            consumers: vec![],
        };
        let out = extract_with(&cfg, src);
        assert_eq!(out.sites.len(), 1, "inherent publish_within matched");
        assert!(out.sites[0].subject_expr.contains("subject"));
    }

    #[test]
    fn const_subject_accepted_as_string_shaped() {
        let src = r#"
            use my_bus::Publisher;
            pub const SUBJECT: &str = "foo.bar";
            async fn run(p: &dyn Publisher) {
                p.publish(SUBJECT, &[]).await.unwrap();
            }
        "#;
        let cfg = CallSiteConfig {
            method_match: MethodMatch::NameTraitPathHint,
            publishers: vec![pub_trait_spec("my_bus::Publisher", "publish")],
            consumers: vec![],
        };
        let out = extract_with(&cfg, src);
        assert_eq!(out.sites.len(), 1);
        let const_count = out
            .symbols
            .symbols
            .iter()
            .filter(|(_, s)| matches!(s, Symbol::Const(_)))
            .count();
        assert_eq!(const_count, 1, "const recorded as Symbol");
    }

    #[test]
    fn format_subject_accepted() {
        let src = r#"
            use my_bus::Publisher;
            async fn run(p: &dyn Publisher, sym: &str) {
                p.publish(format!("market.klines.{sym}.1m"), &[]).await.unwrap();
            }
        "#;
        let cfg = CallSiteConfig {
            method_match: MethodMatch::NameTraitPathHint,
            publishers: vec![pub_trait_spec("my_bus::Publisher", "publish")],
            consumers: vec![],
        };
        let out = extract_with(&cfg, src);
        assert_eq!(out.sites.len(), 1);
    }

    #[test]
    fn reference_to_const_accepted() {
        let src = r#"
            use my_bus::nats::NatsPublisher;
            pub const SUBJECT: &str = "foo";
            async fn run() {
                NatsPublisher::publish_within(&mut tx, &SUBJECT, &[]).await.unwrap();
            }
        "#;
        let cfg = CallSiteConfig {
            method_match: MethodMatch::NameTraitPathHint,
            publishers: vec![pub_inherent_spec(
                "my_bus::nats::NatsPublisher",
                "publish_within",
                1,
            )],
            consumers: vec![],
        };
        let out = extract_with(&cfg, src);
        assert_eq!(out.sites.len(), 1);
        assert!(out.sites[0].subject_expr.contains("SUBJECT"));
    }

    #[test]
    fn subscribe_captures_consumer_name() {
        let src = r#"
            use my_bus::Consumer;
            async fn run(c: &dyn Consumer) {
                c.subscribe("foo.bar", "my-durable").await.unwrap();
            }
        "#;
        let cfg = CallSiteConfig {
            method_match: MethodMatch::NameTraitPathHint,
            publishers: vec![],
            consumers: vec![sub_trait_spec("my_bus::Consumer", "subscribe")],
        };
        let out = extract_with(&cfg, src);
        assert_eq!(out.sites.len(), 1);
        assert_eq!(out.sites[0].kind, CallKind::Subscribe);
        assert!(out.sites[0]
            .consumer_name_expr
            .as_deref()
            .unwrap()
            .contains("my-durable"));
    }

    #[test]
    fn extracts_free_function_into_symbols() {
        let src = r#"
            pub fn build_breaker_subject(name: &str) -> String {
                format!("risk.events.{name}")
            }
        "#;
        let cfg = CallSiteConfig::default();
        let out = extract_with(&cfg, src);
        assert!(out.sites.is_empty());
        assert_eq!(out.symbols.symbols.len(), 1);
        match &out.symbols.symbols[0] {
            (path, Symbol::Function(snip)) => {
                assert!(path.ends_with("::build_breaker_subject"));
                assert!(snip.source.contains("risk.events"));
            }
            other => panic!("expected Function, got {other:?}"),
        }
    }

    #[test]
    fn extracts_impl_methods_into_symbols() {
        let src = r#"
            pub struct FillEvent;
            impl FillEvent {
                pub fn nats_subject(&self) -> String {
                    format!("trading.fills.{}", "bot")
                }
            }
        "#;
        let cfg = CallSiteConfig::default();
        let out = extract_with(&cfg, src);
        let method = out
            .symbols
            .symbols
            .iter()
            .find(|(_, s)| matches!(s, Symbol::Method { .. }));
        assert!(method.is_some(), "impl method recorded");
    }

    #[test]
    fn sites_sorted_by_position() {
        let src = r#"
            use my_bus::Publisher;
            async fn run(p: &dyn Publisher) {
                p.publish("third.subject", &[]).await.unwrap();
                p.publish("first.subject", &[]).await.unwrap();
                p.publish("second.subject", &[]).await.unwrap();
            }
        "#;
        let cfg = CallSiteConfig {
            method_match: MethodMatch::NameTraitPathHint,
            publishers: vec![pub_trait_spec("my_bus::Publisher", "publish")],
            consumers: vec![],
        };
        let out = extract_with(&cfg, src);
        assert_eq!(out.sites.len(), 3);
        // Stable: first site is line N, third site is line N+2.
        assert!(out.sites[0].line < out.sites[1].line);
        assert!(out.sites[1].line < out.sites[2].line);
        assert!(out.sites[0].subject_expr.contains("third.subject"));
        assert!(out.sites[2].subject_expr.contains("second.subject"));
    }

    #[test]
    fn parse_error_returns_typed_err() {
        let res = extract(
            &PathBuf::from("bad.rs"),
            "this is not valid rust ::: {",
            "x",
            &CallSiteConfig::default(),
        );
        assert!(matches!(res, Err(ParseError::Syn { .. })));
    }
}
