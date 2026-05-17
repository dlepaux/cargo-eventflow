//! AST walk + symbol extraction + subject resolution +
//! graph build. Single module per [synthesis §P1-A][synth] —
//! the cut between parse and resolve is internal, not a public
//! seam.
//!
//! Story 01 lands [`symbol_index`]; stories 02-04 wire
//! callsite, subject, binding, annotation, graph in this order.
//!
//! [synth]: <https://github.com/dlepaux/cargo-eventflow> (see
//! `gordon-workspace/plan/active/cargo-eventflow/synthesis.md`
//! for the full design rationale).

pub mod symbol_index;

pub use symbol_index::{ExprSnippet, FqPath, Span, Symbol, SymbolIndex};
