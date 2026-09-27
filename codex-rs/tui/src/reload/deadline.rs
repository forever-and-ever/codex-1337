//! A reload gets one budget, including rollback. Worker progress cannot renew it.

use std::time::Duration;
use std::time::Instant;

const CANDIDATE: Duration = Duration::from_secs(/*secs*/ 3);
const TOTAL: Duration = Duration::from_secs(/*secs*/ 5);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Generation {
    Candidate,
    Previous,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Transition {
    Wait(Duration),
    Rollback,
    Failed,
}

pub(super) struct Deadline {
    started: Instant,
    generation: Generation,
}

impl Deadline {
    pub(super) fn new(now: Instant) -> Self {
        Self {
            started: now,
            generation: Generation::Candidate,
        }
    }

    pub(super) fn poll(&mut self, now: Instant) -> Transition {
        let elapsed = now.saturating_duration_since(self.started);
        if elapsed >= TOTAL {
            return Transition::Failed;
        }
        if self.generation == Generation::Candidate && elapsed >= CANDIDATE {
            self.generation = Generation::Previous;
            return Transition::Rollback;
        }
        let limit = match self.generation {
            Generation::Candidate => CANDIDATE,
            Generation::Previous => TOTAL,
        };
        Transition::Wait(limit.saturating_sub(elapsed))
    }

    pub(super) fn reject(&mut self, now: Instant) -> Transition {
        if now.saturating_duration_since(self.started) >= TOTAL
            || self.generation == Generation::Previous
        {
            Transition::Failed
        } else {
            self.generation = Generation::Previous;
            Transition::Rollback
        }
    }
}

#[cfg(test)]
#[path = "deadline_tests.rs"]
mod tests;
