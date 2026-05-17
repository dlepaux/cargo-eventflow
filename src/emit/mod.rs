//! Output emitters. Mermaid is the only format implemented in
//! v0.1; DOT and D2 are deferred (synthesis §10).

pub mod mermaid;

pub use mermaid::{render_mermaid, MermaidOptions};
