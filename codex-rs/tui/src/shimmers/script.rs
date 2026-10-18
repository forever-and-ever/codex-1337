//! A scripted turn, the design swatches' Cycle: the model works under three headers, then waits on a
//! background terminal whose output enters the context as the wait ends. The picker previews
//! shimmers with it, paced as the live feed paces a real thread.

use super::kit::hash;
use super::stage::DRAIN;
use super::stage::Spell;
use super::stage::pace;

/// Seconds per slot of the token schedule.
const SLOT: f64 = 0.05;

/// What the agent does during a segment.
#[derive(Clone, Copy)]
enum Work {
    /// The model works, its output entering the context at `rate` tokens a second.
    Busy { rate: f64 },
    /// A tool runs; its result's `tokens` enter the context as the wait ends.
    Wait { tokens: f64 },
}

struct Segment {
    header: &'static str,
    length: f64,
    work: Work,
}

static CYCLE: [Segment; 4] = [
    Segment {
        header: "Working",
        length: 2.5,
        work: Work::Busy { rate: 60.0 },
    },
    Segment {
        header: "Tracing shimmer cadence",
        length: 3.5,
        work: Work::Busy { rate: 80.0 },
    },
    Segment {
        header: "Drafting the fix",
        length: 4.0,
        work: Work::Busy { rate: 50.0 },
    },
    Segment {
        header: "Waiting for background terminal",
        length: 7.0,
        work: Work::Wait { tokens: 600.0 },
    },
];

fn length() -> f64 {
    CYCLE.iter().map(|segment| segment.length).sum()
}

/// The segment under way `t` seconds into a cycle.
fn segment(t: f64) -> &'static Segment {
    let mut end = 0.0;
    for segment in &CYCLE[..CYCLE.len() - 1] {
        end += segment.length;
        if t < end {
            return segment;
        }
    }
    &CYCLE[CYCLE.len() - 1]
}

/// Tokens a second entering the context `t` seconds into a cycle: the model's output while it
/// works, and every wait's result draining.
fn inflow(t: f64) -> f64 {
    let length = length();
    // A result drains on through later cycles, so each cycle meets every earlier one's remainder.
    let echoes = 1.0 / -(-length / DRAIN).exp_m1();
    let mut end = 0.0;
    let mut drained = 0.0;
    for segment in &CYCLE {
        end += segment.length;
        if let Work::Wait { tokens } = segment.work {
            drained += tokens / DRAIN * (-(t - end).rem_euclid(length) / DRAIN).exp() * echoes;
        }
    }
    match segment(t).work {
        Work::Busy { rate } => rate + drained,
        Work::Wait { .. } => drained,
    }
}

pub(super) struct Script {
    /// Paced tokens arrived before each slot of one cycle; the last entry is the cycle's total.
    before: Vec<u64>,
}

impl Script {
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        clippy::cast_precision_loss,
        clippy::cast_possible_wrap,
        reason = "The fixed 17-second preview has 340 nonnegative slots; per-slot visual token inflow is deliberately floored."
    )]
    pub(super) fn new() -> Self {
        let slots = (length() / SLOT).round() as usize;
        let mut before = vec![0; slots + 1];
        for slot in 0..slots {
            let paced = pace(inflow((slot as f64 + 0.5) * SLOT)) * SLOT;
            before[slot + 1] = before[slot] + (paced + hash(slot as i64)).floor() as u64;
        }
        Self { before }
    }

    /// The spell, the header and the paced tokens arrived `t` seconds into the looping script.
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "Preview time deliberately selects whole schedule slots, with Rust's saturating cast mapping the pre-first tick to slot zero."
    )]
    pub(super) fn at(&self, t: f64) -> (Spell, &'static str, u64) {
        let segment = segment(t % length());
        let slots = self.before.len() - 1;
        let slot = (t / SLOT + 1e-6).floor() as usize;
        let tokens = (slot / slots) as u64 * self.before[slots] + self.before[slot % slots];
        let spell = match segment.work {
            Work::Busy { .. } => Spell::Busy,
            Work::Wait { .. } => Spell::Wait,
        };
        (spell, segment.header, tokens)
    }
}
