# Security Policy

## Supported Versions

While in `0.x`, only the latest minor release receives security
fixes. After `1.0.0` we will support the current major and the
previous major for security fixes.

## Reporting a Vulnerability

`cargo-eventflow` is a static-analysis tool that reads Rust source
on disk. It does not execute analyzed code, does not make network
calls, and does not write anywhere outside its own configured
output path. The realistic vulnerability surface is small:

- Malicious `.eventflow.toml` triggering parser bugs in `toml` or
  `serde`.
- Malicious source code triggering parser bugs in `syn` or
  unbounded recursion in our resolver.
- Path-traversal via crafted `--manifest-path` / `--output`.

If you find one, please **do not** open a public issue. Instead,
email `d.lepaux@gmail.com` with the subject line
`SECURITY: cargo-eventflow`. Acknowledgement within 72 hours;
fix or mitigation plan within 14 days.

## Coordinated Disclosure

We will credit reporters in the release notes unless asked
otherwise.
