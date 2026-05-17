# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added
- Initial scaffold: dual-licensed (MIT OR Apache-2.0) Cargo crate
  with both `[lib]` and `[[bin]]` targets.
- Public lib surface stubs (`analyze`, `emit_mermaid`).
- CLI skeleton with 7 subcommands: `mermaid`, `dot`, `d2`, `json`,
  `check`, `explain`, `init`.
- `STABILITY.md` locking output format versioning, exit codes,
  determinism invariants, and CLI grammar from `0.1.0`.
- Public-crate quality floor: deny config, MSRV declaration,
  rustfmt + clippy configs, OS matrix CI, dependabot, contributor
  docs.

### Roadmap
- `0.1.0`: full analyzer + Mermaid emit + `check` subcommand.
  Tracked across stories 00-10 in
  `gordon-workspace/plan/active/cargo-eventflow/`.
