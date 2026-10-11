codex-1337: private downstream fork of openai/codex; Linux and fullscreen TUI only

## gate
- Tests: `just test`, never bare `cargo test`; narrowest named test plus `--lib` or `--test <target>`; no whole-crate runs, even for common, core, protocol
- `codex-tui` changes land only after full `just test -p codex-tui --lib` passes
- Full-workspace run: last resort, explicit user approval first; no `--all-features` unless that matrix is needed
- Install: `scripts/install-local` (`local` profile); focused checks on default dev/test profiles; no ad hoc profiles
- Never rerun broad suites to chase timing failures on loaded machine
- Consequential agent-logic change: extend an existing integration case before adding a test

## invariants
- Merge upstream release tags, never upstream `main`; baseline: `git tag --merged HEAD 'rust-v[0-9]*' | sort -V | tail -1`; `origin/main` ≠ baseline
- Fork features in fork-owned files; upstream files get only seams: module decl, call, match arm, field holding fork state
- Fork methods on upstream types: fork-owned submodules; fork behavior: fork-owned traits, never new methods on upstream traits
- Dropped capability: delete all that only served it, set `Stage::Removed`, delete whole files; removal touching many upstream files → one inert stub
- Dead fork-owned code always deleted
- Merge conflict: apply fork intent to upstream's new code; never delete upstream code until tree compiles
- macOS and Windows code kept only to shrink merges: never validated or repaired; delete only for another reason
- Most-broken doctrine rule here: exclusive states are one enum; no flag or `Option` restates a fact another value owns
- No inline-renderer parity; keep upstream legacy code where it eases merges; legacy-only presentation tests not release gates
- Shared rendering contracts, headless `codex exec`, server behavior, model-visible output stay supported and correct
- Transcript: events → owned reducer → immutable cells; never mutate emitted cells by index; replace only via `App::commit_transcript_splice`
- TUI is thin frontend: attach, detach, reconnect, reload never change agent capabilities or whether a conversation accepts work
- Thread coordination and durable delivery belong to app server; new task never waits for frontend; missing frontend never implies approval
- Never set `RUSTFLAGS`, `CARGO_ENCODED_RUSTFLAGS`, `RUSTC_BOOTSTRAP`, or per-call linker/profile overrides; fix root Cargo config instead
- `/tmp` and Cargo targets never hold live IPC, locks, drafts, or sole copies of attachments; use `codex-uds` runtime dirs, Codex home, XDG
- After tests, delete only temp paths your run created; shared owner, empty dir, or empty `lsof` ≠ ownership
- Paste-burst or chat-composer state machine change: keep `chat_composer.rs` and `paste_burst.rs` module docs true to code
- `feature:` prompts: executor works in new `.worktrees/` worktree; primary fixes data model, placement, names, then reviews design fit

## routes
- Merging upstream or resolving merge conflicts → `docs/teleology.md`
- Naming native, owned, receipt, family, farm, retire, unload, close, reload → `docs/fork.md` Glossary
- Changing Cargo inputs, V8 artifacts, or profiles → `docs/fork.md` Build inputs
- Snapshot changes → `docs/fork.md` Snapshots
- Installing or numbering TUI builds → `docs/fork.md` Local TUI releases
- Deciding which providers may receive conversation data → `docs/fork.md` Provider data
