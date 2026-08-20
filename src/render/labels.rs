//! Human-readable labels for search state, shared by TUI and CLI output so the
//! two can no longer drift apart.

use crate::search::{FinishReason, GenerationProgress, SearchSnapshot};
use crate::storage::RunStatus;

/// What the search is actually doing, as opposed to what its stored status says.
///
/// The distinction that matters: a search on a generated cache spends much of
/// its time ahead of the generator. Calling that "waiting for more pi" reads as
/// a stall, when in fact digits are being computed as fast as the machine can.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PipelineState {
    Searching,
    /// Out of digits, and the generator is computing more.
    GeneratingPi,
    /// Out of digits with no generator making progress — the genuinely stuck case.
    Starved,
    Paused,
    PerfectFound,
    SourceExhausted,
}

impl PipelineState {
    pub fn label(self) -> &'static str {
        match self {
            Self::Searching => "searching",
            Self::GeneratingPi => "generating pi",
            Self::Starved => "waiting for digits",
            Self::Paused => "paused",
            Self::PerfectFound => "perfect found",
            Self::SourceExhausted => "source exhausted",
        }
    }

    /// True where the state means "work is happening", which is what decides
    /// whether the status reads as healthy or as a problem.
    pub fn is_busy(self) -> bool {
        matches!(self, Self::Searching | Self::GeneratingPi)
    }

    pub fn is_problem(self) -> bool {
        matches!(self, Self::Starved | Self::SourceExhausted)
    }
}

pub fn pipeline_state(
    status: RunStatus,
    waiting_for_digits: bool,
    generation: Option<GenerationProgress>,
) -> PipelineState {
    match status {
        RunStatus::Running if waiting_for_digits => match generation {
            // Actively computing, or has produced digits within the sampling
            // window — either way the pipeline is moving.
            Some(progress) if progress.active || progress.digits_per_sec > 0.0 => {
                PipelineState::GeneratingPi
            }
            _ => PipelineState::Starved,
        },
        RunStatus::Running => PipelineState::Searching,
        RunStatus::Paused => PipelineState::Paused,
        RunStatus::PerfectFound => PipelineState::PerfectFound,
        RunStatus::SourceExhausted => PipelineState::SourceExhausted,
    }
}

pub fn snapshot_state(snapshot: &SearchSnapshot) -> PipelineState {
    pipeline_state(
        snapshot.run.status,
        snapshot.waiting_for_digits,
        snapshot.generation,
    )
}

/// Short form for tables and status bars.
pub fn finish_reason_label(reason: FinishReason) -> &'static str {
    match reason {
        FinishReason::PerfectFound => "perfect match found",
        FinishReason::SourceExhausted => "source exhausted",
        FinishReason::Interrupted => "interrupted",
        FinishReason::LimitReached => "limit reached",
        FinishReason::MaxOffsetReached => "max offset reached",
    }
}

/// Long form, phrased as something to tell the user about what happens next.
pub fn finish_message(reason: FinishReason) -> &'static str {
    match reason {
        FinishReason::PerfectFound => "perfect match found; search stopped",
        FinishReason::SourceExhausted => {
            "digit source exhausted; use infinite pi or a larger pi file"
        }
        FinishReason::Interrupted => "search stopped; checkpoint saved",
        FinishReason::LimitReached => "search limit reached; checkpoint saved",
        FinishReason::MaxOffsetReached => "max offset reached; checkpoint saved",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn generation(active: bool, digits_per_sec: f64) -> Option<GenerationProgress> {
        Some(GenerationProgress {
            active,
            target_digits: 1_000_000,
            digits_per_sec,
        })
    }

    #[test]
    fn a_running_generator_means_generating_not_waiting() {
        assert_eq!(
            pipeline_state(RunStatus::Running, true, generation(true, 0.0)),
            PipelineState::GeneratingPi
        );
        // Between bursts the flag drops but digits are still arriving.
        assert_eq!(
            pipeline_state(RunStatus::Running, true, generation(false, 4_200.0)),
            PipelineState::GeneratingPi
        );
    }

    #[test]
    fn a_stopped_generator_is_the_only_genuine_starvation() {
        assert_eq!(
            pipeline_state(RunStatus::Running, true, generation(false, 0.0)),
            PipelineState::Starved
        );
        // A finite file has no generator at all.
        assert_eq!(
            pipeline_state(RunStatus::Running, true, None),
            PipelineState::Starved
        );
    }

    #[test]
    fn generating_counts_as_busy_not_as_a_problem() {
        assert!(PipelineState::GeneratingPi.is_busy());
        assert!(!PipelineState::GeneratingPi.is_problem());
        assert!(PipelineState::Starved.is_problem());
        assert!(!PipelineState::Starved.is_busy());
    }

    #[test]
    fn a_healthy_search_reads_as_searching() {
        assert_eq!(
            pipeline_state(RunStatus::Running, false, generation(true, 9_000.0)),
            PipelineState::Searching
        );
        assert_eq!(
            pipeline_state(RunStatus::Paused, true, generation(true, 9_000.0)),
            PipelineState::Paused
        );
        assert_eq!(
            pipeline_state(RunStatus::SourceExhausted, false, None),
            PipelineState::SourceExhausted
        );
    }
}
