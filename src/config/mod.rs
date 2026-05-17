// Config schema + validation. Real implementation lands in story 06.

#![allow(missing_docs)] // re-enabled when schema stabilises in story 06

use std::path::PathBuf;

/// Workspace-level configuration loaded from `.eventflow.toml`.
///
/// v0.1 schema is documented in
/// `gordon-workspace/plan/active/cargo-eventflow/synthesis.md` §P1-B.
#[derive(Debug, Clone, Default)]
pub struct Config {
    pub schema_version: u32,
    pub source: Option<PathBuf>,
}

impl Config {
    /// Construct a default v0.1 config (used until story 06 ships).
    #[must_use]
    pub fn defaults() -> Self {
        Self {
            schema_version: 1,
            source: None,
        }
    }
}
