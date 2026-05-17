//! `.eventflow.toml` schema + loader.
//!
//! Stripped-down v0.1 subset of synthesis §P1-B. Covers
//! everything `examples/gordon.toml` declares: workspace
//! classification, publisher/consumer specs, subject helpers,
//! ingress/egress, output theme/path/markdown. Story 06 expands
//! with `schema_version` migration, `init` auto-detect, and
//! validation diagnostics with line:col.

use std::path::Path;

use serde::Deserialize;

use crate::analysis::callsite::{ConsumerSpec, MethodMatch, PublisherSpec};
use crate::analysis::graph::{Egress, Ingress};
use crate::discover::DiscoverConfig;
use crate::emit::mermaid::Theme;

/// Top-level `.eventflow.toml` schema.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// Schema version. Currently only `1` is supported.
    /// Absent in TOML → defaults to `1`.
    #[serde(default = "default_schema_version")]
    pub schema_version: u32,

    /// Workspace classification rules.
    #[serde(default)]
    pub workspace: DiscoverConfig,

    /// Publisher/consumer specs + matcher mode.
    #[serde(default)]
    pub bus: BusConfig,

    /// Subject-helper config (helper crates + builder names).
    #[serde(default)]
    pub subjects: SubjectsConfig,

    /// Declared ingress (external sources). Optional.
    #[serde(default)]
    pub ingress: IngressTable,

    /// Declared egress (external sinks). Optional.
    #[serde(default)]
    pub egress: EgressTable,

    /// Output destination + theme.
    #[serde(default)]
    pub output: OutputConfig,

    /// Diagnostic limits.
    #[serde(default)]
    pub diagnostics: DiagnosticsConfig,
}

fn default_schema_version() -> u32 {
    1
}

/// `[bus]` section.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BusConfig {
    /// Matcher mode — defaults to `name+trait_path_hint`.
    #[serde(default)]
    pub method_match: MethodMatch,

    /// `[[bus.publisher]]` array.
    #[serde(default)]
    pub publisher: Vec<PublisherSpec>,

    /// `[[bus.consumer]]` array.
    #[serde(default)]
    pub consumer: Vec<ConsumerSpec>,
}

/// `[subjects]` section.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SubjectsConfig {
    /// Additional crates whose const/method/fn surface is
    /// searched (after same-crate-first lookup).
    #[serde(default)]
    pub helper_crates: Vec<String>,
    /// Method names treated as subject builders.
    #[serde(default = "default_builder_methods")]
    pub subject_builder_methods: Vec<String>,
    /// Free-function names treated as subject builders.
    #[serde(default)]
    pub subject_builder_functions: Vec<String>,
    /// Wildcard-collapse strategy. Currently informational —
    /// emit always collapses sibling subjects.
    #[serde(default = "default_collapse_wildcards")]
    pub collapse_wildcards: String,
}

fn default_builder_methods() -> Vec<String> {
    vec!["nats_subject".into()]
}

fn default_collapse_wildcards() -> String {
    "auto".into()
}

/// `[ingress]` table — wraps the `[[ingress.source]]` array
/// because TOML headers like `[[ingress.source]]` deserialise
/// into `ingress: { source: Vec<Ingress> }`.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IngressTable {
    /// `[[ingress.source]]` entries.
    #[serde(default)]
    pub source: Vec<Ingress>,
}

/// `[egress]` table — same shape as `IngressTable`.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EgressTable {
    /// `[[egress.sink]]` entries.
    #[serde(default)]
    pub sink: Vec<Egress>,
}

/// `[output]` section.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OutputConfig {
    /// Output format. v0.1 only `mermaid` is implemented.
    #[serde(default = "default_format")]
    pub format: String,
    /// Default committed-diagram path. CLI `--output` overrides.
    #[serde(default = "default_output_path")]
    pub path: String,
    /// Wrap in a Markdown code fence by default.
    #[serde(default = "default_markdown")]
    pub markdown: bool,
    /// `[output.mermaid]` sub-section.
    #[serde(default)]
    pub mermaid: MermaidOutputConfig,
}

impl Default for OutputConfig {
    fn default() -> Self {
        Self {
            format: default_format(),
            path: default_output_path(),
            markdown: default_markdown(),
            mermaid: MermaidOutputConfig::default(),
        }
    }
}

fn default_format() -> String {
    "mermaid".into()
}

fn default_output_path() -> String {
    "docs/event-flow.md".into()
}

fn default_markdown() -> bool {
    true
}

/// `[output.mermaid]` section.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MermaidOutputConfig {
    /// Theme name (`default` | `high-contrast` | `monochrome`).
    #[serde(default = "default_theme")]
    pub theme: String,
    /// Cluster subjects under their publishing service via subgraphs.
    #[serde(default = "default_group_by_publisher")]
    pub group_by_publisher: bool,
}

fn default_theme() -> String {
    "default".into()
}

fn default_group_by_publisher() -> bool {
    true
}

/// `[diagnostics]` section.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DiagnosticsConfig {
    /// Maximum diagnostic warnings printed before collapsing into a count.
    #[serde(default = "default_max_warnings")]
    pub max_warnings: usize,
    /// Subject patterns whose `?` resolution is silenced.
    #[serde(default)]
    pub allow_dynamic: Vec<String>,
}

impl Default for DiagnosticsConfig {
    fn default() -> Self {
        Self {
            max_warnings: default_max_warnings(),
            allow_dynamic: Vec::new(),
        }
    }
}

fn default_max_warnings() -> usize {
    20
}

/// Errors returned by [`load`].
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    /// I/O failure reading the config file.
    #[error("I/O error reading {path}: {source}")]
    Io {
        /// File that could not be read.
        path: std::path::PathBuf,
        /// Underlying I/O error.
        #[source]
        source: std::io::Error,
    },

    /// TOML parse failure.
    #[error("TOML parse error in {path}: {source}")]
    Parse {
        /// File that failed to parse.
        path: std::path::PathBuf,
        /// Underlying TOML error.
        #[source]
        source: toml::de::Error,
    },

    /// Config declares a schema version newer than the tool supports.
    #[error("unsupported schema_version {found} in {path} (this tool supports {supported})")]
    UnsupportedSchemaVersion {
        /// File that declared the unsupported version.
        path: std::path::PathBuf,
        /// Version the file declared.
        found: u32,
        /// Maximum version this tool understands.
        supported: u32,
    },
}

/// Maximum supported schema version. Bump only on a major tool
/// version per `STABILITY.md`.
pub const SUPPORTED_SCHEMA_VERSION: u32 = 1;

/// Read `.eventflow.toml` from `path` and deserialise into
/// [`Config`].
///
/// # Errors
///
/// Returns [`ConfigError`] on I/O failure, TOML parse failure,
/// or unsupported schema version.
pub fn load(path: &Path) -> Result<Config, ConfigError> {
    let raw = std::fs::read_to_string(path).map_err(|source| ConfigError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    let cfg: Config = toml::from_str(&raw).map_err(|source| ConfigError::Parse {
        path: path.to_path_buf(),
        source,
    })?;
    if cfg.schema_version > SUPPORTED_SCHEMA_VERSION {
        return Err(ConfigError::UnsupportedSchemaVersion {
            path: path.to_path_buf(),
            found: cfg.schema_version,
            supported: SUPPORTED_SCHEMA_VERSION,
        });
    }
    Ok(cfg)
}

/// Map the loaded `[output.mermaid].theme` string to a
/// [`Theme`] enum. Unknown values fall back to [`Theme::Default`]
/// with no error — v0.2 will tighten via `init`'s validator.
#[must_use]
pub fn parse_theme(name: &str) -> Theme {
    match name {
        "high-contrast" => Theme::HighContrast,
        "monochrome" => Theme::Monochrome,
        _ => Theme::Default,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"
schema_version = 1

[workspace]
services = ["svc-*"]
libraries = ["lib-*"]
ignore = ["bench-*"]

[bus]
method_match = "name+trait_path_hint"

[[bus.publisher]]
kind = "trait"
path = "my_bus::Publisher"
method = "publish"

[[bus.publisher]]
kind = "inherent"
type = "my_bus::nats::NatsPublisher"
method = "publish_within"
subject_arg_index = 1

[[bus.consumer]]
kind = "trait"
path = "my_bus::Consumer"
method = "subscribe"
consumer_name_arg_index = 1

[subjects]
helper_crates = ["proto", "domain"]
subject_builder_methods = ["nats_subject"]
subject_builder_functions = ["build_breaker_subject"]
collapse_wildcards = "auto"

[[ingress.source]]
name = "External WS"
into = ["market.>"]
crate = "data"

[[egress.sink]]
name = "Exchange REST"
from_crate = "executor"
triggered_by = ["intents.executor"]

[output]
format = "mermaid"
path = "docs/event-flow.md"
markdown = true

[output.mermaid]
theme = "default"

[diagnostics]
max_warnings = 20
allow_dynamic = []
"#;

    #[test]
    fn parses_full_sample() {
        let cfg: Config = toml::from_str(SAMPLE).expect("parse");
        assert_eq!(cfg.schema_version, 1);
        assert_eq!(cfg.workspace.services, vec!["svc-*"]);
        assert_eq!(cfg.bus.publisher.len(), 2);
        assert_eq!(cfg.bus.consumer.len(), 1);
        assert_eq!(cfg.subjects.helper_crates, vec!["proto", "domain"]);
        assert_eq!(cfg.ingress.source.len(), 1);
        assert_eq!(cfg.egress.sink.len(), 1);
        assert_eq!(cfg.output.path, "docs/event-flow.md");
        assert!(cfg.output.markdown);
    }

    #[test]
    fn parses_minimum_viable() {
        let cfg: Config = toml::from_str("").expect("empty is valid");
        assert_eq!(cfg.schema_version, 1);
        assert_eq!(cfg.bus.publisher.len(), 0);
    }

    #[test]
    fn unknown_field_rejected() {
        let bad = "[workspace]\nbogus_key = 1";
        assert!(toml::from_str::<Config>(bad).is_err());
    }

    #[test]
    fn future_schema_version_rejected_by_loader() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(tmp.path(), "schema_version = 999").unwrap();
        let err = load(tmp.path()).unwrap_err();
        assert!(matches!(
            err,
            ConfigError::UnsupportedSchemaVersion {
                found: 999,
                supported: 1,
                ..
            }
        ));
    }
}
