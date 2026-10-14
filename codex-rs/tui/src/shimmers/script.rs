//! A scripted turn, the design swatches' Cycle: output under three headers with a stall, then a wait on
//! a background terminal. The picker previews shimmers with it and the glare tests measure them on it.

use super::kit::hash;
use super::stage::Spell;

/// Seconds per slot of the token schedule.
const SLOT: f64 = 0.05;

struct Segment {
    spell: Spell,
    header: &'static str,
    length: f64,
    /// Seconds into the segment during which no tokens arrive.
    stall: Option<(f64, f64)>,
}

static CYCLE: [Segment; 4] = [
    Segment {
        spell: Spell::Busy,
        header: "Working",
        length: 2.5,
        stall: None,
    },
    Segment {
        spell: Spell::Busy,
        header: "Tracing shimmer cadence",
        length: 3.5,
        stall: None,
    },
    Segment {
        spell: Spell::Busy,
        header: "Drafting the fix",
        length: 4.0,
        stall: Some((1.0, 3.8)),
    },
    Segment {
        spell: Spell::Wait,
        header: "Waiting for background terminal",
        length: 7.0,
        stall: None,
    },
];

fn length() -> f64 {
    CYCLE.iter().map(|segment| segment.length).sum()
}

/// The segment under way `t` seconds into a cycle, and the seconds into it.
fn segment(t: f64) -> (&'static Segment, f64) {
    let mut start = 0.0;
    for segment in &CYCLE[..CYCLE.len() - 1] {
        if t < start + segment.length {
            return (segment, t - start);
        }
        start += segment.length;
    }
    (&CYCLE[CYCLE.len() - 1], t - start)
}

pub(super) struct Script {
    /// Tokens arrived before each slot of one cycle; the last entry is the cycle's total.
    before: Vec<u64>,
}

impl Script {
    pub(super) fn new() -> Self {
        let slots = (length() / SLOT).round() as usize;
        let mut before = vec![0; slots + 1];
        for slot in 0..slots {
            let k = slot as f64;
            let (segment, into) = segment((k + 0.5) * SLOT);
            let stalled = segment
                .stall
                .is_some_and(|(from, to)| (from..to).contains(&into));
            let flowing =
                segment.spell == Spell::Busy && !stalled && hash(7 * slot as i64 + 3) > 0.08;
            let rate = 62.0 + 28.0 * (0.21 * k).sin() + 18.0 * (0.047 * k + 1.0).sin();
            let arrived = if flowing {
                (rate * SLOT + hash(slot as i64)).floor() as u64
            } else {
                0
            };
            before[slot + 1] = before[slot] + arrived;
        }
        Self { before }
    }

    /// The spell, the header and the tokens arrived `t` seconds into the looping script.
    pub(super) fn at(&self, t: f64) -> (Spell, &'static str, u64) {
        let (segment, _) = segment(t % length());
        let slots = self.before.len() - 1;
        let slot = (t / SLOT + 1e-6).floor() as usize;
        let tokens = (slot / slots) as u64 * self.before[slots] + self.before[slot % slots];
        (segment.spell, segment.header, tokens)
    }
}
