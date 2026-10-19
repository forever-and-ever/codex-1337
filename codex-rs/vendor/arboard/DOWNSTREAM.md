# X11 clipboard handoff compatibility

This is arboard 3.6.1, copied from the crates.io archive:

- Source: https://github.com/1Password/arboard
- Upstream commit: `a3750c79a5d63f3987317e03c412b7d9dffdc2af`
- Archive SHA-256: `0348a1c054491f4bfe6ab86a7b6ab1e44e45d899005de92f58b3df180b36ddaf`
- License: MIT OR Apache-2.0; both license files are retained.

The workspace patches crates.io arboard to this path and excludes it from
workspace membership. Package identity, public API, dependency ranges, platform
implementations and features are retained. Cargo's local cache marker is omitted.

## Local delta

`src/platform/linux/x11.rs` supports ICCCM `MULTIPLE` selection conversion,
which the installed Xfce clipboard manager uses during `SAVE_TARGETS` handoff.
Upstream arboard 3.6.1 does not support it, so even normal text/HTML copies
otherwise disappear on orderly application exit with this manager.

Pair lists must have type `ATOM_PAIR`, format 32, an even atom count and at most
1024 pairs. Oversized/truncated or malformed lists fail before conversion.
Failed conversions set the destination atom to `None`; the original request
receives exactly one `SelectionNotify`. Nested requests and destinations that
alias the pair list are rejected. Legacy single-target requests with no property
use their target atom as the property. UTF-8 MIME aliases already advertised by
upstream are now served. Request writes are checked before reporting success.

PNG reading uses `image::load_from_memory_with_format` instead of the deprecated
`image::io::Reader` alias, retaining compatibility with the image 0.25 range.

The only other source additions are `tests/x11_selection.rs`, its Cargo target,
and a no-retry local nextest profile. The test covers real protocol partial
success/failure, malformed/bounded requests, aliases, notification semantics,
262 KiB text/HTML persistence including INCR replies, PNG retention, independent
PRIMARY ownership and duplicate/clean manager shutdown. It is ignored by default
and requires explicit `CODEX_PRIVATE_X11=1`, `CODEX_CLIPBOARD_KEEPER` pointing to
the repository runner, and a private `CODEX_CLIPBOARD_HOME`.

Run through the installed isolated-X11 skill launcher, never the live DISPLAY:
`just test --manifest-path vendor/arboard/Cargo.toml -p arboard --test x11_selection --run-ignored only`.
The manifest is standalone so its original Cargo.lock and dev-dependencies are
used; ordinary Codex builds still use the workspace lockfile. GUI validation
must retain process, filesystem and network isolation for toolkit children.

Remove this vendor and the workspace patch when an upstream arboard release
provides these compatibility fixes and passes the same protocol/handoff check.
No public fork or upstream publication is implied by this copy.
