// cargo-eventflow CLI entry. v0.1: `mermaid` and `json` wired to
// the full library pipeline. `dot` / `d2` / `check` / `explain` /
// `init` print a "not yet implemented" stub.

#![deny(rust_2018_idioms)]
#![warn(clippy::pedantic)]

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{Context, Result};
use cargo_eventflow::analysis::{build_graph, extract, CallSiteConfig, GraphInputs, SymbolIndex};
use cargo_eventflow::config::{self, Config};
use cargo_eventflow::discover::discover;
use cargo_eventflow::emit::mermaid::{Flags, MermaidOptions};
use cargo_eventflow::emit::render_mermaid;
use clap::{Parser, Subcommand};

/// cargo-eventflow — auto-derive event-flow diagrams from Rust source.
///
/// Static-analysis tool for NATS-based Rust workspaces. Walks the
/// workspace, extracts publish/subscribe call sites, resolves subject
/// patterns, and emits a Mermaid diagram of the event flow.
///
/// See `cargo eventflow check` for CI drift gating.
#[derive(Parser, Debug)]
#[command(
    name = "cargo-eventflow",
    bin_name = "cargo eventflow",
    version,
    about,
    long_about
)]
struct Cli {
    /// Path to the workspace Cargo.toml (default: walks up from cwd).
    #[arg(long, global = true)]
    manifest_path: Option<PathBuf>,

    /// Path to the .eventflow.toml config
    /// (default: workspace-root/.eventflow.toml).
    #[arg(long, global = true)]
    config: Option<PathBuf>,

    /// Verbosity; repeatable: -v info, -vv debug, -vvv trace.
    #[arg(short, long, global = true, action = clap::ArgAction::Count)]
    verbose: u8,

    /// Colour mode (auto | always | never). `NO_COLOR` env overrides.
    #[arg(long, global = true, default_value = "auto")]
    color: String,

    /// Print per-phase timing at end of run.
    #[arg(long, global = true)]
    timing: bool,

    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Generate a Mermaid event-flow diagram (default).
    Mermaid {
        /// Output path (default: stdout).
        #[arg(short, long)]
        output: Option<PathBuf>,
        /// Wrap output in a Markdown fenced code block.
        #[arg(long)]
        markdown: Option<bool>,
        /// Include the cargo-eventflow version in the @generated banner.
        #[arg(long)]
        include_version_stamp: bool,
    },

    /// Generate a Graphviz DOT diagram (deferred to v0.2).
    Dot {
        #[arg(short, long)]
        output: Option<PathBuf>,
    },

    /// Generate a D2 diagram (deferred to v0.2).
    D2 {
        #[arg(short, long)]
        output: Option<PathBuf>,
    },

    /// Print discovered services, subjects, and call sites as JSON.
    Json {
        #[arg(short, long)]
        output: Option<PathBuf>,
    },

    /// Check the committed diagram against the current source (deferred to v0.2).
    Check {
        #[arg(short, long)]
        against: Option<PathBuf>,
        #[arg(short, long, default_value = "mermaid")]
        format: String,
        #[arg(long)]
        markdown: bool,
        #[arg(long)]
        strict_parse: bool,
        #[arg(long)]
        deny_dynamic: bool,
        #[arg(long)]
        strict_annotations: bool,
    },

    /// Print the resolution path for a single subject pattern (deferred to v0.2).
    Explain { subject: String },

    /// Generate a starter .eventflow.toml by auto-detecting the workspace
    /// shape (deferred to v0.2).
    Init {
        #[arg(long)]
        force: bool,
    },
}

fn main() -> ExitCode {
    let mut cli = match parse_cli() {
        Ok(cli) => cli,
        Err(code) => return code,
    };

    init_tracing(cli.verbose);

    let command = cli.command.take().unwrap_or(Command::Mermaid {
        output: None,
        markdown: None,
        include_version_stamp: false,
    });

    match run(&cli, command) {
        Ok(()) => ExitCode::from(0),
        Err(err) => {
            eprintln!("error: {err:#}");
            ExitCode::from(2)
        }
    }
}

fn run(cli: &Cli, command: Command) -> Result<()> {
    match command {
        Command::Mermaid {
            output,
            markdown,
            include_version_stamp,
        } => run_mermaid(cli, output.as_deref(), markdown, include_version_stamp),
        Command::Json { output } => run_json(cli, output.as_deref()),
        Command::Dot { .. } | Command::D2 { .. } => {
            eprintln!("dot / d2 emit deferred to v0.2 — use `mermaid` for now");
            Ok(())
        }
        Command::Check { .. } => {
            eprintln!("check subcommand deferred to v0.2 — regenerate manually and diff via git");
            Ok(())
        }
        Command::Explain { .. } => {
            eprintln!("explain subcommand deferred to v0.2");
            Ok(())
        }
        Command::Init { .. } => {
            eprintln!(
                "init subcommand deferred to v0.2 — copy examples/generic.toml from the repo as a starting point"
            );
            Ok(())
        }
    }
}

fn run_mermaid(
    cli: &Cli,
    output: Option<&Path>,
    markdown_override: Option<bool>,
    include_version_stamp: bool,
) -> Result<()> {
    let manifest = resolve_manifest(cli)?;
    let config_path = resolve_config_path(cli, &manifest)?;
    let config = config::load(&config_path)
        .with_context(|| format!("loading config from {}", config_path.display()))?;

    let (graph, _diagnostics) = analyze_workspace(&manifest, &config)?;

    let markdown = markdown_override.unwrap_or(config.output.markdown);
    let opts = MermaidOptions {
        theme: config::parse_theme(&config.output.mermaid.theme),
        flags: Flags {
            group_by_publisher: config.output.mermaid.group_by_publisher,
            include_ingress_egress: true,
            include_version_stamp,
            markdown,
        },
    };

    let rendered = render_mermaid(&graph, &opts);
    write_output(output, &rendered)?;
    Ok(())
}

#[derive(serde::Serialize)]
struct JsonOut<'a> {
    #[serde(rename = "$schema")]
    schema: &'a str,
    nodes: Vec<serde_json::Value>,
    edges: Vec<serde_json::Value>,
    diagnostics: Vec<String>,
}

fn run_json(cli: &Cli, output: Option<&Path>) -> Result<()> {
    let manifest = resolve_manifest(cli)?;
    let config_path = resolve_config_path(cli, &manifest)?;
    let config = config::load(&config_path)
        .with_context(|| format!("loading config from {}", config_path.display()))?;
    let (graph, diagnostics) = analyze_workspace(&manifest, &config)?;

    let body = JsonOut {
        schema: "https://github.com/dlepaux/cargo-eventflow/schemas/v1.json",
        nodes: graph
            .nodes
            .iter()
            .map(|n| serde_json::json!({ "id": n.id(), "label": n.label() }))
            .collect(),
        edges: graph
            .edges
            .iter()
            .map(|e| {
                serde_json::json!({
                    "from": e.from.id(),
                    "to": e.to.id(),
                    "kind": format!("{:?}", e.kind),
                    "label": e.label,
                })
            })
            .collect(),
        diagnostics: diagnostics.iter().map(|d| format!("{d:?}")).collect(),
    };

    let json = serde_json::to_string_pretty(&body).context("serialising JSON")?;
    write_output(output, &json)?;
    Ok(())
}

fn analyze_workspace(
    manifest: &Path,
    config: &Config,
) -> Result<(
    cargo_eventflow::model::Graph,
    Vec<cargo_eventflow::analysis::GraphDiagnostic>,
)> {
    let ws = discover(manifest, &config.workspace).context("workspace discovery")?;

    let call_cfg = CallSiteConfig {
        method_match: config.bus.method_match,
        publishers: config.bus.publisher.clone(),
        consumers: config.bus.consumer.clone(),
    };

    let mut call_sites = Vec::new();
    let mut index = SymbolIndex::new();
    for member in &ws.members {
        for file in &member.source_files {
            let Ok(src) = std::fs::read_to_string(file) else {
                continue;
            };
            if let Ok(per_file) = extract(file, &src, &member.name, &call_cfg) {
                for site in per_file.sites {
                    call_sites.push((member.name.clone(), site));
                }
                index.merge(per_file.symbols);
            }
        }
    }

    let inputs = GraphInputs {
        call_sites,
        ingress: config.ingress.source.clone(),
        egress: config.egress.sink.clone(),
        helper_crates: config.subjects.helper_crates.clone(),
        subject_builder_methods: config.subjects.subject_builder_methods.clone(),
        subject_builder_functions: config.subjects.subject_builder_functions.clone(),
    };

    Ok(build_graph(&inputs, &index))
}

/// Resolve the workspace manifest path. Defaults to `./Cargo.toml`;
/// if absent, climbs up to the first ancestor containing one.
fn resolve_manifest(cli: &Cli) -> Result<PathBuf> {
    if let Some(p) = &cli.manifest_path {
        return Ok(p.clone());
    }
    let cwd = std::env::current_dir().context("getting current directory")?;
    let mut dir = cwd.as_path();
    loop {
        let candidate = dir.join("Cargo.toml");
        if candidate.exists() {
            return Ok(candidate);
        }
        match dir.parent() {
            Some(p) => dir = p,
            None => anyhow::bail!("no Cargo.toml found in {} or any ancestor", cwd.display()),
        }
    }
}

/// Resolve the config path. Priority: explicit `--config` flag,
/// then `<workspace_root>/.eventflow.toml`.
fn resolve_config_path(cli: &Cli, manifest: &Path) -> Result<PathBuf> {
    if let Some(p) = &cli.config {
        return Ok(p.clone());
    }
    let workspace_root = manifest
        .parent()
        .context("manifest path has no parent")?
        .to_path_buf();
    Ok(workspace_root.join(".eventflow.toml"))
}

fn write_output(path: Option<&Path>, content: &str) -> Result<()> {
    if let Some(p) = path {
        std::fs::write(p, content).with_context(|| format!("writing output to {}", p.display()))
    } else {
        let stdout = std::io::stdout();
        let mut handle = stdout.lock();
        handle
            .write_all(content.as_bytes())
            .context("writing stdout")
    }
}

/// Handle cargo's "cargo-X eventflow ..." invocation pattern.
fn parse_cli() -> Result<Cli, ExitCode> {
    let mut args: Vec<std::ffi::OsString> = std::env::args_os().collect();
    if args.len() >= 2 && args[1] == "eventflow" {
        args.remove(1);
    }
    match Cli::try_parse_from(args) {
        Ok(cli) => Ok(cli),
        Err(err) => {
            let _ = err.print();
            Err(ExitCode::from(if err.use_stderr() { 2 } else { 0 }))
        }
    }
}

fn init_tracing(verbosity: u8) {
    use tracing_subscriber::{fmt, EnvFilter};

    let default_level = match verbosity {
        0 => "warn",
        1 => "info",
        2 => "debug",
        _ => "trace",
    };

    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new(format!("cargo_eventflow={default_level}")));

    fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .without_time()
        .init();
}
