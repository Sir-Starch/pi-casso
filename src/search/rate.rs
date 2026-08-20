//! A rolling throughput meter.
//!
//! A cumulative average — total work divided by total time — is what the search
//! used to report as "speed". It is almost useless while a search is running:
//! it barely moves when throughput halves, and it never recovers visibly when
//! throughput returns. This measures the last few seconds instead.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

/// Long enough to smooth out chunk boundaries, short enough that a stall is
/// visible within a couple of refreshes.
const DEFAULT_WINDOW: Duration = Duration::from_secs(3);

#[derive(Debug)]
pub(crate) struct RateTracker {
    /// (observation time, cumulative total) pairs.
    samples: VecDeque<(Instant, u64)>,
    window: Duration,
}

impl RateTracker {
    pub fn new() -> Self {
        Self {
            samples: VecDeque::new(),
            window: DEFAULT_WINDOW,
        }
    }

    pub fn record(&mut self, now: Instant, total: u64) {
        self.samples.push_back((now, total));
        // Keep one sample older than the window so the span always covers it.
        while self.samples.len() > 2 {
            let second_oldest = self.samples[1].0;
            if now.duration_since(second_oldest) > self.window {
                self.samples.pop_front();
            } else {
                break;
            }
        }
    }

    /// Units per second over the retained window, or zero until there is enough
    /// history to say anything honest.
    pub fn rate(&self) -> f64 {
        let (Some((first_at, first_total)), Some((last_at, last_total))) =
            (self.samples.front(), self.samples.back())
        else {
            return 0.0;
        };
        let elapsed = last_at.duration_since(*first_at).as_secs_f64();
        if elapsed <= 0.0 {
            return 0.0;
        }
        // A counter that went backwards (a reset, or a shrinking file) reads as
        // idle rather than as a negative rate.
        let delta = last_total.saturating_sub(*first_total);
        delta as f64 / elapsed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_empty_tracker_reports_nothing() {
        assert_eq!(RateTracker::new().rate(), 0.0);
    }

    #[test]
    fn a_single_sample_reports_nothing() {
        let mut tracker = RateTracker::new();
        tracker.record(Instant::now(), 100);
        assert_eq!(tracker.rate(), 0.0);
    }

    #[test]
    fn a_steady_counter_reports_its_rate() {
        let mut tracker = RateTracker::new();
        let start = Instant::now();
        for step in 0..=4 {
            tracker.record(start + Duration::from_millis(500 * step), step * 1_000);
        }
        assert!((tracker.rate() - 2_000.0).abs() < 1.0, "{}", tracker.rate());
    }

    #[test]
    fn old_samples_leave_the_window() {
        let mut tracker = RateTracker::new();
        let start = Instant::now();
        // A fast burst, then a long slow stretch.
        tracker.record(start, 0);
        tracker.record(start + Duration::from_millis(100), 100_000);
        for step in 1..=8 {
            tracker.record(
                start + Duration::from_millis(100 + 500 * step),
                100_000 + step * 50,
            );
        }
        // The burst is outside the window now, so the rate reflects the slow part.
        assert!(tracker.rate() < 1_000.0, "{}", tracker.rate());
    }

    #[test]
    fn a_stalled_counter_decays_to_zero() {
        let mut tracker = RateTracker::new();
        let start = Instant::now();
        tracker.record(start, 5_000);
        for step in 1..=8 {
            tracker.record(start + Duration::from_millis(500 * step), 5_000);
        }
        assert_eq!(tracker.rate(), 0.0);
    }

    #[test]
    fn a_counter_that_goes_backwards_reads_as_idle() {
        let mut tracker = RateTracker::new();
        let start = Instant::now();
        tracker.record(start, 900);
        tracker.record(start + Duration::from_millis(500), 100);
        assert_eq!(tracker.rate(), 0.0);
    }
}
