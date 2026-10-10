# Codex-1337 fork reference

Sections routed from `AGENTS.override.md`.

## Glossary

These terms have several meanings in the code or name fork concepts. Use the
qualified forms.

- native: never used alone. Native history
  (`codex-rs/tui/src/app/native_history.rs`) is output written to the terminal's own
  scrollback. Provider-native items are a model provider's own wire format and
  state (`NativeToolCall`, `NativeCompaction`), and native Claude is the provider
  that calls the Anthropic Messages API directly. A native execution result is the
  structured result of Codex's own `exec_command` and `write_stdin` tools.
- owned: the owned screen is the fullscreen renderer, which owns the terminal
  screen. A Code Mode owner (`CodeModeOwner`) identifies the Code Mode execution
  that issued a nested item.
- receipt: a record that an operation took place, with its outcome, named after
  the operation (`FileChangeReceipt`, `DeliveryReceipt`). A status that may still be
  pending or may have failed is not a receipt; the TUI's `FarmReceipt` is misnamed.
- family: a root conversation and its subagent descendants.
- farm: the user's direct delegation of a task to a new subagent, started fresh or
  forked with full history, without a turn of the parent agent.
- retire, unload, close: retiring removes an idle subagent subtree from its family
  permanently and keeps its saved history. Unloading drops a thread's in-memory
  runtime and keeps it in its family. `ThreadClosed` reports that a runtime
  stopped, not a change of membership.
- reload, idle restart: `/reload` replaces the TUI process through the reload host
  without detaching its server connection. An idle restart replaces it after five
  quiet minutes when a newer installation exists, through the reload host when one
  is present (hosted) and by `exec` otherwise (unhosted).

## Build inputs

Use the maintained PATH `cargo` wrapper. It enables workspace feature unification
on pinned Cargo without changing rustc's channel. Do not set `RUSTFLAGS`,
`CARGO_ENCODED_RUSTFLAGS`, per-call linker/profile overrides, or `RUSTC_BOOTSTRAP`.
Our root Cargo config owns clang+mold and sccache; fix that configuration instead
of creating another dependency graph. Linked worktrees share these settings.

A dev-dependency must not change the features of any normal build unit, or
`cargo test` compiles a second copy of everything above it.
`scripts/check_feature_parity.py` fails on such a divergence; raise the feature in
`codex-rs/feature-floor` unless that would change shipped behavior.

The wrapper invokes `.cargo/launch` for builds.
It uses the packaging resolver for the V8 version in `Cargo.lock`, paired bindings
and trusted checksums. Metadata/configuration probes do not fetch artifacts.
Keep V8 artifact policy in `scripts/codex_package/v8.py`.

Use the default dev/test profiles for focused checks and `scripts/install-local`
for installs (`local` profile). Do not alternate ad hoc profiles. Keep `-p`/test
filters narrow: workspace feature unification stabilizes dependencies without
building or running the whole workspace. Shared compiler cache and Cargo targets
are disposable; durable release bundles and evidence belong outside them.

## Snapshots

For snapshot changes, iterate with the relevant snapshot test or a narrow filter,
such as `just test -p codex-tui <filter>`. Of the `cargo insta`
subcommands, only `test` accepts `-p`; the others select the crate with
`--manifest-path`. From `codex-rs`:

- `cargo insta pending-snapshots --manifest-path tui/Cargo.toml` lists pending snapshots.
- `cargo insta show path/to/file.snap.new` previews one.
- `cargo insta accept --manifest-path tui/Cargo.toml --snapshot <snapshot-file-name>`
  accepts one reviewed snapshot; without `--snapshot` it accepts every pending
  snapshot in the crate.

## Local TUI releases

Install through `scripts/install-local`. Each new installed binary receives the next
`tui-N`, starting at zero; builds and tests do not increment it. Reinstalling identical
binary bytes reuses the number. The allocator is durable XDG state at
`codex-1337/tui-builds.sqlite`, never Cargo scratch. Do not reset it during cleanup.
The immutable bundle's `distribution` manifest carries `tui_revision`; preserve that
manifest when copying a release. The status line pairs the connected server version
with the running executable's revision, never the latest installation's revision.
Uninstalled development binaries display `tui-dev`.

`scripts/install-local --staging` installs a trial build beside production: a bundle
under `codex-1337/staging/distributions` whose `distribution` manifest carries
`channel=staging`, the `codex-staging` launcher, and `codex-app-server-staging.service`,
which every staging install restarts. Each channel publishes into and collects only its
own root, so no production install touches staging, even from a checkout that predates
it. The executable reads its channel only from that manifest. Staging keeps its daemon's
socket, startup lock and recovery record under `~/.codex/staging`, runs without remote
control, and refuses daemon and remote-control subcommands.
Configuration, credentials, sessions and SQLite state stay shared, so either
installation opens any thread, omitting stored history items its build cannot read.
Continue a thread only in a build at least as new as the last one that wrote it: an
older build also drops model input it cannot parse. `scripts/uninstall-local
--staging` removes staging alone.

## Provider data

OpenAI and Anthropic are equally trusted with conversation data. A privacy or
data-locality rule either permits both or forbids both. Moving a conversation's
data from one to the other, for example to an approval reviewer, a subagent or a
summary, needs no separate privacy decision.
