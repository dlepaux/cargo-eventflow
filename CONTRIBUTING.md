# Contributing to cargo-eventflow

Thanks for your interest! Bug reports, feature requests, and PRs
all welcome.

## Dev setup

```bash
git clone https://github.com/dlepaux/cargo-eventflow
cd cargo-eventflow
cargo build
cargo test
```

MSRV: see `rust-version` in `Cargo.toml`. CI matrix runs stable,
beta, and the MSRV toolchain.

## Before opening a PR

```bash
cargo fmt
cargo clippy --all-targets -- -D warnings
cargo test
cargo deny check
```

If any of these fail locally, CI will reject the PR.

## Commit messages

Conventional Commits: `feat:`, `fix:`, `docs:`, `chore:`,
`refactor:`, `test:`, `perf:`, `ci:`, `build:`. Scope when
relevant: `feat(emit): add monochrome theme`. Messages explain
**why**, not what — the diff shows the what.

## PR workflow

1. Fork + branch from `main`.
2. Add tests for behaviour changes (snapshot for emit changes,
   `expect-test` for resolver changes, behaviour test for CLI
   changes).
3. Update `CHANGELOG.md` under `[Unreleased]`.
4. Open the PR with a clear description; link to the issue if
   one exists.
5. Address review comments; one approval merges.

## Release flow

Maintainers only. `release-plz` auto-creates release PRs from
conventional commits on `main`. Merging the release PR publishes
to crates.io + creates the GitHub release.

## Stability discipline

Read `STABILITY.md` before changing:
- CLI grammar
- `.eventflow.toml` schema
- Mermaid output format
- JSON envelope shape
- Exit codes
- Determinism invariants

Each of these binds users; breaking them requires a version bump
per `STABILITY.md`.

## Design discipline

This project is "Gordon-shaped first, generalisable second"
(see the epic in `gordon-workspace/plan/active/cargo-eventflow/`).
Concretely:
- Defaults work on Gordon's actual shape out-of-the-box.
- Every Gordon-specific assumption is reachable via
  `.eventflow.toml`, not hardcoded.
- Adding a new backend (Kafka, in-process channels) is deferred
  to v0.2 — do not preemptively add `BusDialect` trait machinery.

When in doubt, the principle is YAGNI on speculative
abstractions. New patterns earn their place by solving a
present problem, not a hypothetical one.
