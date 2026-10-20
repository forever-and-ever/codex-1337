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
//! `ROSTER` with its author. `/shimmer` shows that credit beside the blurb.

mod cambrian;
mod critter;
mod engine;
mod fireflies;
mod forge;
mod kit;
mod lava_lamp;
mod lfsr;
mod live;
mod neon;
mod phyllotaxis;
mod picker;
mod prism;
mod quipu;
#[cfg(test)]
mod reel;
mod relay;
mod rubik;
mod sandpile;
mod script;
mod stage;
mod supernova;
mod tectonic;
mod tetromino;
mod turing;

pub(crate) use live::dress_status;
pub(crate) use live::engage_random;
pub(crate) use live::follow_presentation;
pub(crate) use live::observe;
pub(crate) use live::shade_status;
pub(crate) use live::tool_mark;
pub(crate) use picker::picker;

/// A stock shimmer and its author.
struct RosterEntry {
    recipe: stage::Recipe,
    credit: &'static str,
}

const fn credited(recipe: stage::Recipe, credit: &'static str) -> RosterEntry {
    RosterEntry { recipe, credit }
}

const OPUS: &str = "Claude Opus 5.5";
const DEEPSEEK: &str = "DeepSeek V4.1 Flash 1017";
const USER: &str = "The User";

/// The stock shimmers, in the order `/shimmer` lists them.
const ROSTER: [RosterEntry; 17] = [
    credited(lava_lamp::RECIPE, OPUS),
    credited(critter::RECIPE, OPUS),
    credited(rubik::RECIPE, OPUS),
    credited(tetromino::RECIPE, OPUS),
    credited(forge::RECIPE, OPUS),
    credited(supernova::RECIPE, OPUS),
    credited(fireflies::RECIPE, OPUS),
    credited(neon::RECIPE, OPUS),
    credited(prism::RECIPE, OPUS),
    credited(turing::RECIPE, DEEPSEEK),
    credited(sandpile::RECIPE, DEEPSEEK),
    credited(quipu::RECIPE, DEEPSEEK),
    credited(relay::RECIPE, DEEPSEEK),
    credited(phyllotaxis::RECIPE, DEEPSEEK),
    credited(tectonic::RECIPE, DEEPSEEK),
    credited(lfsr::RECIPE, USER),
    credited(cambrian::RECIPE, USER),
];
