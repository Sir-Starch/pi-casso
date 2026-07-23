use std::cmp::Ordering;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering as AtomicOrdering},
    mpsc,
};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Result, bail};
use chrono::Utc;
use rayon::prelude::*;
use serde::{Deserialize, Serialize};

use crate::art::Bitmap;
use crate::digits::DigitSource;
use crate::gpu::{GpuSearchEngine, GpuWindowScore};
use crate::performance::{
    GpuMode, PerformanceOverrides, PerformanceProfile, PerformanceSettings, RuntimeMetrics,
    SearchBackendChoice, measure_elapsed, on_battery_power,
};
use crate::storage::{BestEventRecord, RunRecord, RunStatus, Storage};

const GROWING_SOURCE_WAIT: Duration = Duration::from_millis(50);

const EMERGENCE_COVERAGE_WEIGHT: f64 = 0.70;
const EMERGENCE_CONTRAST_WEIGHT: f64 = 0.20;
const EMERGENCE_CLEANLINESS_WEIGHT: f64 = 0.10;

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

    fn is_emergence(self) -> bool {
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

#[derive(Clone, Debug)]
pub struct SearchSnapshot {
    pub run: RunRecord,
    pub speed_windows_per_sec: f64,
    pub speed_digits_per_sec: f64,
    pub session_elapsed: Duration,
    pub progress: Option<f64>,
    pub recent_events: Vec<BestEventRecord>,
    pub source_kind: String,
    pub source_len: u64,
    pub source_is_growing: bool,
    pub waiting_for_digits: bool,
    pub cache_gap_digits: u64,
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
struct WindowScore {
    index: usize,
    score: f64,
    inverted: bool,
    digit: Option<u8>,
    x: Option<usize>,
    y: Option<usize>,
    coverage: Option<f64>,
    leakage: Option<f64>,
}

struct SearchChunkParams<'a> {
    target: &'a Bitmap,
    mode: MatchMode,
    canvas_width: usize,
    canvas_height: usize,
    threshold: u8,
    invert: bool,
    window_len: usize,
    emergence_plan: Option<&'a EmergencePlan>,
}

trait SearchBackend {
    fn name(&self) -> &'static str;
    fn gpu_status(&self) -> String;
    fn search_chunk(
        &mut self,
        digits: &[u8],
        actual_windows: usize,
        params: &SearchChunkParams<'_>,
    ) -> Result<Vec<WindowScore>>;
    fn reconfigure(&mut self, workers: usize) -> Result<()>;
}

struct CpuSearchBackend {
    pool: rayon::ThreadPool,
}

#[derive(Clone, Debug)]
struct RuntimeContext {
    last_chunk_processing: Duration,
    checkpoint_count: u64,
    throttling_active: bool,
    battery_throttle_active: bool,
    window_len: usize,
    active_backend: String,
    gpu_status: String,
}

#[derive(Clone, Debug)]
struct EmergencePlan {
    shape_pixels: usize,
    background_pixels: usize,
    placements: Vec<EmergencePlacement>,
}

#[derive(Clone, Debug)]
struct EmergencePlacement {
    x: usize,
    y: usize,
    shape_offsets: Vec<usize>,
    background_offsets: Vec<usize>,
}

fn emergence_score(coverage: f64, leakage: f64) -> f64 {
    let coverage = coverage.clamp(0.0, 1.0);
    let leakage = leakage.clamp(0.0, 1.0);
    if coverage == 1.0 && leakage == 0.0 {
        return 1.0;
    }
    let coverage_density = coverage * coverage;
    let contrast = if coverage > leakage {
        (coverage - leakage) / (1.0 - leakage).max(f64::EPSILON)
    } else {
        0.0
    };
    let cleanliness = 1.0 - leakage;
    EMERGENCE_COVERAGE_WEIGHT * coverage_density
        + EMERGENCE_CONTRAST_WEIGHT * contrast
        + EMERGENCE_CLEANLINESS_WEIGHT * cleanliness
}

impl CpuSearchBackend {
    fn new(workers: usize) -> Result<Self> {
        let workers = workers.max(1);
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(workers)
            .build()?;
        Ok(Self { pool })
    }
}

impl SearchBackend for CpuSearchBackend {
    fn name(&self) -> &'static str {
        "cpu"
    }

    fn gpu_status(&self) -> String {
        "disabled".to_string()
    }

    fn search_chunk(
        &mut self,
        digits: &[u8],
        actual_windows: usize,
        params: &SearchChunkParams<'_>,
    ) -> Result<Vec<WindowScore>> {
        Ok(self.pool.install(|| {
            (0..actual_windows)
                .into_par_iter()
                .map(|index| {
                    score_candidate_window(
                        index,
                        &digits[index..index + params.window_len],
                        params.target,
                        params.mode,
                        params.canvas_width,
                        params.canvas_height,
                        params.threshold,
                        params.invert,
                        params.emergence_plan,
                    )
                })
                .collect()
        }))
    }

    fn reconfigure(&mut self, workers: usize) -> Result<()> {
        *self = Self::new(workers)?;
        Ok(())
    }
}

struct HybridGpuSearchBackend {
    gpu: GpuSearchEngine,
    cpu: CpuSearchBackend,
}

impl HybridGpuSearchBackend {
    fn new(workers: usize, device_filter: Option<&str>) -> Result<Self> {
        Ok(Self {
            gpu: GpuSearchEngine::new(device_filter)?,
            cpu: CpuSearchBackend::new(workers)?,
        })
    }
}

impl SearchBackend for HybridGpuSearchBackend {
    fn name(&self) -> &'static str {
        "gpu"
    }

    fn gpu_status(&self) -> String {
        format!("active: {}", self.gpu.device_name())
    }

    fn search_chunk(
        &mut self,
        digits: &[u8],
        actual_windows: usize,
        params: &SearchChunkParams<'_>,
    ) -> Result<Vec<WindowScore>> {
        if params.mode != MatchMode::Emergence {
            return self.cpu.search_chunk(digits, actual_windows, params);
        }

        match self.gpu.emergence_scores(
            digits,
            actual_windows,
            params.target,
            params.canvas_width,
            params.canvas_height,
        ) {
            Ok(scores) => Ok(scores
                .into_iter()
                .enumerate()
                .map(gpu_score_to_window_score)
                .collect()),
            Err(err) => {
                eprintln!(
                    "warning: GPU search failed; falling back to CPU for this chunk: {err:#}"
                );
                self.cpu.search_chunk(digits, actual_windows, params)
            }
        }
    }

    fn reconfigure(&mut self, workers: usize) -> Result<()> {
        self.cpu.reconfigure(workers)
    }
}

fn create_search_backend(
    options: &SearchOptions,
    target: &Bitmap,
) -> Result<Box<dyn SearchBackend>> {
    let explicit_gpu = options.performance.backend == SearchBackendChoice::Gpu
        || options.performance.gpu == GpuMode::On;
    let wants_gpu = explicit_gpu
        || (options.performance.gpu == GpuMode::Auto && should_auto_use_gpu(options, target));
    if wants_gpu && options.match_mode == MatchMode::Emergence {
        match HybridGpuSearchBackend::new(
            options.performance.limits.cpu_workers,
            options.performance.gpu_device.as_deref(),
        ) {
            Ok(backend) => return Ok(Box::new(backend)),
            Err(err) => {
                if explicit_gpu {
                    eprintln!("warning: GPU backend requested but unavailable; using CPU: {err:#}");
                }
            }
        }
    }
    Ok(Box::new(CpuSearchBackend::new(
        options.performance.limits.cpu_workers,
    )?))
}

fn should_auto_use_gpu(options: &SearchOptions, target: &Bitmap) -> bool {
    if options.match_mode != MatchMode::Emergence {
        return false;
    }
    if options.performance.stress_test || options.performance.profile == PerformanceProfile::Max {
        return true;
    }
    let canvas_pixels = options.canvas_width.saturating_mul(options.canvas_height);
    let target_pixels = target.width.saturating_mul(target.height);
    let placement_width = options
        .canvas_width
        .saturating_sub(target.width)
        .saturating_add(1);
    let placement_height = options
        .canvas_height
        .saturating_sub(target.height)
        .saturating_add(1);
    let estimated_work = options
        .performance
        .limits
        .chunk_size
        .saturating_mul(placement_width.saturating_mul(placement_height))
        .saturating_mul(target_pixels);
    canvas_pixels >= 4_096
        && options.performance.limits.chunk_size >= 50_000
        && estimated_work >= 5_000_000_000
}

fn gpu_score_to_window_score((index, score): (usize, GpuWindowScore)) -> WindowScore {
    WindowScore {
        index,
        score: score.score,
        inverted: false,
        digit: Some(score.digit),
        x: Some(score.x),
        y: Some(score.y),
        coverage: Some(score.coverage),
        leakage: Some(score.leakage),
    }
}

fn growing_prefetch_extra(options: &SearchOptions, window_len: usize) -> usize {
    let profile_floor = match options.performance.profile {
        PerformanceProfile::Eco => 10_000,
        PerformanceProfile::Balanced | PerformanceProfile::Custom => 1_000_000,
        PerformanceProfile::Performance => 5_000_000,
        PerformanceProfile::Max => 20_000_000,
    };
    let chunk_floor = options.performance.limits.chunk_size;
    let memory_cap = options
        .performance
        .limits
        .memory_limit_mb
        .saturating_mul(1024 * 1024)
        .saturating_div(4)
        .max(window_len);
    profile_floor
        .max(chunk_floor)
        .min(memory_cap)
        .max(window_len)
}

fn request_growing_prefetch(
    source: &dyn DigitSource,
    offset: u64,
    options: &SearchOptions,
    window_len: usize,
) -> Result<()> {
    if source.is_growing() {
        let target_digits = offset
            .saturating_add(window_len as u64)
            .saturating_add(growing_prefetch_extra(options, window_len) as u64);
        source.request_prefetch(target_digits)?;
    }
    Ok(())
}

pub fn run_search<R: SearchReporter>(
    storage: &mut Storage,
    run: RunRecord,
    source: &dyn DigitSource,
    options: SearchOptions,
    reporter: &mut R,
) -> Result<RunRecord> {
    run_search_inner(storage, run, source, options, reporter, None)
}

pub fn run_search_controlled<R: SearchReporter>(
    storage: &mut Storage,
    run: RunRecord,
    source: &dyn DigitSource,
    options: SearchOptions,
    reporter: &mut R,
    control: mpsc::Receiver<SearchCommand>,
) -> Result<RunRecord> {
    run_search_inner(storage, run, source, options, reporter, Some(control))
}

fn run_search_inner<R: SearchReporter>(
    storage: &mut Storage,
    mut run: RunRecord,
    source: &dyn DigitSource,
    mut options: SearchOptions,
    reporter: &mut R,
    control: Option<mpsc::Receiver<SearchCommand>>,
) -> Result<RunRecord> {
    if options.threshold > 9 {
        bail!("threshold must be between 0 and 9");
    }
    if options.chunk_windows == 0 {
        bail!("chunk_windows must be non-zero");
    }
    if let Some(workers) = options.workers {
        options.performance.limits.cpu_workers = workers.max(1);
    }
    options.chunk_windows = options.performance.limits.chunk_size;
    options.checkpoint_every =
        Duration::from_secs(options.performance.limits.checkpoint_every_secs);

    let stop = Arc::new(AtomicBool::new(false));
    let stop_for_handler = stop.clone();
    let _ = ctrlc::set_handler(move || {
        stop_for_handler.store(true, AtomicOrdering::SeqCst);
    });

    let mut source_len = source.len()?;
    let (canvas_width, canvas_height) = search_canvas_size(&run, &options);
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
    let mut backend = create_search_backend(&options, &run.target_bitmap)?;
    let total_pixels = window_len as u32;
    let session_start = Instant::now();
    let invocation_start_offset = run.current_offset;
    let invocation_start_scanned = run.scanned_windows;
    let runtime_base = run.total_runtime_secs;
    let mut last_checkpoint = Instant::now();
    let mut recent_events = storage.history(&run.id, Some(8)).unwrap_or_default();
    let mut best_score = run.best_score;
    let mut paused = false;
    let mut runtime = RuntimeContext {
        last_chunk_processing: Duration::ZERO,
        checkpoint_count: 0,
        throttling_active: false,
        battery_throttle_active: false,
        window_len,
        active_backend: backend.name().to_string(),
        gpu_status: backend.gpu_status(),
    };
    if source.is_growing() {
        run.generated_digit_count = source_len;
    }

    run.status = RunStatus::Running;
    run.total_runtime_secs = runtime_base;
    storage.update_run(&mut run)?;
    reporter.on_update(&snapshot(
        &run,
        &recent_events,
        source,
        source_len,
        session_start,
        invocation_start_offset,
        invocation_start_scanned,
        runtime_base,
        &options,
        &runtime,
    ))?;

    loop {
        if source.is_growing() {
            source_len = source.len()?;
            run.generated_digit_count = source_len;
            request_growing_prefetch(source, run.current_offset, &options, window_len)?;
        }

        if handle_control_commands(
            control.as_ref(),
            storage,
            &mut run,
            &mut paused,
            reporter,
            &recent_events,
            source,
            source_len,
            session_start,
            invocation_start_offset,
            invocation_start_scanned,
            runtime_base,
            &mut options,
            &mut backend,
            &mut runtime,
        )? {
            finish(
                storage,
                &mut run,
                reporter,
                &recent_events,
                source,
                source_len,
                session_start,
                invocation_start_offset,
                invocation_start_scanned,
                runtime_base,
                &options,
                &runtime,
                FinishReason::Interrupted,
            )?;
            return Ok(run);
        }
        runtime.active_backend = backend.name().to_string();
        runtime.gpu_status = backend.gpu_status();

        if paused {
            reporter.on_update(&snapshot(
                &run,
                &recent_events,
                source,
                source_len,
                session_start,
                invocation_start_offset,
                invocation_start_scanned,
                runtime_base,
                &options,
                &runtime,
            ))?;
            thread::sleep(Duration::from_millis(100));
            continue;
        }

        if stop.load(AtomicOrdering::SeqCst) {
            finish(
                storage,
                &mut run,
                reporter,
                &recent_events,
                source,
                source_len,
                session_start,
                invocation_start_offset,
                invocation_start_scanned,
                runtime_base,
                &options,
                &runtime,
                FinishReason::Interrupted,
            )?;
            return Ok(run);
        }

        if source_len < window_len as u64 || run.current_offset + window_len as u64 > source_len {
            if source.is_growing() {
                request_growing_prefetch(source, run.current_offset, &options, window_len)?;
                source_len = source.len()?;
                run.generated_digit_count = source_len;
                if source_len >= window_len as u64
                    && run.current_offset + window_len as u64 <= source_len
                {
                    continue;
                }
                run.status = RunStatus::Running;
                run.total_runtime_secs = runtime_base + session_start.elapsed().as_secs_f64();
                if last_checkpoint.elapsed() >= options.checkpoint_every {
                    storage.update_run(&mut run)?;
                    runtime.checkpoint_count += 1;
                    last_checkpoint = Instant::now();
                }
                reporter.on_update(&snapshot(
                    &run,
                    &recent_events,
                    source,
                    source_len,
                    session_start,
                    invocation_start_offset,
                    invocation_start_scanned,
                    runtime_base,
                    &options,
                    &runtime,
                ))?;
                thread::sleep(GROWING_SOURCE_WAIT);
                continue;
            }
            finish(
                storage,
                &mut run,
                reporter,
                &recent_events,
                source,
                source_len,
                session_start,
                invocation_start_offset,
                invocation_start_scanned,
                runtime_base,
                &options,
                &runtime,
                FinishReason::SourceExhausted,
            )?;
            return Ok(run);
        }

        if let Some(max_offset) = options.max_offset {
            if run.current_offset >= max_offset {
                finish(
                    storage,
                    &mut run,
                    reporter,
                    &recent_events,
                    source,
                    source_len,
                    session_start,
                    invocation_start_offset,
                    invocation_start_scanned,
                    runtime_base,
                    &options,
                    &runtime,
                    FinishReason::MaxOffsetReached,
                )?;
                return Ok(run);
            }
        }

        if let Some(limit) = options.limit {
            let scanned_this_invocation = run.scanned_windows - invocation_start_scanned;
            if scanned_this_invocation >= limit {
                finish(
                    storage,
                    &mut run,
                    reporter,
                    &recent_events,
                    source,
                    source_len,
                    session_start,
                    invocation_start_offset,
                    invocation_start_scanned,
                    runtime_base,
                    &options,
                    &runtime,
                    FinishReason::LimitReached,
                )?;
                return Ok(run);
            }
        }

        let available_windows = source_len - run.current_offset - window_len as u64 + 1;
        let mut windows_to_scan = options.chunk_windows as u64;
        windows_to_scan = windows_to_scan.min(available_windows);
        if let Some(limit) = options.limit {
            windows_to_scan =
                windows_to_scan.min(limit - (run.scanned_windows - invocation_start_scanned));
        }
        if let Some(max_offset) = options.max_offset {
            windows_to_scan = windows_to_scan.min(max_offset - run.current_offset);
        }

        if windows_to_scan == 0 {
            continue;
        }

        let chunk_start_offset = run.current_offset;
        let chunk_start_scanned = run.scanned_windows;
        let read_len = windows_to_scan as usize + window_len - 1;
        if source.is_growing() {
            let prefetch_target = chunk_start_offset
                .saturating_add(read_len as u64)
                .saturating_add(growing_prefetch_extra(&options, window_len) as u64);
            source.request_prefetch(prefetch_target)?;
        }
        let digits = source.read_range(chunk_start_offset, read_len)?;
        if source.is_growing() {
            source_len = source.len()?;
            run.generated_digit_count = source_len;
        }
        if digits.len() < window_len {
            if source.is_growing() {
                reporter.on_update(&snapshot(
                    &run,
                    &recent_events,
                    source,
                    source_len,
                    session_start,
                    invocation_start_offset,
                    invocation_start_scanned,
                    runtime_base,
                    &options,
                    &runtime,
                ))?;
                thread::sleep(GROWING_SOURCE_WAIT);
                continue;
            }
            finish(
                storage,
                &mut run,
                reporter,
                &recent_events,
                source,
                source_len,
                session_start,
                invocation_start_offset,
                invocation_start_scanned,
                runtime_base,
                &options,
                &runtime,
                FinishReason::SourceExhausted,
            )?;
            return Ok(run);
        }
        let actual_windows = (digits.len() - window_len + 1).min(windows_to_scan as usize);
        let target = run.target_bitmap.clone();
        let emergence_plan = if options.match_mode == MatchMode::Emergence {
            Some(EmergencePlan::new(&target, canvas_width, canvas_height))
        } else {
            None
        };
        if options.performance.limits.pause_when_on_battery
            && matches!(on_battery_power(), Some(true))
        {
            runtime.battery_throttle_active = true;
            thread::sleep(Duration::from_secs(1));
        } else {
            runtime.battery_throttle_active = false;
        }
        let chunk_start = Instant::now();
        let params = SearchChunkParams {
            target: &target,
            mode: options.match_mode,
            canvas_width,
            canvas_height,
            threshold: options.threshold,
            invert: options.invert,
            window_len,
            emergence_plan: emergence_plan.as_ref(),
        };
        let mut scores = backend.search_chunk(&digits, actual_windows, &params)?;
        runtime.last_chunk_processing = measure_elapsed(chunk_start);

        let mut perfect_found = false;
        for score in &scores {
            if score.score > best_score {
                let (bitmap, details) = build_match_output(
                    &digits[score.index..score.index + window_len],
                    score,
                    &target,
                    options.match_mode,
                    canvas_width,
                    canvas_height,
                    options.threshold,
                )?;
                let offset = chunk_start_offset + score.index as u64;
                let event = BestEventRecord {
                    id: 0,
                    run_id: run.id.clone(),
                    timestamp: Utc::now().to_rfc3339(),
                    offset,
                    score: score.score,
                    bitmap: bitmap.clone(),
                    inverted: score.inverted,
                    scanned_windows: chunk_start_scanned + score.index as u64 + 1,
                    details: details.clone(),
                };
                best_score = score.score;
                run.best_score = event.score;
                run.best_offset = Some(offset);
                run.best_bitmap = Some(bitmap);
                run.best_inverted = score.inverted;
                run.best_match = details;
                run.current_offset = offset + 1;
                run.scanned_windows = event.scanned_windows;
                run.total_runtime_secs = runtime_base + session_start.elapsed().as_secs_f64();
                merge_top_match(
                    &mut run.top_matches,
                    TopMatch {
                        offset,
                        score: event.score,
                        bitmap: event.bitmap.clone(),
                        inverted: event.inverted,
                        scanned_windows: event.scanned_windows,
                        details: event.details.clone(),
                    },
                    options.top_n,
                );
                storage.insert_best_event(&event)?;
                storage.update_run(&mut run)?;
                recent_events.push(event.clone());
                if recent_events.len() > 8 {
                    recent_events.remove(0);
                }
                reporter.on_new_best(
                    &snapshot(
                        &run,
                        &recent_events,
                        source,
                        source_len,
                        session_start,
                        invocation_start_offset,
                        invocation_start_scanned,
                        runtime_base,
                        &options,
                        &runtime,
                    ),
                    &event,
                )?;
                if score.score >= 1.0 && !options.keep_going_after_perfect {
                    perfect_found = true;
                    break;
                }
            }
        }

        if perfect_found {
            finish(
                storage,
                &mut run,
                reporter,
                &recent_events,
                source,
                source_len,
                session_start,
                invocation_start_offset,
                invocation_start_scanned,
                runtime_base,
                &options,
                &runtime,
                FinishReason::PerfectFound,
            )?;
            return Ok(run);
        }

        merge_chunk_top_matches(
            &mut run.top_matches,
            &mut scores,
            &digits,
            chunk_start_offset,
            chunk_start_scanned,
            run.width as usize,
            run.height as usize,
            canvas_width,
            canvas_height,
            options.match_mode,
            options.threshold,
            total_pixels,
            options.top_n,
        )?;

        run.current_offset = chunk_start_offset + actual_windows as u64;
        run.scanned_windows = chunk_start_scanned + actual_windows as u64;
        run.total_runtime_secs = runtime_base + session_start.elapsed().as_secs_f64();

        let due_checkpoint = last_checkpoint.elapsed() >= options.checkpoint_every;
        if due_checkpoint {
            storage.update_run(&mut run)?;
            runtime.checkpoint_count += 1;
            last_checkpoint = Instant::now();
        }
        runtime.throttling_active = options
            .performance
            .throttle_after_batch(runtime.last_chunk_processing);

        reporter.on_update(&snapshot(
            &run,
            &recent_events,
            source,
            source_len,
            session_start,
            invocation_start_offset,
            invocation_start_scanned,
            runtime_base,
            &options,
            &runtime,
        ))?;
    }
}

#[allow(clippy::too_many_arguments)]
fn handle_control_commands<R: SearchReporter>(
    control: Option<&mpsc::Receiver<SearchCommand>>,
    storage: &mut Storage,
    run: &mut RunRecord,
    paused: &mut bool,
    reporter: &mut R,
    recent_events: &[BestEventRecord],
    source: &dyn DigitSource,
    source_len: u64,
    session_start: Instant,
    invocation_start_offset: u64,
    invocation_start_scanned: u64,
    runtime_base: f64,
    options: &mut SearchOptions,
    backend: &mut Box<dyn SearchBackend>,
    runtime: &mut RuntimeContext,
) -> Result<bool> {
    let Some(control) = control else {
        return Ok(false);
    };

    let mut should_stop = false;
    let mut changed = false;
    while let Ok(command) = control.try_recv() {
        match command {
            SearchCommand::Pause => {
                *paused = true;
                run.status = RunStatus::Paused;
                changed = true;
            }
            SearchCommand::Resume => {
                *paused = false;
                run.status = RunStatus::Running;
                changed = true;
            }
            SearchCommand::SaveCheckpoint => {
                changed = true;
            }
            SearchCommand::Stop => {
                should_stop = true;
                changed = true;
            }
            SearchCommand::SetProfile(profile) => {
                options.performance = PerformanceSettings::from_profile(
                    profile,
                    options.performance.backend,
                    options.performance.generator_backend,
                    options.performance.gpu,
                    options.performance.gpu_device.clone(),
                    options.performance.thermal_mode,
                    options.performance.stress_test,
                    options.performance.show_metrics,
                    options.match_mode,
                    PerformanceOverrides::default(),
                );
                options.chunk_windows = options.performance.limits.chunk_size;
                options.checkpoint_every =
                    Duration::from_secs(options.performance.limits.checkpoint_every_secs);
                *backend = create_search_backend(options, &run.target_bitmap)?;
                changed = true;
            }
            SearchCommand::AdjustWorkers(delta) => {
                let current = options.performance.limits.cpu_workers as i32;
                let workers = (current + delta).max(1) as usize;
                options.performance.limits.cpu_workers = workers;
                backend.reconfigure(workers)?;
                changed = true;
            }
            SearchCommand::AdjustChunkSize(delta) => {
                let current = options.performance.limits.chunk_size as i64;
                let step = (current / 4).max(1);
                let chunk = if delta >= 0 {
                    current.saturating_add(step * delta as i64)
                } else {
                    current.saturating_sub(step * (-delta) as i64)
                };
                options.performance.limits.chunk_size = chunk.max(1) as usize;
                options.chunk_windows = options.performance.limits.chunk_size;
                changed = true;
            }
            SearchCommand::CycleThermalMode => {
                options.performance.thermal_mode = options.performance.thermal_mode.next();
                changed = true;
            }
            SearchCommand::ToggleMetrics => {
                options.performance.show_metrics = !options.performance.show_metrics;
                changed = true;
            }
            SearchCommand::ToggleGpuMode => {
                options.performance.gpu = match options.performance.gpu {
                    crate::performance::GpuMode::Off => crate::performance::GpuMode::Auto,
                    crate::performance::GpuMode::Auto => crate::performance::GpuMode::On,
                    crate::performance::GpuMode::On => crate::performance::GpuMode::Off,
                };
                *backend = create_search_backend(options, &run.target_bitmap)?;
                changed = true;
            }
        }
    }

    if changed {
        runtime.active_backend = backend.name().to_string();
        runtime.gpu_status = backend.gpu_status();
        run.total_runtime_secs = runtime_base + session_start.elapsed().as_secs_f64();
        storage.update_run(run)?;
        reporter.on_update(&snapshot(
            run,
            recent_events,
            source,
            source_len,
            session_start,
            invocation_start_offset,
            invocation_start_scanned,
            runtime_base,
            options,
            runtime,
        ))?;
    }

    Ok(should_stop)
}

#[allow(clippy::too_many_arguments)]
fn score_candidate_window(
    index: usize,
    digits: &[u8],
    target: &Bitmap,
    mode: MatchMode,
    canvas_width: usize,
    canvas_height: usize,
    threshold: u8,
    invert: bool,
    emergence_plan: Option<&EmergencePlan>,
) -> WindowScore {
    match mode {
        MatchMode::Emergence => {
            if let Some(plan) = emergence_plan {
                score_emergence_window_with_plan(index, digits, plan)
            } else {
                score_emergence_window(index, digits, target, canvas_width, canvas_height)
            }
        }
        MatchMode::Threshold | MatchMode::Exact => {
            let matching = score_threshold_window(digits, &target.pixels, threshold);
            let total_pixels = target.pixels.len() as u32;
            let inverted_matching = total_pixels - matching;
            let (matched, inverted) = if invert && inverted_matching > matching {
                (inverted_matching, true)
            } else {
                (matching, false)
            };
            WindowScore {
                index,
                score: matched as f64 / total_pixels as f64,
                inverted,
                digit: None,
                x: None,
                y: None,
                coverage: None,
                leakage: None,
            }
        }
    }
}

fn score_threshold_window(digits: &[u8], target: &[u8], threshold: u8) -> u32 {
    digits
        .iter()
        .zip(target.iter())
        .filter(|(digit, target_pixel)| u8::from(**digit >= threshold) == **target_pixel)
        .count() as u32
}

impl EmergencePlan {
    fn new(target: &Bitmap, canvas_width: usize, canvas_height: usize) -> Self {
        let shape_pixels = target.pixels.iter().filter(|pixel| **pixel == 1).count();
        let background_pixels = target.pixels.len().saturating_sub(shape_pixels);
        let mut placements = Vec::new();
        for y_offset in 0..=canvas_height - target.height {
            for x_offset in 0..=canvas_width - target.width {
                let mut shape_offsets = Vec::with_capacity(shape_pixels);
                let mut background_offsets = Vec::with_capacity(background_pixels);
                for target_y in 0..target.height {
                    for target_x in 0..target.width {
                        let offset = (y_offset + target_y) * canvas_width + x_offset + target_x;
                        if target.get(target_x, target_y) == 1 {
                            shape_offsets.push(offset);
                        } else {
                            background_offsets.push(offset);
                        }
                    }
                }
                placements.push(EmergencePlacement {
                    x: x_offset,
                    y: y_offset,
                    shape_offsets,
                    background_offsets,
                });
            }
        }
        Self {
            shape_pixels,
            background_pixels,
            placements,
        }
    }
}

fn score_emergence_window_with_plan(
    index: usize,
    digits: &[u8],
    plan: &EmergencePlan,
) -> WindowScore {
    let mut best = WindowScore {
        index,
        score: 0.0,
        inverted: false,
        digit: Some(0),
        x: Some(0),
        y: Some(0),
        coverage: Some(0.0),
        leakage: Some(0.0),
    };

    if plan.shape_pixels == 0 {
        return best;
    }

    for placement in &plan.placements {
        let mut shape_counts = [0usize; 10];
        let mut background_counts = [0usize; 10];
        for offset in &placement.shape_offsets {
            shape_counts[digits[*offset] as usize] += 1;
        }
        for offset in &placement.background_offsets {
            background_counts[digits[*offset] as usize] += 1;
        }
        for digit in 0..=9 {
            let matched_shape = shape_counts[digit];
            let leaked = background_counts[digit];
            let coverage = matched_shape as f64 / plan.shape_pixels as f64;
            let leakage = if plan.background_pixels == 0 {
                0.0
            } else {
                leaked as f64 / plan.background_pixels as f64
            };
            let score = emergence_score(coverage, leakage);
            let is_better = score > best.score
                || (score == best.score && coverage > best.coverage.unwrap_or(0.0))
                || (score == best.score
                    && coverage == best.coverage.unwrap_or(0.0)
                    && leakage < best.leakage.unwrap_or(1.0));
            if is_better {
                best = WindowScore {
                    index,
                    score,
                    inverted: false,
                    digit: Some(digit as u8),
                    x: Some(placement.x),
                    y: Some(placement.y),
                    coverage: Some(coverage),
                    leakage: Some(leakage),
                };
            }
        }
    }

    best
}

fn score_emergence_window(
    index: usize,
    digits: &[u8],
    target: &Bitmap,
    canvas_width: usize,
    canvas_height: usize,
) -> WindowScore {
    let shape_pixels = target.pixels.iter().filter(|pixel| **pixel == 1).count();
    let background_pixels = target.pixels.len().saturating_sub(shape_pixels);
    let mut best = WindowScore {
        index,
        score: 0.0,
        inverted: false,
        digit: Some(0),
        x: Some(0),
        y: Some(0),
        coverage: Some(0.0),
        leakage: Some(0.0),
    };

    if shape_pixels == 0 {
        return best;
    }

    for y_offset in 0..=canvas_height - target.height {
        for x_offset in 0..=canvas_width - target.width {
            for digit in 0..=9 {
                let mut matched_shape = 0usize;
                let mut leaked = 0usize;
                for target_y in 0..target.height {
                    for target_x in 0..target.width {
                        let target_pixel = target.get(target_x, target_y);
                        let canvas_index =
                            (y_offset + target_y) * canvas_width + x_offset + target_x;
                        if digits[canvas_index] == digit {
                            if target_pixel == 1 {
                                matched_shape += 1;
                            } else {
                                leaked += 1;
                            }
                        }
                    }
                }

                let coverage = matched_shape as f64 / shape_pixels as f64;
                let leakage = if background_pixels == 0 {
                    0.0
                } else {
                    leaked as f64 / background_pixels as f64
                };
                let score = emergence_score(coverage, leakage);
                let is_better = score > best.score
                    || (score == best.score && coverage > best.coverage.unwrap_or(0.0))
                    || (score == best.score
                        && coverage == best.coverage.unwrap_or(0.0)
                        && leakage < best.leakage.unwrap_or(1.0));
                if is_better {
                    best = WindowScore {
                        index,
                        score,
                        inverted: false,
                        digit: Some(digit),
                        x: Some(x_offset),
                        y: Some(y_offset),
                        coverage: Some(coverage),
                        leakage: Some(leakage),
                    };
                }
            }
        }
    }

    best
}

#[allow(clippy::too_many_arguments)]
fn build_match_output(
    digits: &[u8],
    score: &WindowScore,
    target: &Bitmap,
    mode: MatchMode,
    canvas_width: usize,
    canvas_height: usize,
    threshold: u8,
) -> Result<(Bitmap, Option<BestMatchDetails>)> {
    match mode {
        MatchMode::Emergence => {
            let digit = score.digit.unwrap_or(0);
            let pixels = digits
                .iter()
                .map(|value| u8::from(*value == digit))
                .collect();
            let bitmap = Bitmap::new(canvas_width, canvas_height, pixels)?;
            let raw_canvas_digits = digits
                .iter()
                .map(|digit| char::from(b'0' + *digit))
                .collect();
            let details = BestMatchDetails {
                mode,
                digit: Some(digit),
                x: score.x.map(|value| value as u32),
                y: score.y.map(|value| value as u32),
                canvas_width: canvas_width as u32,
                canvas_height: canvas_height as u32,
                raw_canvas_digits: Some(raw_canvas_digits),
                coverage: score.coverage,
                leakage: score.leakage,
            };
            Ok((bitmap, Some(details)))
        }
        MatchMode::Threshold | MatchMode::Exact => {
            let bitmap = Bitmap::from_digit_window(digits, target.width, target.height, threshold)?;
            let details = BestMatchDetails {
                mode,
                digit: None,
                x: None,
                y: None,
                canvas_width: target.width as u32,
                canvas_height: target.height as u32,
                raw_canvas_digits: None,
                coverage: None,
                leakage: None,
            };
            Ok((bitmap, Some(details)))
        }
    }
}

fn search_canvas_size(run: &RunRecord, options: &SearchOptions) -> (usize, usize) {
    if options.match_mode.is_emergence() {
        (options.canvas_width, options.canvas_height)
    } else {
        (run.width as usize, run.height as usize)
    }
}

#[allow(clippy::too_many_arguments)]
fn finish<R: SearchReporter>(
    storage: &mut Storage,
    run: &mut RunRecord,
    reporter: &mut R,
    recent_events: &[BestEventRecord],
    source: &dyn DigitSource,
    source_len: u64,
    session_start: Instant,
    invocation_start_offset: u64,
    invocation_start_scanned: u64,
    runtime_base: f64,
    options: &SearchOptions,
    runtime: &RuntimeContext,
    reason: FinishReason,
) -> Result<()> {
    run.status = match reason {
        FinishReason::PerfectFound => RunStatus::PerfectFound,
        FinishReason::SourceExhausted => RunStatus::SourceExhausted,
        FinishReason::Interrupted | FinishReason::LimitReached | FinishReason::MaxOffsetReached => {
            RunStatus::Paused
        }
    };
    run.total_runtime_secs = runtime_base + session_start.elapsed().as_secs_f64();
    storage.update_run(run)?;
    reporter.on_finish(
        &snapshot(
            run,
            recent_events,
            source,
            source_len,
            session_start,
            invocation_start_offset,
            invocation_start_scanned,
            runtime_base,
            options,
            runtime,
        ),
        reason,
    )
}

#[allow(clippy::too_many_arguments)]
fn snapshot(
    run: &RunRecord,
    recent_events: &[BestEventRecord],
    source: &dyn DigitSource,
    source_len: u64,
    session_start: Instant,
    invocation_start_offset: u64,
    invocation_start_scanned: u64,
    _runtime_base: f64,
    options: &SearchOptions,
    runtime: &RuntimeContext,
) -> SearchSnapshot {
    let elapsed = session_start.elapsed();
    let scanned_this_invocation = run.scanned_windows - invocation_start_scanned;
    let speed = if elapsed.as_secs_f64() > 0.0 {
        scanned_this_invocation as f64 / elapsed.as_secs_f64()
    } else {
        0.0
    };
    let progress = if let Some(limit) = options.limit {
        if limit == 0 {
            Some(1.0)
        } else {
            Some((scanned_this_invocation as f64 / limit as f64).min(1.0))
        }
    } else if let Some(max_offset) = options.max_offset {
        let span = max_offset.saturating_sub(invocation_start_offset);
        if span == 0 {
            Some(1.0)
        } else {
            Some(
                (run.current_offset.saturating_sub(invocation_start_offset) as f64 / span as f64)
                    .min(1.0),
            )
        }
    } else {
        None
    };
    let (canvas_width, canvas_height) = search_canvas_size(run, options);
    let window_len = if options.match_mode.is_emergence() {
        (canvas_width * canvas_height) as u64
    } else {
        run.target_bitmap.pixels.len() as u64
    };
    let source_is_growing = source.is_growing();
    let waiting_for_digits = source_is_growing
        && (source_len < window_len || run.current_offset + window_len > source_len);
    let mut metrics = RuntimeMetrics::from_settings(
        &options.performance,
        runtime.last_chunk_processing,
        runtime.checkpoint_count,
        source_len,
        run.current_offset,
        source_len.saturating_sub(run.current_offset),
        runtime.window_len,
        runtime.throttling_active,
        runtime.battery_throttle_active,
    );
    metrics.search_backend = runtime.active_backend.clone();
    metrics.gpu_status = runtime.gpu_status.clone();
    SearchSnapshot {
        run: run.clone(),
        speed_windows_per_sec: speed,
        speed_digits_per_sec: speed,
        session_elapsed: elapsed,
        progress,
        recent_events: recent_events.to_vec(),
        source_kind: source.kind().to_string(),
        source_len,
        source_is_growing,
        waiting_for_digits,
        cache_gap_digits: source_len.saturating_sub(run.current_offset),
        metrics,
    }
}

#[allow(clippy::too_many_arguments)]
fn merge_chunk_top_matches(
    top_matches: &mut Vec<TopMatch>,
    scores: &mut [WindowScore],
    digits: &[u8],
    chunk_start_offset: u64,
    chunk_start_scanned: u64,
    width: usize,
    height: usize,
    canvas_width: usize,
    canvas_height: usize,
    match_mode: MatchMode,
    threshold: u8,
    _total_pixels: u32,
    top_n: usize,
) -> Result<()> {
    if top_n == 0 {
        top_matches.clear();
        return Ok(());
    }

    scores.sort_unstable_by(|left, right| {
        right
            .score
            .partial_cmp(&left.score)
            .unwrap_or(Ordering::Equal)
            .then_with(|| left.index.cmp(&right.index))
    });

    let window_len = if match_mode.is_emergence() {
        canvas_width * canvas_height
    } else {
        width * height
    };
    let target = Bitmap::blank(width, height);
    for score in scores.iter().take(top_n) {
        let offset = chunk_start_offset + score.index as u64;
        let (bitmap, details) = build_match_output(
            &digits[score.index..score.index + window_len],
            score,
            &target,
            match_mode,
            canvas_width,
            canvas_height,
            threshold,
        )?;
        merge_top_match(
            top_matches,
            TopMatch {
                offset,
                score: score.score,
                bitmap,
                inverted: score.inverted,
                scanned_windows: chunk_start_scanned + score.index as u64 + 1,
                details,
            },
            top_n,
        );
    }
    Ok(())
}

fn merge_top_match(top_matches: &mut Vec<TopMatch>, candidate: TopMatch, top_n: usize) {
    if top_n == 0 {
        top_matches.clear();
        return;
    }
    if top_matches
        .iter()
        .any(|item| item.offset == candidate.offset)
    {
        return;
    }
    top_matches.push(candidate);
    top_matches.sort_by(|left, right| compare_top(right, left));
    top_matches.truncate(top_n);
}

fn compare_top(left: &TopMatch, right: &TopMatch) -> Ordering {
    left.score
        .partial_cmp(&right.score)
        .unwrap_or(Ordering::Equal)
        .then_with(|| right.offset.cmp(&left.offset))
}

#[cfg(test)]
mod tests {
    use std::io::Write;
    use std::time::Duration;

    use anyhow::Result;
    use tempfile::{NamedTempFile, tempdir};

    use super::*;
    use crate::art::Bitmap;
    use crate::digits::{DigitSourceSpec, FileDigitSource};
    use crate::performance::{
        GeneratorBackendChoice, GpuMode, PerformanceProfile, SearchBackendChoice, ThermalMode,
    };
    use crate::storage::{NewRun, Storage};

    struct NullReporter;

    impl SearchReporter for NullReporter {
        fn on_update(&mut self, _snapshot: &SearchSnapshot) -> Result<()> {
            Ok(())
        }

        fn on_new_best(
            &mut self,
            _snapshot: &SearchSnapshot,
            _event: &BestEventRecord,
        ) -> Result<()> {
            Ok(())
        }

        fn on_finish(&mut self, _snapshot: &SearchSnapshot, _reason: FinishReason) -> Result<()> {
            Ok(())
        }
    }

    fn options(limit: Option<u64>) -> SearchOptions {
        SearchOptions {
            max_offset: None,
            limit,
            match_mode: MatchMode::Threshold,
            canvas_width: 2,
            canvas_height: 2,
            threshold: 5,
            invert: false,
            workers: None,
            checkpoint_every: Duration::from_secs(60),
            top_n: 10,
            keep_going_after_perfect: false,
            chunk_windows: 2,
            performance: PerformanceSettings::from_profile(
                PerformanceProfile::Custom,
                SearchBackendChoice::Cpu,
                GeneratorBackendChoice::Cpu,
                GpuMode::Off,
                None,
                ThermalMode::Normal,
                false,
                false,
                MatchMode::Threshold,
                PerformanceOverrides {
                    chunk_size: Some(2),
                    checkpoint_every_secs: Some(60),
                    ..PerformanceOverrides::default()
                },
            ),
        }
    }

    #[test]
    fn finds_exact_match_across_chunk_boundary() {
        let mut digits = NamedTempFile::new().unwrap();
        write!(digits, "110660123").unwrap();
        let source = FileDigitSource::new_with_options(digits.path().to_path_buf(), false);
        let dir = tempdir().unwrap();
        let mut storage = Storage::open_path(dir.path().join("state.db")).unwrap();
        let target = Bitmap::new(2, 2, vec![0, 1, 1, 0]).unwrap();
        let run = storage
            .create_run(NewRun {
                name: "boundary".to_string(),
                source: DigitSourceSpec::file(digits.path().to_path_buf(), false),
                template_name: None,
                art_hash: target.sha256(),
                width: 2,
                height: 2,
                canvas_width: 2,
                canvas_height: 2,
                match_mode: MatchMode::Threshold,
                threshold: 5,
                invert_enabled: false,
                start_offset: Some(0),
                target_bitmap: target,
                generated_digit_count: 0,
                params_json: "{}".to_string(),
            })
            .unwrap();

        let mut reporter = NullReporter;
        let run = run_search(&mut storage, run, &source, options(None), &mut reporter).unwrap();
        assert_eq!(run.status, RunStatus::PerfectFound);
        assert_eq!(run.best_offset, Some(2));
        assert_eq!(run.best_score, 1.0);
    }

    #[test]
    fn checkpoint_resume_continues_from_current_offset() {
        let mut digits = NamedTempFile::new().unwrap();
        write!(digits, "1110660123").unwrap();
        let source = FileDigitSource::new_with_options(digits.path().to_path_buf(), false);
        let dir = tempdir().unwrap();
        let mut storage = Storage::open_path(dir.path().join("state.db")).unwrap();
        let target = Bitmap::new(2, 2, vec![0, 1, 1, 0]).unwrap();
        let run = storage
            .create_run(NewRun {
                name: "resume".to_string(),
                source: DigitSourceSpec::file(digits.path().to_path_buf(), false),
                template_name: None,
                art_hash: target.sha256(),
                width: 2,
                height: 2,
                canvas_width: 2,
                canvas_height: 2,
                match_mode: MatchMode::Threshold,
                threshold: 5,
                invert_enabled: false,
                start_offset: Some(0),
                target_bitmap: target,
                generated_digit_count: 0,
                params_json: "{}".to_string(),
            })
            .unwrap();

        let mut reporter = NullReporter;
        let partial =
            run_search(&mut storage, run, &source, options(Some(2)), &mut reporter).unwrap();
        assert_eq!(partial.status, RunStatus::Paused);
        assert_eq!(partial.current_offset, 2);

        let loaded = storage.resolve_run("resume").unwrap();
        let resumed =
            run_search(&mut storage, loaded, &source, options(None), &mut reporter).unwrap();
        assert_eq!(resumed.status, RunStatus::PerfectFound);
        assert_eq!(resumed.best_offset, Some(3));
    }

    #[test]
    fn emergence_finds_shape_from_one_repeated_digit() {
        let mut digits = NamedTempFile::new().unwrap();
        write!(digits, "712173456").unwrap();
        let source = FileDigitSource::new_with_options(digits.path().to_path_buf(), false);
        let dir = tempdir().unwrap();
        let mut storage = Storage::open_path(dir.path().join("state.db")).unwrap();
        let target = Bitmap::new(2, 2, vec![1, 0, 0, 1]).unwrap();
        let run = storage
            .create_run(NewRun {
                name: "emergence".to_string(),
                source: DigitSourceSpec::file(digits.path().to_path_buf(), false),
                template_name: None,
                art_hash: target.sha256(),
                width: 2,
                height: 2,
                canvas_width: 3,
                canvas_height: 3,
                match_mode: MatchMode::Emergence,
                threshold: 5,
                invert_enabled: false,
                start_offset: Some(0),
                target_bitmap: target,
                generated_digit_count: 0,
                params_json: "{}".to_string(),
            })
            .unwrap();

        let mut reporter = NullReporter;
        let mut options = options(None);
        options.match_mode = MatchMode::Emergence;
        options.canvas_width = 3;
        options.canvas_height = 3;
        options.chunk_windows = 1;
        let run = run_search(&mut storage, run, &source, options, &mut reporter).unwrap();

        assert_eq!(run.status, RunStatus::PerfectFound);
        assert_eq!(run.best_offset, Some(0));
        let details = run.best_match.unwrap();
        assert_eq!(details.digit, Some(7));
        assert_eq!(details.x, Some(0));
        assert_eq!(details.y, Some(0));
        assert_eq!(details.raw_canvas_digits.as_deref(), Some("712173456"));
    }

    #[test]
    fn optimized_emergence_scoring_matches_reference() {
        let target = Bitmap::new(2, 2, vec![1, 0, 0, 1]).unwrap();
        let digits = vec![7, 1, 2, 1, 7, 3, 4, 5, 6];
        let plan = EmergencePlan::new(&target, 3, 3);
        let reference = score_emergence_window(0, &digits, &target, 3, 3);
        let optimized = score_emergence_window_with_plan(0, &digits, &plan);
        assert_eq!(optimized.digit, reference.digit);
        assert_eq!(optimized.x, reference.x);
        assert_eq!(optimized.y, reference.y);
        assert!((optimized.score - reference.score).abs() < f64::EPSILON);
        assert_eq!(optimized.coverage, reference.coverage);
        assert_eq!(optimized.leakage, reference.leakage);
    }
}
