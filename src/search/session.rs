//! The mutable state of one search invocation, gathered into a single value.
//!
//! Before this existed, every place that needed to publish a snapshot or wind a
//! search down had to thread the same ten-to-thirteen loose parameters through
//! by hand — `snapshot(...)` was called that way twelve times, `finish(...)` six.
//! Those call sites are now `session.snapshot()` and `session.finish(...)`, and
//! adding a field no longer means editing eighteen argument lists.

use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Result, bail};

use crate::benchmark_report::{MemoryReport, QueueReport, ReducerReport};
use crate::digits::DigitSource;
use crate::performance::{PerformanceSnapshot, RuntimeMetrics};
use crate::search::backend::{SearchBackend, create_search_backend};
use crate::search::rate::RateTracker;
use crate::search::types::{
    FinishReason, GenerationProgress, MatchMode, SearchOptions, SearchReporter, SearchSnapshot,
};
use crate::storage::{BestEventRecord, CheckpointProgress, RunRecord, RunStatus, Storage};

mod pipeline;
pub(crate) mod resource_budget;
mod source_reader;
mod telemetry;

pub(crate) use resource_budget::{ReaderCapacityLease, ResourceBudget};
pub(crate) use source_reader::DigitReaderPool;
pub(crate) use telemetry::SessionTelemetry;

pub(crate) use pipeline::run as run_pipeline;

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
    pub producer_epochs: u64,
    pub coalesced_request_count: u64,
    pub generation_batches: u64,
    pub reducer_contiguous_completed_offsets: u64,
    pub reducer_max_reorder_depth: u64,
}

pub(crate) struct SearchSession<'a> {
    pub run: RunRecord,
    pub options: SearchOptions,
    pub backend: Box<dyn SearchBackend>,
    pub(crate) budget: Arc<ResourceBudget>,
    pub runtime: RuntimeContext,
    pub telemetry: SessionTelemetry,
    pub reader_pool: Option<DigitReaderPool<'a>>,
    _reader_capacity: ReaderCapacityLease,
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
    pub fn new_with_budget(
        storage: &Storage,
        run: RunRecord,
        source: &'a dyn DigitSource,
        mut options: SearchOptions,
        shared_budget: Option<Arc<ResourceBudget>>,
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

        let budget = match shared_budget {
            Some(budget) => budget,
            None => ResourceBudget::new(
                options.performance.limits.queue_depth,
                options.performance.limits.memory_limit_mb,
                options.performance.limits.cpu_workers,
            )?,
        };
        budget.validate_minimum(window_len)?;
        let source_len = source.len()?;

        let recent_events = storage.history(&run.id, Some(8)).unwrap_or_default();
        let runtime_base = run.total_runtime_secs;
        let invocation_start_offset = run.current_offset;
        let invocation_start_scanned = run.scanned_windows;
        let telemetry_enabled = options.performance.show_metrics;
        let max_read_len = options
            .chunk_windows
            .checked_add(window_len - 1)
            .ok_or_else(|| anyhow::anyhow!("digit reader buffer size overflowed"))?;
        let reader_capacity = DigitReaderPool::configured_capacity_bytes(
            source,
            options.performance.limits.cpu_workers,
            options.performance.limits.queue_depth,
            max_read_len,
        )?;
        let reader_capacity = budget.reserve_reader_capacity(reader_capacity)?;
        let backend = create_search_backend(&options, &run.target_bitmap, budget.as_ref())?;
        let reader_pool = DigitReaderPool::new(
            source,
            options.performance.limits.cpu_workers,
            options.performance.limits.queue_depth,
            max_read_len,
        )?;
        let mut telemetry = SessionTelemetry::new(telemetry_enabled);
        telemetry.record_source_pool(reader_pool.telemetry());
        let runtime = RuntimeContext {
            last_chunk_processing: Duration::ZERO,
            checkpoint_count: 0,
            throttling_active: false,
            battery_throttle_active: false,
            window_len,
            active_backend: backend.name().to_string(),
            gpu_status: backend.gpu_status(),
            producer_epochs: 0,
            coalesced_request_count: 0,
            generation_batches: 0,
            reducer_contiguous_completed_offsets: 0,
            reducer_max_reorder_depth: 0,
        };

        let mut session = Self {
            run,
            options,
            backend,
            budget,
            runtime,
            telemetry,
            reader_pool: Some(reader_pool),
            _reader_capacity: reader_capacity,
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
        let backend =
            create_search_backend(&self.options, &self.run.target_bitmap, self.budget.as_ref())?;
        self.backend = backend;
        self.sync_backend_labels();
        Ok(())
    }

    pub fn checkpoint(&mut self, storage: &mut Storage) -> Result<()> {
        self.touch_runtime();
        let prior = self.run.checkpoint_state();
        let progress = CheckpointProgress {
            current_offset: self.run.current_offset,
            scanned_windows: self.run.scanned_windows,
            best_score: self.run.best_score,
            best_offset: self.run.best_offset,
            stop_reason: prior.progress.stop_reason,
            checkpoint_sequence: prior.progress.checkpoint_sequence,
        };
        let params = serde_json::from_str::<serde_json::Value>(&self.run.params_json)?;
        let mut performance_snapshot = match params.get("performance_snapshot") {
            Some(value) => PerformanceSnapshot::decode_value(value.clone())?,
            None => PerformanceSnapshot::from_settings(
                self.options.performance.clone(),
                Some(self.run.current_offset),
                self.options.work_windows,
                self.options.limit,
            ),
        };
        performance_snapshot.settings = self.options.performance.clone();
        performance_snapshot.current_offset = Some(self.run.current_offset);
        performance_snapshot.work_windows = self.options.work_windows;
        performance_snapshot.limit = self.options.limit;
        performance_snapshot.max_offset = self.options.max_offset;
        performance_snapshot.keep_going_after_perfect = self.options.keep_going_after_perfect;
        let started = Instant::now();
        let checkpoint = storage
            .checkpoint_with_snapshot(&self.run.id, &progress, &performance_snapshot)
            .map_err(|failure| failure.cause)?;
        self.run.current_offset = checkpoint.progress.current_offset;
        self.run.scanned_windows = checkpoint.progress.scanned_windows;
        self.run.best_score = checkpoint.progress.best_score;
        self.run.best_offset = checkpoint.progress.best_offset;
        self.run.params_json = checkpoint.params_json;
        self.telemetry.record_persistence(started.elapsed());
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
        let started = Instant::now();
        storage.update_run(&mut self.run)?;
        self.telemetry.record_persistence(started.elapsed());
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
        let source_len = if source_is_growing {
            self.source.len().unwrap_or(self.source_len)
        } else {
            self.source_len
        };
        let window_len = self.window_len as u64;
        // "Waiting" means running but starved: the generator has not produced
        // enough digits for the next window yet.
        let waiting_for_digits = source_is_growing
            && (source_len < window_len || self.run.current_offset + window_len > source_len);

        let mut metrics = RuntimeMetrics::from_settings(
            &self.options.performance,
            self.runtime.last_chunk_processing,
            self.runtime.checkpoint_count,
            source_len,
            self.run.current_offset,
            source_len.saturating_sub(self.run.current_offset),
            self.runtime.window_len,
            self.runtime.throttling_active,
            self.runtime.battery_throttle_active,
        );
        metrics.search_backend = self.runtime.active_backend.clone();
        metrics.gpu_status = self.runtime.gpu_status.clone();

        let resolved_backend = self
            .telemetry
            .resolved_backend(&self.runtime.active_backend);
        metrics.backend_device = self
            .runtime
            .gpu_status
            .strip_prefix("active: ")
            .map_or_else(
                || {
                    if resolved_backend == "cpu" {
                        "cpu".to_string()
                    } else {
                        "wgpu".to_string()
                    }
                },
                str::to_string,
            );
        metrics.backend_feature_available =
            matches!(resolved_backend.as_str(), "cpu" | "wgpu" | "cuda" | "mixed");
        metrics.backend_fault_status = if self.telemetry.fallback_count() > 0 {
            "runtime_fault".to_string()
        } else {
            "none".to_string()
        };
        metrics.fallback = self.telemetry.fallback_count() > 0;
        metrics.fallback_reason = self.telemetry.fallback_reason();
        metrics.fallback_count = self.telemetry.fallback_count();
        metrics.gpu_submissions = self.telemetry.gpu_submissions();
        metrics.gpu_completions = self.telemetry.gpu_completions();
        metrics.gpu_buffer_creations = self.telemetry.gpu_buffer_creations();
        metrics.gpu_bind_group_creations = self.telemetry.gpu_bind_group_creations();
        metrics.gpu_resource_reuses = self.telemetry.gpu_resource_reuses();
        let overlap = self.telemetry.gpu_overlap();
        metrics.gpu_overlap_ms = if overlap.is_zero() {
            0
        } else {
            u64::try_from(overlap.as_millis())
                .unwrap_or(u64::MAX)
                .max(1)
        };
        metrics.gpu_max_in_flight = self.telemetry.gpu_max_in_flight();
        metrics.gpu_overlap_events = self.telemetry.gpu_overlap_events();
        metrics.gpu_test_only_mock = self.telemetry.gpu_test_only_mock();
        let gpu_duty = self.telemetry.gpu_duty();
        metrics.gpu_duty_wait_ms = duration_ms(gpu_duty.wait);
        metrics.gpu_initial_submission_wait_ms = duration_ms(gpu_duty.initial_submission_wait);
        metrics.active_submission_ratio = gpu_duty.active_submission_ratio;
        metrics.dispatch_quantum_ratio = gpu_duty.dispatch_quantum_ratio;
        metrics.stage_timings = self.telemetry.stages();
        metrics.waits = self.telemetry.waits();
        metrics.source = self.telemetry.source_report();
        let budget = self.budget.snapshot();
        metrics.queue = QueueReport {
            current_occupancy: budget.queue_current,
            max_occupancy: budget.queue_peak,
            permits: budget.queue_limit,
            global_limit: budget.queue_limit,
        };
        metrics.memory = MemoryReport {
            logical_reserved_mb: bytes_to_mb(budget.memory_reserved_bytes),
            logical_peak_mb: bytes_to_mb(budget.memory_peak_bytes),
            logical_budget_mb: bytes_to_mb(budget.memory_limit_bytes),
            logical_reserved_bytes: budget.memory_reserved_bytes,
            logical_peak_bytes: budget.memory_peak_bytes,
            logical_budget_bytes: budget.memory_limit_bytes,
            rss_peak_mb: budget.rss_peak_mb,
            rss_baseline_mb: budget.rss_baseline_mb,
            rss_margin_mb: budget.rss_margin_mb,
            gpu_vram_status: "unavailable".to_string(),
            ..MemoryReport::default()
        };
        metrics.reducer = ReducerReport {
            ordered: true,
            contiguous_completed_offsets: self.runtime.reducer_contiguous_completed_offsets,
            max_reorder_depth: self.runtime.reducer_max_reorder_depth,
        };
        metrics.cpu_permits_in_use = budget.cpu_permits_in_use;
        metrics.cpu_permits_peak = budget.cpu_permits_peak;
        metrics.cpu_permits_max = budget.cpu_permits_max;
        metrics.resolved_backend = resolved_backend;
        let generation_rate = self
            .source
            .generation_metrics()
            .map(|generation| {
                let elapsed = generation.generator_wait.as_secs_f64();
                if elapsed > 0.0 {
                    generation.generated_source_digits as f64 / elapsed
                } else {
                    0.0
                }
            })
            .unwrap_or(0.0)
            .max(self.generation_rate.rate());
        metrics.generator_digits_per_second = generation_rate;
        metrics.telemetry_enabled = self.telemetry.enabled();

        let generation = self.source.generation().map(|state| GenerationProgress {
            active: state.active,
            target_digits: state.target_digits,
            digits_per_sec: generation_rate,
        });

        SearchSnapshot {
            run: self.run.clone(),
            speed_windows_per_sec: speed,
            average_windows_per_sec: average,
            session_elapsed: elapsed,
            progress: self.progress(scanned_this_invocation),
            recent_events: self.recent_events.clone(),
            source_kind: self.source.kind().to_string(),
            source_len,
            source_is_growing,
            waiting_for_digits,
            cache_gap_digits: source_len.saturating_sub(self.run.current_offset),
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

fn bytes_to_mb(bytes: u64) -> f64 {
    bytes as f64 / 1_048_576.0
}

fn duration_ms(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
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
