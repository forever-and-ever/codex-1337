# Zip bzip2 backend feature forwarding

This is the published zip 9.0.0 package from https://crates.io/crates/zip/9.0.0,
sourced from https://github.com/zip-rs/zip2.

- Upstream commit: `2b1c4244a6f42906c8b3f21a358bf2321d0baaef`
- Archive SHA-256: `711e8c9646791c49b7eb799a19c4ae70a9b8372ddc6a1fa4778defd1e2aa5d5e`, verified against the workspace Cargo.lock
  before extraction.
- License: MIT; LICENSE and upstream README are retained.

The workspace excludes this directory from membership and patches crates.io
zip to this path. All compiler source is byte-identical to the published
archive. Cargo.lock and the original manifest are retained; repository-only
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
