//! Admit wheel distance, not animation: capped input is discarded rather than replayed later.

use tokio::time::Instant;

const ROWS_PER_SECOND: f64 = 150.0;
// Terminals deliver ticks in bursts. Retain at most 100 ms of unused capacity, not idle-time credit.
const BURST_ROWS: f64 = ROWS_PER_SECOND / 10.0;

pub(super) struct WheelScroll {
    remainder: f64,
    credit: f64,
    last_input: Instant,
}

impl Default for WheelScroll {
    fn default() -> Self {
        Self {
            remainder: 0.0,
            credit: BURST_ROWS,
            last_input: Instant::now(),
        }
    }
}

impl WheelScroll {
    #[expect(
        clippy::cast_possible_truncation,
        reason = "Admitted distances are integral and bounded to fifteen rows before conversion."
    )]
    pub(super) fn rows(&mut self, delta: f64) -> isize {
        let now = Instant::now();
        self.credit = (self.credit
            + now.duration_since(self.last_input).as_secs_f64() * ROWS_PER_SECOND)
            .min(BURST_ROWS);
        self.last_input = now;

        if self.remainder.signum() != delta.signum() {
            self.remainder = 0.0;
        }
        self.remainder += delta;
        // Decimal sensitivities can land just short of a whole row after repeated addition.
        let rounded = self.remainder.round();
        if rounded != 0.0 && (self.remainder - rounded).abs() < 1e-9 {
            self.remainder = rounded;
        }
        let rows = self.remainder.trunc().abs().min(self.credit.floor());
        self.remainder = self.remainder.fract();
        self.credit -= rows;
        (rows * delta.signum()) as isize
    }
}

#[cfg(test)]
mod tests {
    use super::WheelScroll;
    use std::time::Duration;

    #[tokio::test(start_paused = true)]
    async fn wheel_bursts_cap_distance_without_replaying_excess() {
        // Event batching must not turn a velocity cap into either a frame-rate cap or scroll debt.
        for batch_ms in [1, 10, 20, 50, 100] {
            let mut wheel = WheelScroll::default();
            assert_eq!(wheel.rows(10_000.0), 15);
            let mut distance = 0;
            for _ in 0..1_000 / batch_ms {
                tokio::time::advance(Duration::from_millis(batch_ms)).await;
                distance += wheel.rows(10_000.0);
            }
            assert!((149..=150).contains(&distance), "{batch_ms} ms: {distance}");

            tokio::time::advance(Duration::from_secs(1)).await;
            assert_eq!(wheel.rows(-1.0), -1);
            assert_eq!(wheel.rows(0.5), 0);
            assert_eq!(wheel.rows(-0.5), 0);
            assert_eq!(wheel.rows(-0.5), -1);
        }
    }
}
