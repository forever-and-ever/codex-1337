# Zip bzip2 backend feature forwarding

This is the published zip 9.0.5 package from https://crates.io/crates/zip/9.0.5,
sourced from https://github.com/zip-rs/zip2.

- Upstream commit: `0123ed144cfa7d672dae78b79a034d8900f9aaeb`
- Archive SHA-256: `6e04976da629d3b04204ba8105b396a193af8ad2583b2c79bfb3ab8af580a11e`, verified against the upstream Codex rust-v0.171.0 Cargo.lock before extraction.
- License: MIT; LICENSE and upstream README are retained.

The workspace excludes this directory from membership and patches crates.io
zip to this path. The Rust sources are the published archive's sources with
repository rustfmt applied. Cargo.lock and Cargo.toml.orig are retained; repository-only
development/release/scanning metadata is omitted.

## Local delta

In Cargo.toml and Cargo.toml.orig, the bzip2-rs feature forwards
`bzip2?/bzip2-sys` instead of `bzip2/bzip2-sys`. Its existing `dep:bzip2`
and `_bzip2_any` entries are unchanged. Weak forwarding avoids activating
zip's sibling bzip2 feature, which otherwise enables bzip2/default and
compiles an unused Rust backend alongside the selected C backend.

Package identity, dependency ranges, public API, archive codecs and the
bzip2-sys C implementation are unchanged. No library code is patched.
Two trailing spaces in upstream SECURITY.md are normalized for the repository
whitespace gate; its security policy text is unchanged.

Remove this vendor and the workspace patch once an upstream release fixes
the forwarding edge and the existing archive checks confirm the same
codec/backend graph. Until then, dependency updates must preserve this edge.
