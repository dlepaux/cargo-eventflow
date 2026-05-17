# cargo-eventflow

> **Status:** v0.0.x scaffold. The analyzer is not yet implemented —
> see the [roadmap](#roadmap). This README will be filled with screenshots
> and copy-paste CI snippets once v0.1.0 ships.

[![CI](https://github.com/dlepaux/cargo-eventflow/actions/workflows/ci.yml/badge.svg)](https://github.com/dlepaux/cargo-eventflow/actions/workflows/ci.yml)
[![Crates.io](https://img.shields.io/crates/v/cargo-eventflow.svg)](https://crates.io/crates/cargo-eventflow)
[![License: MIT OR Apache-2.0](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue.svg)](#license)

Auto-derive event-flow diagrams from Rust source for NATS-based
systems. Walks your workspace, extracts publish/subscribe call sites,
resolves subject patterns, and emits a Mermaid diagram of the
event flow. `cargo eventflow check` gates CI on diagram drift —
same governance pattern as `cargo fmt --check`.

## What problem this solves

NATS-based Rust systems lose their event topology in code within
months. Subjects are string-formatted at call sites, durable
consumer names are passed at boot, and the only place the full
publish/consume graph exists is in the operator's head — plus,
sometimes, a hand-drawn Mermaid diagram in `docs/` that goes stale
on the next refactor.

`cargo-eventflow` is the missing executable contract: **the
diagram is generated from the AST**, and CI fails if the committed
diagram drifts.

## Why static analysis instead of annotations

Doc-comment annotations (`@publishes foo`) are tempting but have a
fatal flaw: **they don't validate**. You write
`@publishes risk.commands`, refactor publish away, annotation stays.
Same drift mode you wanted to escape from hand-drawn diagrams. No
gate catches it.

AST extraction can't drift — regenerated each run from the actual
code. Catches inconsistencies for free (publisher without consumer,
consumer without publisher). Doc-comment annotations stay in the
toolkit as the **escape hatch** for the 5% AST can't recover (macros,
runtime-bound subjects, ingress/egress on functions).

## Quickstart (preview — once v0.1 ships)

```bash
cargo install cargo-eventflow
cd your-workspace
cargo eventflow init               # auto-detect publisher/consumer shape
cargo eventflow mermaid > docs/event-flow.md
```

CI:

```yaml
- uses: dlepaux/cargo-eventflow/.github/actions/cargo-eventflow@v0.1
```

## Output preview

`graph LR` Mermaid with ingress trapezoids, publisher-grouped
subgraphs, dashed consume edges, egress trapezoids. Theme system
includes `default`, `high-contrast` (WCAG AA), and `monochrome`
(for PDFs / colour-blind audiences). Full preview ships with v0.1
README.

## Configuration

`.eventflow.toml` at workspace root. Minimum viable file:

```toml
[bus]
[[bus.publisher]]
kind = "trait"
path = "my_bus::Publisher"
method = "publish"

[[bus.consumer]]
kind = "trait"
path = "my_bus::Consumer"
method = "subscribe"
```

Full annotated reference: see [`examples/`](examples/) once
v0.1 ships.

## Roadmap

v0.1 ships when:
- Real-world workspace (15-crate Rust + NATS) renders correctly
  in <1s warm.
- `cargo eventflow check` runs as a CI gate (composite GitHub
  Action included).
- Cross-platform: Linux + macOS + Windows.

Stories: see the epic at
[`gordon-workspace/plan/active/cargo-eventflow/`](https://github.com/dlepaux/cargo-eventflow/issues)
(epic to be split into GitHub issues before story 01 work
starts).

| Story | Status |
|---|---|
| 00 — Scaffold | this commit |
| 01 — Workspace discovery + symbol index | not started |
| 02 — Call-site extraction with arity + arg-shape filter | not started |
| 03 — Subject resolution (5 resolvers + bounded recursion) | not started |
| 04 — Mermaid emit (theme + flow direction + determinism) | not started |
| 05 — `check` subcommand (+ annotation-drift gate) | not started |
| 06 — Config + `init` auto-detect + annotations | not started |
| 07 — 22 fixtures + criterion benches + cross-platform CI | not started |
| 08 — CI artifacts (composite Action + pre-commit hook) | not started |
| 09 — Observability (`--timing` + `tracing` + `--explain`) | not started |
| 10 — Real-world integration + explainer | not started |

## Stability

Format version + tool semver promises live in
[STABILITY.md](STABILITY.md). The short version: format version
(`mermaid-v1`) is stable for the `0.x` lifetime; breaking format
bumps go to `mermaid-v2` with a minor cycle of dual emit.

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md). Code of conduct:
[CODE_OF_CONDUCT.md](CODE_OF_CONDUCT.md). Security policy:
[SECURITY.md](SECURITY.md).

## License

Dual-licensed under either of:

- [MIT License](LICENSE-MIT)
- [Apache License, Version 2.0](LICENSE-APACHE)

at your option.

### Contribution

Unless you explicitly state otherwise, any contribution
intentionally submitted for inclusion in the work by you, as
defined in the Apache-2.0 license, shall be dual-licensed as
above, without any additional terms or conditions.
