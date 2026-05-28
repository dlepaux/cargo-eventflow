## 1.0.0 (2026-05-28)

### Features

* **analysis:** callsite extraction with arity + arg-shape filter (story 02) ([fb9e673](https://github.com/dlepaux/cargo-eventflow/commit/fb9e67317275e1269637bb08dd93836dbea33db8))
* **analysis:** subject resolution — 5 resolvers + bounded recursion (story 03) ([2d7642f](https://github.com/dlepaux/cargo-eventflow/commit/2d7642faca9a6c33e75af45a46b6132f93d911b4)), closes [#3](https://github.com/dlepaux/cargo-eventflow/issues/3) [#4](https://github.com/dlepaux/cargo-eventflow/issues/4)
* **ci:** add self-hosted CI pipeline with semantic-release and kellnr publishing ([f1396b9](https://github.com/dlepaux/cargo-eventflow/commit/f1396b9b1525f15a6abfe3d4544a817c158bad22))
* **ci:** upgrade to rust-crate@v1.6.1 with is_cli_tool — skip println lint for CLI binaries ([ea056a0](https://github.com/dlepaux/cargo-eventflow/commit/ea056a0f5e08b32eb21e3cb328b00fa1fe8b1c36))
* **cli:** wire mermaid + json subcommands to library API + config loader ([40800a9](https://github.com/dlepaux/cargo-eventflow/commit/40800a90241761bce82e2c481d24ded863a3799c))
* **deps:** bump cargo_metadata 0.18 → 0.23 — pkg.name is now typed PackageName ([a901640](https://github.com/dlepaux/cargo-eventflow/commit/a901640797df36c533a0928bf6ed8ce425a5c450)), closes [#4](https://github.com/dlepaux/cargo-eventflow/issues/4)
* **deps:** bump thiserror 1.0.69 → 2.0.18 ([572fc3a](https://github.com/dlepaux/cargo-eventflow/commit/572fc3a62939fc3aca7728985039abaa6832c228)), closes [#2](https://github.com/dlepaux/cargo-eventflow/issues/2)
* **discover:** workspace discovery + symbol_index (story 01) ([475fa34](https://github.com/dlepaux/cargo-eventflow/commit/475fa3470e157d63ea89f5d21d175dfa8a0ab639)), closes [#3](https://github.com/dlepaux/cargo-eventflow/issues/3)
* **emit:** collision-free Mermaid id disambiguator (P1 commit 1) ([73e68aa](https://github.com/dlepaux/cargo-eventflow/commit/73e68aa72ea4aae69ceae58e2c9571f81f2912b9)), closes [#1](https://github.com/dlepaux/cargo-eventflow/issues/1)
* **emit:** Mermaid emitter — themes + flow direction + determinism (story 04) ([4e35aab](https://github.com/dlepaux/cargo-eventflow/commit/4e35aabb340c33461d10b781a9e9387a0627f8f4))
* **graph,emit:** EdgeKind::Matches sweep + undirected Mermaid edges (P1 commit 4) ([f711f41](https://github.com/dlepaux/cargo-eventflow/commit/f711f4132b395d21b745b1482f32f17ef2399f57)), closes [#94a3b8](https://github.com/dlepaux/cargo-eventflow/issues/94a3b8) [#6a3d9](https://github.com/dlepaux/cargo-eventflow/issues/6a3d9) [#999](https://github.com/dlepaux/cargo-eventflow/issues/999)
* **graph:** orphan ingress/egress detection via NatsPattern overlap (P2) ([80f50f2](https://github.com/dlepaux/cargo-eventflow/commit/80f50f2b994c156232dab39ca03e858d2843aec9))
* **model,graph,emit:** thread NatsPattern through Node::Subject (P1 commit 3) ([7912a05](https://github.com/dlepaux/cargo-eventflow/commit/7912a05caba5f71f376e8577ef071cc843df9e45))
* **model:** NatsPattern subject algebra module (P1 commit 2) ([d2fca42](https://github.com/dlepaux/cargo-eventflow/commit/d2fca42c3ac17149b156cf12917916590cffef26))

### Bug Fixes

* add allow-reason comments for CI lint gate compliance ([b4d305c](https://github.com/dlepaux/cargo-eventflow/commit/b4d305c73ea7817b6778bd6be9753443e55f16ae))
* add forbid(unsafe_code) crate attribute for CI gate compliance ([548f266](https://github.com/dlepaux/cargo-eventflow/commit/548f266fca647b52168e62f26cee1fa1d0d38754))
* add forbid(unsafe_code) to main.rs — CI checks all crate roots ([33c79ee](https://github.com/dlepaux/cargo-eventflow/commit/33c79ee314cfe0de0bf17363dca9ccd8d6345571))
* add println annotations for CI gate compliance ([20db8bb](https://github.com/dlepaux/cargo-eventflow/commit/20db8bb02201dc34105f4c8f7dff7a40266276af))
* **deps:** add serde + std features to toml — from_str moved under serde gate in toml 1.x ([21c7435](https://github.com/dlepaux/cargo-eventflow/commit/21c7435a420e83f7515205df695d467cb6f68dc6))
* **emit:** banner as Mermaid %% comment instead of HTML comment ([f14cb84](https://github.com/dlepaux/cargo-eventflow/commit/f14cb8453d59b5a583aaf7b1115b6f0fd5cc95c3))
* **graph:** resolve consumer-name expressions + plumb diagnostics through CLI ([ddaa35f](https://github.com/dlepaux/cargo-eventflow/commit/ddaa35fd865b9efa6fe993b05b865be35b908fa1))
* resolve clippy pedantic + nursery warnings for CI compliance ([e645447](https://github.com/dlepaux/cargo-eventflow/commit/e64544723b150098f7d3b1cae643931dde722a1c))

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
