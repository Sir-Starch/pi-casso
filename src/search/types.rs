//! Data passed across the search boundary: what a caller asks for, what the
//! engine reports back, and what a front-end can command mid-flight.

use std::time::Duration;

use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};

use crate::art::Bitmap;
use crate::performance::{PerformanceProfile, PerformanceSettings, RuntimeMetrics};
use crate::storage::{BestEventRecord, RunRecord};

#[derive(Clone, Debug)]
pub struct SearchOptions {
    pub max_offset: Option<u64>,
    pub limit: Option<u64>,
    pub match_mode: MatchMode,
    pub canvas_width: usize,
    pub canvas_height: usize,
    pub threshold: u8,
    pub invert: bool,
    pub workers: Option<usize>,
    pub checkpoint_every: Duration,
    pub top_n: usize,
    pub keep_going_after_perfect: bool,
    pub chunk_windows: usize,
    pub performance: PerformanceSettings,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TopMatch {
    pub offset: u64,
    pub score: f64,
    pub bitmap: Bitmap,
    pub inverted: bool,
    pub scanned_windows: u64,
    #[serde(default)]
    pub details: Option<BestMatchDetails>,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MatchMode {
    Emergence,
    Threshold,
    Exact,
}

impl MatchMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Emergence => "emergence",
            Self::Threshold => "threshold",
            Self::Exact => "exact",
        }
    }

    pub fn from_str(value: &str) -> Result<Self> {
        match value {
            "emergence" => Ok(Self::Emergence),
            "threshold" => Ok(Self::Threshold),
            "exact" => Ok(Self::Exact),
            other => bail!("unknown match mode {other:?}"),
        }
    }

    pub(crate) fn is_emergence(self) -> bool {
        self == Self::Emergence
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BestMatchDetails {
    pub mode: MatchMode,
    pub digit: Option<u8>,
    pub x: Option<u32>,
    pub y: Option<u32>,
    pub canvas_width: u32,
    pub canvas_height: u32,
    pub raw_canvas_digits: Option<String>,
    pub coverage: Option<f64>,
    pub leakage: Option<f64>,
}

/// How the digit producer is doing, for growing sources only.
#[derive(Clone, Copy, Debug, Default)]
pub struct GenerationProgress {
    /// True while digits are actively being computed.
    pub active: bool,
    /// Total digits the producer has been asked to reach.
    pub target_digits: u64,
    /// Measured over a short window, so it reflects the last few seconds
    /// rather than the whole session.
    pub digits_per_sec: f64,
}

#[derive(Clone, Debug)]
pub struct SearchSnapshot {
    pub run: RunRecord,
    /// Throughput over the last few seconds. This is what a user means by
    /// "how fast is it going"; a session-long average hides every slowdown.
    pub speed_windows_per_sec: f64,
    /// Throughput across the whole invocation.
    pub average_windows_per_sec: f64,
    pub session_elapsed: Duration,
    pub progress: Option<f64>,
    pub recent_events: Vec<BestEventRecord>,
    pub source_kind: String,
    pub source_len: u64,
    pub source_is_growing: bool,
    pub waiting_for_digits: bool,
    pub cache_gap_digits: u64,
    /// `None` for sources that cannot grow.
    pub generation: Option<GenerationProgress>,
    pub metrics: RuntimeMetrics,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FinishReason {
    PerfectFound,
    SourceExhausted,
    Interrupted,
    LimitReached,
    MaxOffsetReached,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SearchCommand {
    Pause,
    Resume,
    SaveCheckpoint,
    Stop,
    SetProfile(PerformanceProfile),
    AdjustWorkers(i32),
    AdjustChunkSize(i32),
    CycleThermalMode,
    ToggleMetrics,
    ToggleGpuMode,
}

pub trait SearchReporter {
    fn on_update(&mut self, snapshot: &SearchSnapshot) -> Result<()>;
    fn on_new_best(&mut self, snapshot: &SearchSnapshot, event: &BestEventRecord) -> Result<()>;
    fn on_finish(&mut self, snapshot: &SearchSnapshot, reason: FinishReason) -> Result<()>;
}

#[derive(Clone, Debug)]
pub(crate) struct WindowScore {
    pub(crate) index: usize,
    pub(crate) score: f64,
    pub(crate) inverted: bool,
    pub(crate) digit: Option<u8>,
    pub(crate) x: Option<usize>,
    pub(crate) y: Option<usize>,
    pub(crate) coverage: Option<f64>,
    pub(crate) leakage: Option<f64>,
}

impl WindowScore {
    /// A window that scored nothing, used as the starting point of every
    /// best-placement search.
    pub(crate) fn empty(index: usize) -> Self {
        Self {
            index,
            score: 0.0,
            inverted: false,
            digit: Some(0),
            x: Some(0),
            y: Some(0),
            coverage: Some(0.0),
            leakage: Some(0.0),
        }
    }
}
