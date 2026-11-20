//! Codex-tui's live shimmer presentation.
//!
//! Libshimmer owns the recipes, shaders, stage, clock and paint. This module owns the process
//! singleton, the feed measuring the displayed thread's tokens, terminal color detection, the
//! presentation gate, the picker and the reel.
//!
//! Each TUI engages a random stock recipe at startup and `/shimmer` swaps it. A recipe that listens
//! hears what the speakers play through `PipeWire`, and is offered only where `PipeWire` runs.
//! A random roll can pick a listening recipe only when a media player reports active playback.
//! Until one is engaged, as in tests, and wherever the terminal lacks 24-bit color, the classic
//! sweep remains. Reduced or frozen windows never advance the shimmer; returning to Normal does not
//! replay their hidden time.

mod ears;
mod live;
mod picker;
mod playback;
#[cfg(test)]
mod reel;

pub(crate) use live::Earplug;
pub(crate) use live::bullet;
pub(crate) use live::dress_status;
pub(crate) use live::engage_random;
pub(crate) use live::follow_presentation;
pub(crate) use live::follow_thread;
pub(crate) use live::observe;
pub(crate) use live::shade_status;
pub(crate) use picker::picker;
