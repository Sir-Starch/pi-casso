//! The mutable state of one search invocation, gathered into a single value.
//!
//! Before this existed, every place that needed to publish a snapshot or wind a
//! search down had to thread the same ten-to-thirteen loose parameters through
//! by hand — `snapshot(...)` was called that way twelve times, `finish(...)` six.
//! Those call sites are now `session.snapshot()` and `session.finish(...)`, and
//! adding a field no longer means editing eighteen argument lists.

use std::time::{Duration, Instant};

use anyhow::{Result, bail};

use crate::digits::DigitSource;
use crate::performance::RuntimeMetrics;
use crate::search::backend::{SearchBackend, create_search_backend};
use crate::search::rate::RateTracker;
use crate::search::types::{
    FinishReason, GenerationProgress, MatchMode, SearchOptions, SearchReporter, SearchSnapshot,
};
use crate::storage::{BestEventRecord, RunRecord, RunStatus, Storage};

/// Live engine facts that are reported but not configured.
#[derive(Clone, Debug)]
pub(crate) struct RuntimeContext {
    pub last_chunk_processing: Duration,
    pub checkpoint_count: u64,
    pub throttling_active: bool,
    pub battery_throttle_active: bool,
    pub window_len: usize,
    pub active_backend: String,
    pub gpu_status: String,
}

pub(crate) struct SearchSession<'a> {
    pub run: RunRecord,
    pub options: SearchOptions,
    pub backend: Box<dyn SearchBackend>,
    pub runtime: RuntimeContext,
    pub recent_events: Vec<BestEventRecord>,
    pub source: &'a dyn DigitSource,
    pub source_len: u64,
    pub canvas_width: usize,
    pub canvas_height: usize,
    pub window_len: usize,
    pub paused: bool,
    /// Rolling meters, so "speed" means the last few seconds rather than the
    /// session average. They are updated wherever the underlying counter moves.
    scan_rate: RateTracker,
    generation_rate: RateTracker,
    session_start: Instant,
    last_checkpoint: Instant,
    /// Where this invocation began, so progress and speed describe *this* run of
    /// the search rather than the whole lifetime of a resumed hunt.
    invocation_start_offset: u64,
    invocation_start_scanned: u64,
    runtime_base: f64,
}

impl<'a> SearchSession<'a> {
    pub fn new(
        storage: &Storage,
        run: RunRecord,
        source: &'a dyn DigitSource,
        mut options: SearchOptions,
    ) -> Result<Self> {
        if options.threshold > 9 {
            bail!("threshold must be between 0 and 9");
        }
        if options.chunk_windows == 0 {
            bail!("chunk_windows must be non-zero");
        }
        if let Some(workers) = options.workers {
            options.performance.limits.cpu_workers = workers.max(1);
        }
        // The profile is the single source of truth for chunking and checkpoint
        // cadence; the loose fields on SearchOptions mirror it.
        options.chunk_windows = options.performance.limits.chunk_size;
        options.checkpoint_every =
            Duration::from_secs(options.performance.limits.checkpoint_every_secs);

        let source_len = source.len()?;
        let (canvas_width, canvas_height) = canvas_size(&run, &options);
        if options.match_mode.is_emergence()
            && (canvas_width < run.target_bitmap.width || canvas_height < run.target_bitmap.height)
        {
            bail!(
                "canvas size {}x{} must be at least target size {}x{}",
                canvas_width,
                canvas_height,
                run.target_bitmap.width,
                run.target_bitmap.height
            );
        }
        let window_len = if options.match_mode.is_emergence() {
            canvas_width * canvas_height
        } else {
            run.target_bitmap.pixels.len()
        };
        if window_len == 0 {
            bail!("target bitmap is empty");
        }

        let backend = create_search_backend(&options, &run.target_bitmap)?;
        let recent_events = storage.history(&run.id, Some(8)).unwrap_or_default();
        let runtime = RuntimeContext {
            last_chunk_processing: Duration::ZERO,
            checkpoint_count: 0,
            throttling_active: false,
            battery_throttle_active: false,
            window_len,
            active_backend: backend.name().to_string(),
            gpu_status: backend.gpu_status(),
        };
        let runtime_base = run.total_runtime_secs;
        let invocation_start_offset = run.current_offset;
        let invocation_start_scanned = run.scanned_windows;

        let mut session = Self {
            run,
            options,
            backend,
            runtime,
            recent_events,
            source,
            source_len,
            canvas_width,
            canvas_height,
            window_len,
            paused: false,
            scan_rate: RateTracker::new(),
            generation_rate: RateTracker::new(),
            session_start: Instant::now(),
            last_checkpoint: Instant::now(),
            invocation_start_offset,
            invocation_start_scanned,
            runtime_base,
        };
        if source.is_growing() {
            session.run.generated_digit_count = source_len;
        }
        session.run.total_runtime_secs = runtime_base;
        Ok(session)
    }

    /// Total runtime is the base carried over from previous invocations plus the
    /// wall clock of this one. Called before every write so a checkpoint never
    /// records a stale runtime.
    pub fn touch_runtime(&mut self) {
        self.run.total_runtime_secs =
            self.runtime_base + self.session_start.elapsed().as_secs_f64();
    }

    pub fn refresh_source_len(&mut self) -> Result<()> {
        if self.source.is_growing() {
            self.source_len = self.source.len()?;
            self.run.generated_digit_count = self.source_len;
            self.generation_rate.record(Instant::now(), self.source_len);
        }
        Ok(())
    }

    /// Called wherever scan progress advances, which is what the rolling meter
    /// samples. Cheap enough to call on every chunk.
    pub fn record_progress(&mut self) {
        self.scan_rate
            .record(Instant::now(), self.run.scanned_windows);
    }

    pub fn sync_backend_labels(&mut self) {
        self.runtime.active_backend = self.backend.name().to_string();
        self.runtime.gpu_status = self.backend.gpu_status();
    }

    /// Rebuilds the backend after a change that could flip the CPU/GPU decision.
    pub fn rebuild_backend(&mut self) -> Result<()> {
        let backend = create_search_backend(&self.options, &self.run.target_bitmap)?;
        self.backend = backend;
        self.sync_backend_labels();
        Ok(())
    }

    pub fn checkpoint(&mut self, storage: &mut Storage) -> Result<()> {
        self.touch_runtime();
        storage.update_run(&mut self.run)?;
        self.runtime.checkpoint_count += 1;
        self.last_checkpoint = Instant::now();
        Ok(())
    }

    pub fn checkpoint_if_due(&mut self, storage: &mut Storage) -> Result<bool> {
        if self.last_checkpoint.elapsed() < self.options.checkpoint_every {
            return Ok(false);
        }
        self.checkpoint(storage)?;
        Ok(true)
    }

    pub fn report<R: SearchReporter>(&self, reporter: &mut R) -> Result<()> {
        reporter.on_update(&self.snapshot())
    }

    pub fn push_event(&mut self, event: BestEventRecord) {
        self.recent_events.push(event);
        if self.recent_events.len() > 8 {
            self.recent_events.remove(0);
        }
    }

    /// Writes the terminal state and tells the reporter why the search stopped.
    pub fn finish<R: SearchReporter>(
        &mut self,
        storage: &mut Storage,
        reporter: &mut R,
        reason: FinishReason,
    ) -> Result<()> {
        self.run.status = match reason {
            FinishReason::PerfectFound => RunStatus::PerfectFound,
            FinishReason::SourceExhausted => RunStatus::SourceExhausted,
            // A stop, a limit, or a ceiling all leave a resumable run.
            FinishReason::Interrupted
            | FinishReason::LimitReached
            | FinishReason::MaxOffsetReached => RunStatus::Paused,
        };
        self.touch_runtime();
        storage.update_run(&mut self.run)?;
        reporter.on_finish(&self.snapshot(), reason)
    }

    pub fn snapshot(&self) -> SearchSnapshot {
        let elapsed = self.session_start.elapsed();
        let scanned_this_invocation = self
            .run
            .scanned_windows
            .saturating_sub(self.invocation_start_scanned);
        let average = if elapsed.as_secs_f64() > 0.0 {
            scanned_this_invocation as f64 / elapsed.as_secs_f64()
        } else {
            0.0
        };
        // Before the rolling meter has two samples it reports zero, which would
        // read as "stalled" on the first frame; fall back to the average there.
        let rolling = self.scan_rate.rate();
        let speed = if rolling > 0.0 { rolling } else { average };
        let source_is_growing = self.source.is_growing();
        let window_len = self.window_len as u64;
        // "Waiting" means running but starved: the generator has not produced
        // enough digits for the next window yet.
        let waiting_for_digits = source_is_growing
            && (self.source_len < window_len
                || self.run.current_offset + window_len > self.source_len);

        let mut metrics = RuntimeMetrics::from_settings(
            &self.options.performance,
            self.runtime.last_chunk_processing,
            self.runtime.checkpoint_count,
            self.source_len,
            self.run.current_offset,
            self.source_len.saturating_sub(self.run.current_offset),
            self.runtime.window_len,
            self.runtime.throttling_active,
            self.runtime.battery_throttle_active,
        );
        metrics.search_backend = self.runtime.active_backend.clone();
        metrics.gpu_status = self.runtime.gpu_status.clone();

        let generation = self.source.generation().map(|state| GenerationProgress {
            active: state.active,
            target_digits: state.target_digits,
            digits_per_sec: self.generation_rate.rate(),
        });

        SearchSnapshot {
            run: self.run.clone(),
            speed_windows_per_sec: speed,
            average_windows_per_sec: average,
            session_elapsed: elapsed,
            progress: self.progress(scanned_this_invocation),
            recent_events: self.recent_events.clone(),
            source_kind: self.source.kind().to_string(),
            source_len: self.source_len,
            source_is_growing,
            waiting_for_digits,
            cache_gap_digits: self.source_len.saturating_sub(self.run.current_offset),
            generation,
            metrics,
        }
    }

    /// Only a bounded search has meaningful progress. An endless hunt reports
    /// `None` rather than a fake percentage.
    fn progress(&self, scanned_this_invocation: u64) -> Option<f64> {
        if let Some(limit) = self.options.limit {
            if limit == 0 {
                return Some(1.0);
            }
            return Some((scanned_this_invocation as f64 / limit as f64).min(1.0));
        }
        let max_offset = self.options.max_offset?;
        let span = max_offset.saturating_sub(self.invocation_start_offset);
        if span == 0 {
            return Some(1.0);
        }
        Some(
            (self
                .run
                .current_offset
                .saturating_sub(self.invocation_start_offset) as f64
                / span as f64)
                .min(1.0),
        )
    }

    pub fn scanned_this_invocation(&self) -> u64 {
        self.run
            .scanned_windows
            .saturating_sub(self.invocation_start_scanned)
    }
}

/// Emergence searches read a canvas larger than the target so the shape has room
/// to appear anywhere inside it; the other modes read exactly the target.
pub(crate) fn canvas_size(run: &RunRecord, options: &SearchOptions) -> (usize, usize) {
    if options.match_mode == MatchMode::Emergence {
        (options.canvas_width, options.canvas_height)
    } else {
        (run.width as usize, run.height as usize)
    }
}
