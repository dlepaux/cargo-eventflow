// cargo-eventflow CLI entry — stub for story 00.
// Each subcommand currently prints "unimplemented" and exits 0.
// Real subcommand wiring lands in stories 01-09.

#![deny(rust_2018_idioms)]
#![warn(clippy::pedantic)]

use clap::{Parser, Subcommand};
use std::path::PathBuf;
use std::process::ExitCode;

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

    /// Path to the .eventflow.toml config (default: workspace-root/.eventflow.toml).
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
        /// Wrap output in a Markdown fenced code block (\`\`\`mermaid).
        #[arg(long)]
        markdown: bool,
        /// Include the cargo-eventflow version in the @generated banner.
        #[arg(long)]
        include_version_stamp: bool,
    },

    /// Generate a Graphviz DOT diagram (requires `dot` feature).
    Dot {
        #[arg(short, long)]
        output: Option<PathBuf>,
    },

    /// Generate a D2 diagram (requires `d2` feature).
    D2 {
        #[arg(short, long)]
        output: Option<PathBuf>,
    },

    /// Print discovered services, subjects, and call sites as JSON.
    Json {
        #[arg(short, long)]
        output: Option<PathBuf>,
    },

    /// Check the committed diagram against the current source (CI mode).
    Check {
        /// File to compare against (default: docs/event-flow.md).
        #[arg(short, long)]
        against: Option<PathBuf>,
        /// Format the committed file uses (default: mermaid).
        #[arg(short, long, default_value = "mermaid")]
        format: String,
        /// Compare the wrapped (markdown-fenced) form.
        #[arg(long)]
        markdown: bool,
        /// Fail on any individual file parse error (default: warn + skip).
        #[arg(long)]
        strict_parse: bool,
        /// Fail if any subject resolves to `?` (unresolved).
        #[arg(long)]
        deny_dynamic: bool,
        /// Fail if any annotation is orphaned (annotation-drift gate).
        #[arg(long)]
        strict_annotations: bool,
    },

    /// Print the resolution path for a single subject pattern.
    Explain {
        /// Subject pattern to explain (supports `*` and `>` wildcards).
        subject: String,
    },

    /// Generate a starter .eventflow.toml by auto-detecting the workspace shape.
    Init {
        /// Overwrite an existing .eventflow.toml.
        #[arg(long)]
        force: bool,
    },
}

fn main() -> ExitCode {
    let cli = match parse_cli() {
        Ok(cli) => cli,
        Err(code) => return code,
    };

    init_tracing(cli.verbose);

    let command = cli.command.unwrap_or(Command::Mermaid {
        output: None,
        markdown: false,
        include_version_stamp: false,
    });

    match command {
        Command::Mermaid { .. }
        | Command::Dot { .. }
        | Command::D2 { .. }
        | Command::Json { .. }
        | Command::Check { .. }
        | Command::Explain { .. }
        | Command::Init { .. } => {
            eprintln!(
                "cargo-eventflow {} — subcommand not yet implemented",
                env!("CARGO_PKG_VERSION")
            );
            eprintln!("See https://github.com/dlepaux/cargo-eventflow for roadmap.");
            ExitCode::from(0)
        }
    }
}

/// Handle cargo's "cargo-X eventflow ..." invocation pattern: when
/// invoked as `cargo eventflow ...`, cargo passes "eventflow" as
/// the first argument. Strip it before clap parses.
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
