//! Shimmers: the animation that dresses codex-tui's live rows.
//!
//! A shimmer paints the status row (its mark, its header, the cells after the header, and a backdrop
//! behind the row) and the one-cell bullet of each running tool. The engine owns time, the pace of
//! tokens entering the displayed thread's context and whether the agent is busy or waiting on a tool,
//! and hands them to the shimmer every 32 ms tick as a [`stage::Stage`], the way a shader receives
//! its uniforms; `stage` is that interface and `kit` the patterns stock shimmers share. Nothing
//! outside this module knows which shimmer runs or how.
//!
//! Each TUI engages a random stock shimmer at startup and `/shimmer` swaps it. Until one is engaged, as
//! in tests, and wherever the terminal lacks 24-bit color, the classic sweep remains. Reduced or frozen
//! windows do not run the shimmer clock; reduced motion keeps the existing static fallbacks.
//! To add a shimmer, implement `stage::Shimmer` in a new file beside a `RECIPE` and list it in
//! `ROSTER`.

mod critter;
mod engine;
mod fireflies;
mod forge;
mod kit;
mod lava_lamp;
mod live;
mod neon;
mod picker;
mod prism;
mod rubik;
mod script;
mod stage;
mod supernova;
mod tetromino;

pub(crate) use live::dress_status;
pub(crate) use live::engage_random;
pub(crate) use live::follow_presentation;
pub(crate) use live::observe;
pub(crate) use live::shade_status;
pub(crate) use live::tool_mark;
pub(crate) use picker::picker;

/// The stock shimmers, in the order `/shimmer` lists them.
const ROSTER: [stage::Recipe; 9] = [
    lava_lamp::RECIPE,
    critter::RECIPE,
    rubik::RECIPE,
    tetromino::RECIPE,
    forge::RECIPE,
    supernova::RECIPE,
    fireflies::RECIPE,
    neon::RECIPE,
    prism::RECIPE,
];
