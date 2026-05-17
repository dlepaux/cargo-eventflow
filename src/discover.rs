//! Workspace discovery — enumerate Cargo workspace members via
//! [`cargo_metadata`], classify them as `Service` / `Library` /
//! `Ignored` based on config, and enumerate the `.rs` files for
//! each.
//!
//! Per [`crate::config::Config`], `services` define
//! pub/sub call sites, `libraries` host subject helpers
//! (constants, methods, free functions), and `ignored` are
//! skipped entirely. Glob patterns match crate names.
//!
//! Test directories (`tests/`, `examples/`, `benches/`) are
//! excluded by default; opt-in via
//! `[workspace.include_test_files] = true`.

use std::path::{Path, PathBuf};

use globset::{Glob, GlobSet, GlobSetBuilder};

/// Workspace discovery error.
#[derive(Debug, thiserror::Error)]
pub enum DiscoverError {
    /// `cargo metadata` invocation failed (subprocess error, JSON
    /// parse error, etc.).
    #[error("cargo metadata failed: {0}")]
    Metadata(#[from] cargo_metadata::Error),

    /// A glob pattern in the workspace config did not parse.
    #[error("invalid glob pattern {pattern:?}: {source}")]
    InvalidGlob {
        /// The offending pattern.
        pattern: String,
        /// Underlying globset error.
        #[source]
        source: globset::Error,
    },

    /// I/O error while walking source files.
    #[error("I/O error walking {path}: {source}")]
    Io {
        /// Path that failed.
        path: PathBuf,
        /// Underlying I/O error.
        #[source]
        source: std::io::Error,
    },
}

/// Classification of a workspace member.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CrateKind {
    /// Service crates host publish/subscribe call sites.
    Service,
    /// Library crates host subject helpers (consts, methods, fns).
    Library,
    /// Ignored crates are skipped entirely.
    Ignored,
}

/// One workspace member with its classification + source files.
#[derive(Debug, Clone)]
pub struct CrateInfo {
    /// Crate name as declared in its `Cargo.toml`.
    pub name: String,
    /// Filesystem path to the crate directory.
    pub path: PathBuf,
    /// Classification per config.
    pub kind: CrateKind,
    /// `.rs` files belonging to this crate (per inclusion rules).
    pub source_files: Vec<PathBuf>,
}

/// A discovered workspace.
#[derive(Debug, Clone)]
pub struct Workspace {
    /// Workspace root (where the top-level `Cargo.toml` lives).
    pub root: PathBuf,
    /// All members, classified.
    pub members: Vec<CrateInfo>,
}

impl Workspace {
    /// Iterate over service crates.
    pub fn services(&self) -> impl Iterator<Item = &CrateInfo> {
        self.members.iter().filter(|c| c.kind == CrateKind::Service)
    }

    /// Iterate over library crates.
    pub fn libraries(&self) -> impl Iterator<Item = &CrateInfo> {
        self.members.iter().filter(|c| c.kind == CrateKind::Library)
    }
}

/// Workspace-classification config consumed by [`discover`].
///
/// This mirrors the `[workspace]` block in `.eventflow.toml`; the
/// full [`crate::config::Config`] wraps this plus the bus +
/// subject + output sections. Kept minimal here so unit tests can
/// drive `discover` without a TOML round-trip.
#[derive(Debug, Clone, Default, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DiscoverConfig {
    /// Glob patterns matching service crate names.
    /// Empty = match every member (default).
    #[serde(default)]
    pub services: Vec<String>,
    /// Glob patterns matching library crate names.
    #[serde(default)]
    pub libraries: Vec<String>,
    /// Glob patterns matching crates to skip.
    #[serde(default)]
    pub ignore: Vec<String>,
    /// Include `tests/`, `examples/`, `benches/` in walked
    /// source files. Off by default.
    #[serde(default)]
    pub include_test_files: bool,
}

/// Walk the workspace at `manifest_path` and return a
/// [`Workspace`] classified per `config`.
///
/// # Errors
///
/// Returns [`DiscoverError`] on metadata failure, invalid glob,
/// or I/O failure while enumerating source files.
///
/// # Panics
///
/// Panics if `cargo_metadata` returns a workspace member id that
/// is not present in the `packages` list. This is an invariant
/// of `cargo metadata` itself and would indicate a cargo bug.
pub fn discover(manifest_path: &Path, config: &DiscoverConfig) -> Result<Workspace, DiscoverError> {
    let mut cmd = cargo_metadata::MetadataCommand::new();
    cmd.manifest_path(manifest_path);
    cmd.no_deps();
    let metadata = cmd.exec()?;

    let services_set = build_glob_set(&config.services)?;
    let libraries_set = build_glob_set(&config.libraries)?;
    let ignore_set = build_glob_set(&config.ignore)?;

    let mut members = Vec::with_capacity(metadata.workspace_members.len());
    for pkg_id in &metadata.workspace_members {
        let pkg = metadata
            .packages
            .iter()
            .find(|p| &p.id == pkg_id)
            .expect("workspace_members reference packages list");

        let kind = classify(
            &pkg.name,
            config,
            &services_set,
            &libraries_set,
            &ignore_set,
        );
        if kind == CrateKind::Ignored {
            continue;
        }

        let crate_path = pkg
            .manifest_path
            .parent()
            .map_or_else(|| PathBuf::from("."), |p| p.as_std_path().to_path_buf());

        let source_files = enumerate_source_files(&crate_path, config.include_test_files)?;

        members.push(CrateInfo {
            name: pkg.name.clone(),
            path: crate_path,
            kind,
            source_files,
        });
    }

    members.sort_by(|a, b| a.name.cmp(&b.name));

    Ok(Workspace {
        root: metadata.workspace_root.as_std_path().to_path_buf(),
        members,
    })
}

fn classify(
    name: &str,
    config: &DiscoverConfig,
    services_set: &GlobSet,
    libraries_set: &GlobSet,
    ignore_set: &GlobSet,
) -> CrateKind {
    if !config.ignore.is_empty() && ignore_set.is_match(name) {
        return CrateKind::Ignored;
    }
    if !config.libraries.is_empty() && libraries_set.is_match(name) {
        return CrateKind::Library;
    }
    if !config.services.is_empty() {
        if services_set.is_match(name) {
            return CrateKind::Service;
        }
        // Explicit services list without a match → ignored.
        return CrateKind::Ignored;
    }
    // Default: every non-ignored member is a service.
    CrateKind::Service
}

fn build_glob_set(patterns: &[String]) -> Result<GlobSet, DiscoverError> {
    let mut builder = GlobSetBuilder::new();
    for pattern in patterns {
        let glob = Glob::new(pattern).map_err(|source| DiscoverError::InvalidGlob {
            pattern: pattern.clone(),
            source,
        })?;
        builder.add(glob);
    }
    builder
        .build()
        .map_err(|source| DiscoverError::InvalidGlob {
            pattern: patterns.join(", "),
            source,
        })
}

fn enumerate_source_files(
    crate_path: &Path,
    include_test_files: bool,
) -> Result<Vec<PathBuf>, DiscoverError> {
    let src_dir = crate_path.join("src");
    let mut files = Vec::new();

    if src_dir.exists() {
        walk_rs(&src_dir, &mut files).map_err(|source| DiscoverError::Io {
            path: src_dir.clone(),
            source,
        })?;
    }

    if include_test_files {
        for opt_dir in ["tests", "examples", "benches"] {
            let dir = crate_path.join(opt_dir);
            if dir.exists() {
                walk_rs(&dir, &mut files).map_err(|source| DiscoverError::Io {
                    path: dir.clone(),
                    source,
                })?;
            }
        }
    }

    files.sort();
    Ok(files)
}

/// Recursively walk `dir`, appending `.rs` files to `out`.
///
/// Uses [`ignore::WalkBuilder`] so `.gitignore` is respected;
/// hidden files are skipped. Symlinks are not followed (avoids
/// pathological cycles via the meta-workspace pattern Gordon
/// uses for cross-crate dev — see synthesis §P1-H).
fn walk_rs(dir: &Path, out: &mut Vec<PathBuf>) -> std::io::Result<()> {
    let walker = ignore::WalkBuilder::new(dir)
        .follow_links(false)
        .standard_filters(true)
        .build();

    for entry in walker {
        let entry = entry.map_err(|err| match err.io_error() {
            Some(io) => std::io::Error::new(io.kind(), err.to_string()),
            None => std::io::Error::other(err.to_string()),
        })?;
        let path = entry.path();
        if path.is_file() && path.extension().is_some_and(|ext| ext == "rs") {
            out.push(path.to_path_buf());
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn make_workspace(tmp: &TempDir) -> std::path::PathBuf {
        let root = tmp.path();
        fs::write(
            root.join("Cargo.toml"),
            r#"
[workspace]
members = ["service-a", "service-b", "lib-c", "bench-d"]
resolver = "2"
"#,
        )
        .unwrap();

        for (name, src) in [
            ("service-a", "fn main() {}\n"),
            ("service-b", "pub fn b() {}\n"),
            ("lib-c", "pub const X: &str = \"\";\n"),
            ("bench-d", "fn main() {}\n"),
        ] {
            let crate_dir = root.join(name);
            fs::create_dir_all(crate_dir.join("src")).unwrap();
            fs::write(
                crate_dir.join("Cargo.toml"),
                format!(
                    r#"
[package]
name = "{name}"
version = "0.0.1"
edition = "2021"

[lib]
path = "src/lib.rs"
"#,
                ),
            )
            .unwrap();
            fs::write(crate_dir.join("src/lib.rs"), src).unwrap();
        }

        root.join("Cargo.toml")
    }

    #[test]
    fn classify_defaults_to_service() {
        let tmp = TempDir::new().unwrap();
        let manifest = make_workspace(&tmp);
        let workspace = discover(&manifest, &DiscoverConfig::default()).unwrap();

        assert_eq!(workspace.members.len(), 4);
        assert!(workspace
            .members
            .iter()
            .all(|c| c.kind == CrateKind::Service));
    }

    #[test]
    fn classify_respects_ignore() {
        let tmp = TempDir::new().unwrap();
        let manifest = make_workspace(&tmp);
        let config = DiscoverConfig {
            ignore: vec!["bench-*".to_string()],
            ..Default::default()
        };
        let workspace = discover(&manifest, &config).unwrap();

        assert_eq!(workspace.members.len(), 3);
        assert!(workspace.members.iter().all(|c| c.name != "bench-d"));
    }

    #[test]
    fn classify_libraries_separate_from_services() {
        let tmp = TempDir::new().unwrap();
        let manifest = make_workspace(&tmp);
        let config = DiscoverConfig {
            services: vec!["service-*".to_string()],
            libraries: vec!["lib-*".to_string()],
            ignore: vec!["bench-*".to_string()],
            ..Default::default()
        };
        let workspace = discover(&manifest, &config).unwrap();

        let services: Vec<_> = workspace.services().map(|c| c.name.as_str()).collect();
        let libraries: Vec<_> = workspace.libraries().map(|c| c.name.as_str()).collect();
        assert_eq!(services, vec!["service-a", "service-b"]);
        assert_eq!(libraries, vec!["lib-c"]);
    }

    #[test]
    fn explicit_services_excludes_unmatched() {
        let tmp = TempDir::new().unwrap();
        let manifest = make_workspace(&tmp);
        let config = DiscoverConfig {
            services: vec!["service-a".to_string()],
            ..Default::default()
        };
        let workspace = discover(&manifest, &config).unwrap();
        let names: Vec<_> = workspace.members.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, vec!["service-a"]);
    }

    #[test]
    fn source_files_include_only_src_by_default() {
        let tmp = TempDir::new().unwrap();
        let manifest = make_workspace(&tmp);
        let crate_dir = tmp.path().join("service-a");
        fs::create_dir_all(crate_dir.join("tests")).unwrap();
        fs::write(crate_dir.join("tests/it.rs"), "fn it() {}\n").unwrap();

        let workspace = discover(&manifest, &DiscoverConfig::default()).unwrap();
        let svc = workspace
            .members
            .iter()
            .find(|c| c.name == "service-a")
            .unwrap();
        assert_eq!(svc.source_files.len(), 1, "default skips tests/");
        assert!(svc.source_files[0].ends_with("src/lib.rs"));
    }

    #[test]
    fn source_files_include_tests_when_opted_in() {
        let tmp = TempDir::new().unwrap();
        let manifest = make_workspace(&tmp);
        let crate_dir = tmp.path().join("service-a");
        fs::create_dir_all(crate_dir.join("tests")).unwrap();
        fs::write(crate_dir.join("tests/it.rs"), "fn it() {}\n").unwrap();

        let config = DiscoverConfig {
            include_test_files: true,
            ..Default::default()
        };
        let workspace = discover(&manifest, &config).unwrap();
        let svc = workspace
            .members
            .iter()
            .find(|c| c.name == "service-a")
            .unwrap();
        assert_eq!(svc.source_files.len(), 2);
    }

    #[test]
    fn members_sorted_alphabetically() {
        let tmp = TempDir::new().unwrap();
        let manifest = make_workspace(&tmp);
        let workspace = discover(&manifest, &DiscoverConfig::default()).unwrap();
        let names: Vec<_> = workspace.members.iter().map(|c| c.name.as_str()).collect();
        let mut sorted = names.clone();
        sorted.sort_unstable();
        assert_eq!(names, sorted);
    }

    #[test]
    fn invalid_glob_returns_typed_error() {
        let tmp = TempDir::new().unwrap();
        let manifest = make_workspace(&tmp);
        let config = DiscoverConfig {
            services: vec!["[unclosed".to_string()],
            ..Default::default()
        };
        let err = discover(&manifest, &config).unwrap_err();
        assert!(matches!(err, DiscoverError::InvalidGlob { .. }));
    }
}
