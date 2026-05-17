// cargo-eventflow library surface.
// Stub for story 00 — real implementation lands in stories 01-04.
// See plan in https://github.com/dlepaux/cargo-eventflow (or gordon-workspace
// plan/active/cargo-eventflow/ for the full epic).

#![deny(rust_2018_idioms)]
#![warn(missing_docs)]
#![warn(clippy::pedantic)]
#![allow(clippy::missing_errors_doc)] // re-enabled per-module as APIs stabilize

//! cargo-eventflow — auto-derive event-flow diagrams from Rust source.
//!
//! Public surface is intentionally narrow in v0.1: configure, analyze,
//! emit. Future versions may expand as editor / LSP integrations
//! materialize.

pub mod analysis;
pub mod config;
pub mod discover;
pub mod model;

/// Top-level error type returned by [`analyze`].
#[derive(Debug, thiserror::Error)]
pub enum AnalyzeError {
    /// Workspace discovery via `cargo metadata` failed.
    #[error("workspace discovery failed: {0}")]
    Discover(String),

    /// Source parse failed beyond recovery.
    #[error("parse error in {file}: {message}")]
    Parse {
        /// Path to the source file that failed.
        file: std::path::PathBuf,
        /// Reason from `syn`.
        message: String,
    },

    /// Configuration validation failed.
    #[error("config error: {0}")]
    Config(String),
}

/// Options consumed by [`emit_mermaid`].
#[derive(Debug, Clone)]
pub struct MermaidOpts {
    /// Theme controls the colour palette.
    pub theme: Theme,
    /// Group subjects under their publishing service via subgraphs.
    pub group_by_publisher: bool,
    /// Emit ingress/egress trapezoid nodes for declared external sources/sinks.
    pub include_ingress_egress: bool,
    /// Add tool-version stamp to the @generated banner (default: false).
    pub include_version_stamp: bool,
}

impl Default for MermaidOpts {
    fn default() -> Self {
        Self {
            theme: Theme::Default,
            group_by_publisher: true,
            include_ingress_egress: true,
            include_version_stamp: false,
        }
    }
}

/// Colour palette for Mermaid output.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Theme {
    /// Default blue/green/red/purple palette.
    Default,
    /// WCAG AA contrast palette (`ColorBrewer` Set2-ish).
    HighContrast,
    /// Black + grayscale only; stabilises `check` diffs across renderers.
    Monochrome,
    /// Custom palette from `[output.mermaid.custom]` config section.
    Custom,
}

/// Walk the workspace at `manifest`, extract pub/sub call sites,
/// resolve subjects, return the graph model.
///
/// # Errors
///
/// Returns [`AnalyzeError`] on workspace discovery, parse, or config failure.
///
/// # Panics
///
/// Currently panics with `unimplemented!()` — real implementation lands
/// in stories 01-04.
pub fn analyze(
    _config: &config::Config,
    _manifest: &std::path::Path,
) -> Result<model::Graph, AnalyzeError> {
    unimplemented!("analyzer not yet implemented — see story 01-04")
}

/// Render a [`model::Graph`] to a Mermaid `graph LR` diagram string.
///
/// # Panics
///
/// Currently panics with `unimplemented!()` — real implementation lands
/// in story 04.
#[must_use]
pub fn emit_mermaid(_graph: &model::Graph, _opts: &MermaidOpts) -> String {
    unimplemented!("emit not yet implemented — see story 04")
}
