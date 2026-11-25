# Codex-1337 fork reference

Sections routed from `AGENTS.md`.

## Glossary

These terms have several meanings in the code or name fork concepts. Use the
qualified forms.

- Preamble: the initial instructions, tools and managed context of one context
  window. Injections are subsequent arrivals, not refreshed preambles. Preamble
  starts collapsed and opens its client-only census before a separate source
  fold. Raw shows the retained capture; Fmt uses the terminal's default background
  for its structural or semantic projection, without changing context inclusion
  or meaning. Unsafe terminal controls are
  visibly escaped with a client notice. Tool Declarations contains submitted
  definitions as client JSON, not raw wire bytes or the provider's unobservable
  internal rendering. Its body is model-bound; its heading is human-only.
  Additional-tools Injections keep their definitions behind an independent,
  initially closed Raw Context fold. Older mixed captures
  retain a legacy label rather than guessed section boundaries. User messages,
  assistant prose, disclosed raw reasoning, inter-agent prose and other incoming
  context lie on the worn pack's prompt, response, reasoning, message and
  environment grounds. Verbatim tool-call source also lies on the environment ground; syntax
  highlighting alone remains Raw. Human-only headings,
  summaries, notices and diagnostic decoration
  use the terminal's default background, never grey or reverse video. Author
  color does not determine context inclusion. `/context` opens a read-only Context
  snapshot of retained special material, including superseded copies and amendments;
  it neither injects text nor wakes an unloaded thread. Older windows without a
  saved inspection prefix become inspectable after their next model request.
- alarm: a thread-owned clock or exec-session watch that sounds once and queues
  a model-visible result. An alarm sounding is its delivered result. Heartbeats
  are upstream user-authored scheduled tasks (`UserInputOrigin::Heartbeat`),
  not alarms or prompt-cache warming. `clock.sleep` blocks its current tool call;
  an alarm leaves the agent free to work. XSync desktop attention
  (`desktop_attention.rs`) watches desktop activity, not thread alarm conditions.
- nested tool, roster: a nested tool is one that a Code Mode cell calls as
  `tools.<member>`. The roster lists, by name and one purpose sentence, the nested
  tools whose contracts wait for `describe_tool`; the model reads it as the
  `<nested_tools>` section. Qualify every other roster: agent roster, skill roster.
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
- OpenRouter host, endpoint, provider: a host is a company serving OpenRouter
  models, named by its base endpoint slug (`deepseek`); an endpoint is one of its
  serving units, `host[/variant][/region][/tier]` (`deepinfra/turbo`). OpenRouter's
  own documentation calls hosts "providers"; in Codex a provider is only a model
  provider such as `openrouter`. A saved model's endpoint filter admits all
  endpoints, only listed ones, or all but listed ones. An OpenRouter key is a named
  API key whose secret stays in its file; spend is summed `usage.cost` in dollars,
  and balance is credits minus usage.
- reload, idle restart: `/reload` replaces the TUI process through the reload host
  without detaching its server connection. An idle restart replaces it after five
  quiet minutes when a newer installation exists, through the reload host when one
  is present (hosted) and by `exec` otherwise (unhosted).
- dialogue, work: the dialogue is the user's prompts and the agent's replies: its final
  answers, and any message no final answer follows before the next prompt, so a running,
  interrupted or steered turn keeps what it said. Work is everything else the transcript
  shows. Dialogue mode (Alt+W, `tui.keymap.global.toggle_dialogue`) hides the work in the
  main transcript; detailed and raw views always show it.
- rewind, revert: a rewind is the user choosing an earlier prompt (Alt+Z,
  `tui.keymap.global.rewind`; arrows choose, Enter confirms, Esc leaves) in the
  dialogue lens, which stays on afterwards; the revert is the thread operation the confirmed
  rewind requests, which drops that prompt's turn and everything after it and returns the
  prompt to the composer. Draft undo (F7) and redo (Shift+F7) edit only the unsent
  prompt; neither changes the conversation. Upstream's Esc-Esc backtracking is retired.
- beat, glint, peripheral: the beat is the clock every TUI animation shares
  (`libshimmer::TICK`, 32 ms), so concurrent motion paints in one frame; it is no
  heartbeat. A glint repaints only the status row over the last full frame and writes the
  cells that changed; a full frame repaints the whole screen. A peripheral window is
  visible but unfocused: its content coalesces and only its status row moves, glinting
  every third beat.
- rest row: the status row's place between turns, kept up so that the composer never jumps.
  It says how the last turn in view ended (Finished, Interrupted or Failed, and Ready before
  one, as on a resumed thread) and carries the engaged shimmer's idle paint, glinting as the
  status row does.
- goal box, timebox, quotabox, halt, reword: a goal box is a limit the user sets
  on a root agent's unfinished goal. A timebox ends at a wall-clock deadline and
  reminds the agent each time its time left halves while at least ten minutes
  remain; a quotabox is a floor on the quota the root agent's provider account
  has left: a percent of a subscription's weekly window, or the dollars an
  OpenRouter key can still spend. A halt is a box being reached: the server
  pauses the goal and asks the agent to stop work gracefully, on its honor, and
  no activation succeeds while the box stays reached. To reword is to give an
  unfinished goal a new objective in place (`/goal set`), keeping its usage and
  boxes; a goal's boxes end when it completes or is cleared.

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

For V8 152.2.0 on Linux x86_64 GNU, Cargo uses the repository-owned binding
in `third_party/v8/bindings/152.2.0/`, pinned by SHA-256 in that resolver.
The September Codex release paired a Bazel archive with a pre-generated GN
binding; its CppGC slots and typed-array limits disagree with the archive.
Regenerate only the binding through upstream #57644's bindgen target under
the same `sandbox_release` transition (pointer compression, sandbox, pinned
Chromium libc++), not the source/workspace-runtime profile:

```bash
bazel build -c opt --platforms=@llvm//platforms:linux_x86_64 \
  //third_party/v8:rusty_v8_sandbox_release_binding_x86_64_unknown_linux_gnu
```

Run from the checkout root; copy the reported `binding_generated_152_2_0.rs`
output to the versioned binding path, then format it with the checkout's pinned
Rust toolchain and the same configuration as `scripts/format.py` before hashing:

```bash
binding=third_party/v8/bindings/152.2.0/src_binding_ptrcomp_sandbox_release_x86_64-unknown-linux-gnu.rs
rustfmt --edition 2024 --config-path codex-rs/rustfmt.toml \
  --config imports_granularity=Item,skip_children=true "$binding"
sha256sum "$binding"
```

Pin the formatted output's digest in the resolver. Formatting is part of
regeneration, not a later source change: raw bindgen output has a different hash.
The root `.bazelrc` uses `repo_env=PIP_REQUIRE_VIRTUALENV=false` because host
pip policy otherwise leaks into rules_python's isolated wheel builds, which
install nothing globally.

SHA-256 provenance:
- Published binding before: `3e4d5fecdbe0f98a6017e69d7ca82519a77fb032c5eb5e4cdbecdd3e2ec519d5`.
- Regenerated binding after: `63d6d85dc05dd5c5cdf7e8eb5afe1aa924d09736ef611cbc3ba838553fd4ba90`.
- Published archive unchanged: `b3b490f2aa809eda7f6ac727f851fb00ce8355be72827577cfaac28c7137dbcd`
  (`librusty_v8_ptrcomp_sandbox_release_x86_64-unknown-linux-gnu.a.gz`).
- Published pair manifest unchanged: `1ec730635970ab1c0e8f4764ea113ad29d69d7f8d4c2ae69f3d766434a759350`.

The archive and its published manifest still authenticate against the existing
release pins; only the separately checked binding input changes. Missing or
modified local bindings fail closed, never reverting to the unsafe published
binding.

Use the default dev/test profiles for focused checks and `scripts/install-local`
for installs (`local` profile). Do not alternate ad hoc profiles. Keep `-p`/test
filters narrow: workspace feature unification stabilizes dependencies without
building or running the whole workspace. Shared compiler cache and Cargo targets
are disposable; durable release bundles and evidence belong outside them.

The workspace vendors arboard 3.6.1 for standard X11 `MULTIPLE` clipboard-manager
handoff and advertised UTF-8 aliases. Provenance, local changes, validation and
removal condition live in `codex-rs/vendor/arboard/DOWNSTREAM.md`. Do not replace
this patch with build-time edits to Cargo's shared registry cache.

The workspace vendors zip 9.0.5 for one dependency-feature forwarding fix,
retaining its C bzip2 backend without compiling the unused Rust backend.
Provenance, the exact metadata delta and removal condition live in
`codex-rs/vendor/zip/DOWNSTREAM.md`; the library has no functional changes.

## Saved-thread links

Clicking a `codex://threads/<uuid>` link asks the transcript's app server for
that saved thread. On an i3/Alacritty desktop, Codex focuses a window already
displaying it or starts another Alacritty on the same backend. The originating
chat, overlay and draft stay put. A missing thread or an endpoint that cannot
be safely reopened produces an error, not a local fallback. The URL remains
copyable and is never sent to a web browser.

## Current X11 clipboard keeper

The optional desktop-wide keeper retains only the current CLIPBOARD offer in
memory: no history, disk archive, PRIMARY monitoring or clipboard synchronization.
It hosts the installed `Libxfce4ui.ClipboardManager` API, not a Codex protocol
implementation. Requirements are system Python/PyGObject, GTK 3 and libxfce4ui
4.20 with the `Libxfce4ui-2.0` typelib. The host's `/tmp` must be tmpfs: GTK image
decoding uses temporary files there; the unit fixes `TMPDIR=/tmp`. Memory-only is
not a guarantee against swap or privileged memory inspection.

Setup is separate from `install-local` and does not activate itself:

```sh
python3 scripts/local_integration.py install-clipboard-keeper "$HOME/.local"
systemctl --user daemon-reload
systemctl --user import-environment DISPLAY XAUTHORITY
systemctl --user enable --now codex-clipboard-keeper.service
```

Run activation from the intended X11 session. The existing host `.xinitrc` imports
DISPLAY/XAUTHORITY into the user manager and starts `x11-apps.target`, as for dunst
and picom. The keeper follows that target, not the inactive generic
`graphical-session.target`, app server or individual TUI. There is no restart
loop and it refuses to replace another clipboard manager. Diagnose an unavailable
display or competing owner before explicitly starting it again.

Arboard's generic text/HTML handoff is cooperative on orderly application exit;
SIGKILL and arbitrary crashes are not covered. Offers from other desktop apps
also use this keeper, subject to their supported handoff/target formats. The
current contents disappear when the keeper/session exits. To stop and remove it:

```sh
systemctl --user disable --now codex-clipboard-keeper.service
```

Then remove only the installed `libexec/codex-clipboard-keeper` under the selected
prefix and the `codex-clipboard-keeper.service` user unit, and reload user units.
Codex daemon installation/uninstallation never changes this desktop-wide opt-in.

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

The production and staging managed app servers start with `--managed-daemon`
for restart recovery and channel-owned alarms. Systemd supervision alone does
not enable these facilities.

They also start with `--linux-sandbox-pid-namespace inherit`: their local Linux
tool sandboxes share the host PID namespace and `/proc`, including same-UID
signaling authority.
Filesystem, network, user-namespace and seccomp restrictions remain in force
for directly executed tools; services launched through the host user manager
are outside those tool sandboxes.
Ordinary app-server startup defaults to `isolate`; this trusted startup setting
is not a request permission or repository configuration, and does not change
remote executors' own startup policy. Production adopts changed unit arguments
on its next idle refresh; staging adopts them on its install-time restart.

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

After a user-requested staging trial reaches production, disable the staging service:
`systemctl --user disable --now codex-app-server-staging.service`.

## UI latency recorder

Each TUI worker keeps content-free, fixed-capacity wall-time counters. Inspect it
directly, without its dispatcher, Tokio runtime, reload host or app server:

```sh
codex debug ui list
codex debug ui dump --pid PID
codex debug ui dump --pid PID --json
codex debug ui reset --pid PID --json
```

`list --json` includes responsive worker identities, unresponsive PIDs and a
`partial` flag. Discovery stops after two seconds, 1024 directory entries or
64 worker candidates; use an exact PID when discovery is incomplete. Identity
includes PID, Linux process-start ticks, build commit, installed channel,
terminal device and reload generation. PID/start and same-UID peer credentials
are checked on each request, so PID reuse cannot silently select another worker.

JSON has `schema_version: 1`, fixed stage names, nanosecond durations and
`histogram_upper_ns` bucket bounds. `timing: wall_inclusive` means parent and
child totals overlap: never sum them as disjoint CPU work. `IdleSelect` is
select polling and waiting, normally idle rather than a handler. A slow
synchronous select poll cannot be distinguished from ordinary waiting here. Other stages identify coarse dispatcher
occupancy, key routing, server and delivery completions, frame
preparation/painting, status-row glints (`GlintPaint`), invocation materialization, and terminal
diff/write/flush. The output writer thread measures the terminal stages; the UI
records them when it drains that frame's completion. Retired stage ids
(`PreKeyPaint`, `RpcWait`) stay reserved and never record.
A long handler does not prove CPU work or rendering is responsible. The slow
ring retains the latest 128 completed non-idle spans of at least 8 ms; human
reports mark 50 ms or more prominently. Ring sequence numbers reveal overwritten
history. All completed spans, including faster ones and idle waits, enter fixed
histograms. Input reports the gate reason, three queue sizes and the latest
consumed-input timestamp; it cannot measure physical key-to-consumption latency
while input is gated.

`ReactionLatency` measures the interval from app-side input consumption to
completion of the first frame flush whose painting began after its handler
returned. Key, paste, mouse-button and wheel events count; motion, focus and
resize do not. Inputs eligible for the same frame share one sample, measured
from the earliest input, including time queued behind admission gates. Every
sample enters the fixed histogram; only reactions of at least 16.7 ms enter the
slow ring, while stages retain the 8 ms threshold. JSON includes reactions in
`stages` and `recent_slow`. Human dumps show p50 and p99 upper bounds from the
histogram, capped by the observed maximum. Reset
discards inputs still waiting for a frame. Empty-diff frames still complete a
sample; no frame means no completed sample. This measures neither physical input arrival nor
terminal presentation.

Current top/parent stages are published before work. Last-committed progress
remains separate; a suspended publisher never makes snapshots wait for it.
Each accepted atomic record is consistent, but records are not one simultaneous
cut. `incomplete_records` marks interrupted publications; `suppressed_scopes`
marks nesting beyond the recorder's fixed capacity. An unavailable current
record is not evidence that the UI is idle.

Reset immediately selects a new logical reporting epoch, not a physical memory
clear or remote cancellation. `reset_pending` stays true until the UI next
crosses a boundary. Counters and slow records from older epochs disappear from
the report immediately; spans entered before reset never count toward the new
epoch. Current and last-committed markers deliberately remain visible with their
original epoch, including a handler that never resumes.

The dedicated OS thread sleeps when unused and serves one bounded request at a
time, with a 200 ms total client deadline and fixed-size binary response.
Endpoints are worker-owned `ui-latency-PID-START.sock` files (0600) in the
validated private `codex-uds` user runtime directory. Normal shutdown removes
only that worker's endpoint. SIGKILL may leave a stale socket until runtime
directory expiry; discovery ignores dead process identities and never sweeps
others' files. Listener startup failure logs one warning and does not prevent
the UI from starting.

Normal collection allocates no heap memory, acquires no locks, formats no
strings, logs nothing and writes no files. Recorder state has a compile-time
64 KiB bound, excluding its 128 KiB listener stack and socket buffers. Clocks and
atomic stores still cost time; see measured evidence before claiming numerical
overhead. Dumps default to stdout; redirect JSON explicitly to retain it. There
is no automatic disk stream, saved-report directory or built-in retention policy.

The listener can answer while the UI is synchronously blocked, awaiting a
server response or blocked on terminal output, provided its own OS thread can
run. It is not a crash debugger: SIGSTOP, death, process-wide corruption, severe
resource exhaustion and total CPU starvation can prevent a reply.

## Provider data

OpenAI, Anthropic and OpenRouter are equally trusted with conversation data. A
privacy or data-locality rule either permits all three or forbids all three.
Moving a conversation's data between them, for example to an approval reviewer,
a subagent or a summary, needs no separate privacy decision. OpenRouter forwards
each request to an endpoint the model's filter admits. Forgetting a model removes
it from the picker but retains its endpoint filter. Re-saving restores the picker
entry with that filter; changing the route explicitly replaces it.

OpenRouter bills per token. Codex sends it only requests for models the user
chose, as a conversation's model or a subagent's. Work Codex starts on its own
(approval review, web search, memories, titles, scoring) never runs on
OpenRouter: it runs on the ChatGPT login, natively on the Claude subscription
where Codex has that path, or not at all.

The experimental `thread/sample` RPC asks for one response against a snapshot
of a saved root thread. It does not switch the parent model or launch a
persistent worker. Omitting `model` inherits the source; an explicit alternate
must be admitted on its pinned provider and account and be compatible with
the captured history. An incompatible selector is an error, not permission
to change payer or translate private provider state. The extra question has
no sampler-specific byte cap; context limits and ordinary inference charges
still apply. For a review on another provider today, give a fresh subagent
the material to review rather than forking the full history.

A memory startup job is an independently admitted operation. It captures its
conversation's committed credential authority before any worker is queued and
pins it through extraction (including dual-write) and consolidation. A later
conversation account switch affects future jobs, not admitted jobs. The captured
source may refresh its own credentials; removal or principal replacement fails
closed, never falling back to the server's active login. Noncredential settings
such as the live service tier remain live.

## Warpaint

`[tui] theme` names a warpaint pack, which paints chrome, syntax and tool output alike.
Each conversation follows its family root's saved override, or configuration when that
choice is null. A fixed config pack applies directly; `"random"` derives one stable pack
from the root thread ID using rendezvous hashing, so a family wears one colour across
switches, restarts and machines. Adding a pack only moves families that it wins.
`/theme` previews transiently; cancel restores the prior pack and confirm saves the
family's override through app-server thread metadata. “Follow config” clears it.
Conversations without an override reskin when configuration reloads. Configuration is
reloaded on existing folder/session/config-refresh paths, not watched continuously.
Without an active conversation, fixed config applies and `"random"` wears the default,
`slag`. An unknown pack warns and falls back to `slag`. `docs/warpaint.md` describes the engine.

Speed-tier commands such as `/fast` and their keybindings change the conversation,
not the saved `service_tier` default. Edit configuration explicitly to change defaults.

Removed: foreign `.tmTheme` selection, the embedded `two_face` theme list,
`$CODEX_HOME/themes/*.tmTheme` loading, model themes, adaptive light/dark theme choice,
contrast-corrected colors (upstream's correction still decides whether a color is painted;
the pack decides which), the `codex.accent` scope convention, and unsmelted packs from
`$CODEX_HOME`. `two_face` remains solely for its extended syntax grammars.
