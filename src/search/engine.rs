//! The search loop: pull digits, score them, record anything better than the
//! current best, checkpoint, repeat.
//!
//! The loop body is expressed as a sequence of steps that each either continue
//! or ask to stop with a reason. That replaces the six duplicated "write final
//! state and return" blocks the loop used to carry.

use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering as AtomicOrdering},
    mpsc,
};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::Result;
use chrono::Utc;

use crate::digits::DigitSource;
use crate::performance::{
    GpuMode, PerformanceOverrides, PerformanceProfile, PerformanceSettings, measure_elapsed,
    on_battery_power,
};
use crate::search::backend::SearchChunkParams;
use crate::search::scoring::{
    EmergencePlan, build_match_output, merge_chunk_top_matches, merge_top_match,
};
use crate::search::session::SearchSession;
use crate::search::types::{
    FinishReason, MatchMode, SearchCommand, SearchOptions, SearchReporter, TopMatch,
};
use crate::storage::{BestEventRecord, RunRecord, RunStatus, Storage};

/// How long to idle before re-checking a still-growing digit source. Short
/// enough that the search resumes promptly, long enough not to spin a core.
const GROWING_SOURCE_WAIT: Duration = Duration::from_millis(50);

/// What one turn of the loop decided.
enum Step {
    /// Keep going.
    Continue,
    /// Wind the search down for this reason.
    Stop(FinishReason),
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
    run: RunRecord,
    source: &dyn DigitSource,
    options: SearchOptions,
    reporter: &mut R,
    control: Option<mpsc::Receiver<SearchCommand>>,
) -> Result<RunRecord> {
    let mut session = SearchSession::new(storage, run, source, options)?;

    // Ctrl+C must leave a resumable checkpoint rather than killing the process
    // mid-chunk. Registration can fail if a handler is already installed (the
    // TUI installs its own), which is fine — the control channel covers that case.
    let stop = Arc::new(AtomicBool::new(false));
    let stop_for_handler = stop.clone();
    let _ = ctrlc::set_handler(move || {
        stop_for_handler.store(true, AtomicOrdering::SeqCst);
    });

    session.run.status = RunStatus::Running;
    storage.update_run(&mut session.run)?;
    session.report(reporter)?;

    loop {
        match step(&mut session, storage, reporter, control.as_ref(), &stop)? {
            Step::Continue => {}
            Step::Stop(reason) => {
                session.finish(storage, reporter, reason)?;
                return Ok(session.run);
            }
        }
    }
}

fn step<R: SearchReporter>(
    session: &mut SearchSession<'_>,
    storage: &mut Storage,
    reporter: &mut R,
    control: Option<&mpsc::Receiver<SearchCommand>>,
    stop: &AtomicBool,
) -> Result<Step> {
    session.refresh_source_len()?;
    if session.source.is_growing() {
        request_growing_prefetch(session, session.run.current_offset)?;
    }

    if handle_control_commands(session, storage, reporter, control)? {
        return Ok(Step::Stop(FinishReason::Interrupted));
    }
    session.sync_backend_labels();

    if session.paused {
        session.report(reporter)?;
        thread::sleep(Duration::from_millis(100));
        return Ok(Step::Continue);
    }

    if stop.load(AtomicOrdering::SeqCst) {
        return Ok(Step::Stop(FinishReason::Interrupted));
    }

    match ensure_digits_available(session, storage, reporter)? {
        DigitAvailability::Ready => {}
        DigitAvailability::Waiting => return Ok(Step::Continue),
        DigitAvailability::Exhausted => return Ok(Step::Stop(FinishReason::SourceExhausted)),
    }

    if let Some(max_offset) = session.options.max_offset {
        if session.run.current_offset >= max_offset {
            return Ok(Step::Stop(FinishReason::MaxOffsetReached));
        }
    }
    if let Some(limit) = session.options.limit {
        if session.scanned_this_invocation() >= limit {
            return Ok(Step::Stop(FinishReason::LimitReached));
        }
    }

    scan_one_chunk(session, storage, reporter)
}

enum DigitAvailability {
    Ready,
    /// A growing source has not caught up yet; the caller should loop again.
    Waiting,
    Exhausted,
}

/// A finite file simply runs out. A generated cache only *temporarily* runs out,
/// so the search idles and reports "waiting for more pi" instead of stopping.
fn ensure_digits_available<R: SearchReporter>(
    session: &mut SearchSession<'_>,
    storage: &mut Storage,
    reporter: &mut R,
) -> Result<DigitAvailability> {
    let window_len = session.window_len as u64;
    let has_full_window = session.source_len >= window_len
        && session.run.current_offset + window_len <= session.source_len;
    if has_full_window {
        return Ok(DigitAvailability::Ready);
    }
    if !session.source.is_growing() {
        return Ok(DigitAvailability::Exhausted);
    }

    request_growing_prefetch(session, session.run.current_offset)?;
    session.refresh_source_len()?;
    if session.source_len >= window_len
        && session.run.current_offset + window_len <= session.source_len
    {
        return Ok(DigitAvailability::Waiting);
    }

    session.run.status = RunStatus::Running;
    session.touch_runtime();
    session.checkpoint_if_due(storage)?;
    session.report(reporter)?;
    thread::sleep(GROWING_SOURCE_WAIT);
    Ok(DigitAvailability::Waiting)
}

fn scan_one_chunk<R: SearchReporter>(
    session: &mut SearchSession<'_>,
    storage: &mut Storage,
    reporter: &mut R,
) -> Result<Step> {
    let window_len = session.window_len;
    let available_windows = session.source_len - session.run.current_offset - window_len as u64 + 1;
    let mut windows_to_scan = (session.options.chunk_windows as u64).min(available_windows);
    if let Some(limit) = session.options.limit {
        windows_to_scan = windows_to_scan.min(limit - session.scanned_this_invocation());
    }
    if let Some(max_offset) = session.options.max_offset {
        windows_to_scan = windows_to_scan.min(max_offset - session.run.current_offset);
    }
    if windows_to_scan == 0 {
        return Ok(Step::Continue);
    }

    let chunk_start_offset = session.run.current_offset;
    let chunk_start_scanned = session.run.scanned_windows;
    // One extra window's worth of digits so the last window in the chunk is whole.
    let read_len = windows_to_scan as usize + window_len - 1;
    if session.source.is_growing() {
        let target = chunk_start_offset
            .saturating_add(read_len as u64)
            .saturating_add(growing_prefetch_extra(&session.options, window_len) as u64);
        session.source.request_prefetch(target)?;
    }
    let digits = session.source.read_range(chunk_start_offset, read_len)?;
    session.refresh_source_len()?;

    if digits.len() < window_len {
        if session.source.is_growing() {
            session.report(reporter)?;
            thread::sleep(GROWING_SOURCE_WAIT);
            return Ok(Step::Continue);
        }
        return Ok(Step::Stop(FinishReason::SourceExhausted));
    }

    let actual_windows = (digits.len() - window_len + 1).min(windows_to_scan as usize);
    let target = session.run.target_bitmap.clone();
    let emergence_plan = if session.options.match_mode == MatchMode::Emergence {
        Some(EmergencePlan::new(
            &target,
            session.canvas_width,
            session.canvas_height,
        ))
    } else {
        None
    };

    if session.options.performance.limits.pause_when_on_battery
        && matches!(on_battery_power(), Some(true))
    {
        session.runtime.battery_throttle_active = true;
        thread::sleep(Duration::from_secs(1));
    } else {
        session.runtime.battery_throttle_active = false;
    }

    let chunk_start = Instant::now();
    let params = SearchChunkParams {
        target: &target,
        mode: session.options.match_mode,
        canvas_width: session.canvas_width,
        canvas_height: session.canvas_height,
        threshold: session.options.threshold,
        invert: session.options.invert,
        window_len,
        emergence_plan: emergence_plan.as_ref(),
    };
    let mut scores = session
        .backend
        .search_chunk(&digits, actual_windows, &params)?;
    session.runtime.last_chunk_processing = measure_elapsed(chunk_start);

    if record_new_bests(
        session,
        storage,
        reporter,
        &digits,
        &scores,
        &target,
        chunk_start_offset,
        chunk_start_scanned,
    )? {
        return Ok(Step::Stop(FinishReason::PerfectFound));
    }

    merge_chunk_top_matches(
        &mut session.run.top_matches,
        &mut scores,
        &digits,
        chunk_start_offset,
        chunk_start_scanned,
        session.run.width as usize,
        session.run.height as usize,
        session.canvas_width,
        session.canvas_height,
        session.options.match_mode,
        session.options.threshold,
        session.options.top_n,
    )?;

    session.run.current_offset = chunk_start_offset + actual_windows as u64;
    session.run.scanned_windows = chunk_start_scanned + actual_windows as u64;
    session.record_progress();
    session.touch_runtime();
    session.checkpoint_if_due(storage)?;
    session.runtime.throttling_active = session
        .options
        .performance
        .throttle_after_batch(session.runtime.last_chunk_processing);
    session.report(reporter)?;
    Ok(Step::Continue)
}

/// Persists every window that beats the running best. Returns true when a
/// perfect match should end the search.
#[allow(clippy::too_many_arguments)]
fn record_new_bests<R: SearchReporter>(
    session: &mut SearchSession<'_>,
    storage: &mut Storage,
    reporter: &mut R,
    digits: &[u8],
    scores: &[crate::search::types::WindowScore],
    target: &crate::art::Bitmap,
    chunk_start_offset: u64,
    chunk_start_scanned: u64,
) -> Result<bool> {
    let window_len = session.window_len;
    for score in scores {
        if score.score <= session.run.best_score {
            continue;
        }
        let (bitmap, details) = build_match_output(
            &digits[score.index..score.index + window_len],
            score,
            target,
            session.options.match_mode,
            session.canvas_width,
            session.canvas_height,
            session.options.threshold,
        )?;
        let offset = chunk_start_offset + score.index as u64;
        let event = BestEventRecord {
            id: 0,
            run_id: session.run.id.clone(),
            timestamp: Utc::now().to_rfc3339(),
            offset,
            score: score.score,
            bitmap: bitmap.clone(),
            inverted: score.inverted,
            scanned_windows: chunk_start_scanned + score.index as u64 + 1,
            details: details.clone(),
        };

        session.run.best_score = event.score;
        session.run.best_offset = Some(offset);
        session.run.best_bitmap = Some(bitmap);
        session.run.best_inverted = score.inverted;
        session.run.best_match = details;
        // Positioned just past the winning window so an interrupt here resumes
        // without rescanning it.
        session.run.current_offset = offset + 1;
        session.run.scanned_windows = event.scanned_windows;
        session.touch_runtime();

        let top_n = session.options.top_n;
        merge_top_match(
            &mut session.run.top_matches,
            TopMatch {
                offset,
                score: event.score,
                bitmap: event.bitmap.clone(),
                inverted: event.inverted,
                scanned_windows: event.scanned_windows,
                details: event.details.clone(),
            },
            top_n,
        );
        storage.insert_best_event(&event)?;
        storage.update_run(&mut session.run)?;
        session.push_event(event.clone());
        reporter.on_new_best(&session.snapshot(), &event)?;

        if score.score >= 1.0 && !session.options.keep_going_after_perfect {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Applies every command queued by the front-end. Returns true when the search
/// was asked to stop.
fn handle_control_commands<R: SearchReporter>(
    session: &mut SearchSession<'_>,
    storage: &mut Storage,
    reporter: &mut R,
    control: Option<&mpsc::Receiver<SearchCommand>>,
) -> Result<bool> {
    let Some(control) = control else {
        return Ok(false);
    };

    let mut should_stop = false;
    let mut changed = false;
    while let Ok(command) = control.try_recv() {
        changed = true;
        match command {
            SearchCommand::Pause => {
                session.paused = true;
                session.run.status = RunStatus::Paused;
            }
            SearchCommand::Resume => {
                session.paused = false;
                session.run.status = RunStatus::Running;
            }
            // Both are handled by the unconditional checkpoint below.
            SearchCommand::SaveCheckpoint => {}
            SearchCommand::Stop => should_stop = true,
            SearchCommand::SetProfile(profile) => {
                apply_profile(session, profile);
                session.rebuild_backend()?;
            }
            SearchCommand::AdjustWorkers(delta) => {
                let current = session.options.performance.limits.cpu_workers as i32;
                let workers = (current + delta).max(1) as usize;
                session.options.performance.limits.cpu_workers = workers;
                session.backend.reconfigure(workers)?;
            }
            SearchCommand::AdjustChunkSize(delta) => {
                adjust_chunk_size(session, delta);
            }
            SearchCommand::CycleThermalMode => {
                session.options.performance.thermal_mode =
                    session.options.performance.thermal_mode.next();
            }
            SearchCommand::ToggleMetrics => {
                session.options.performance.show_metrics =
                    !session.options.performance.show_metrics;
            }
            SearchCommand::ToggleGpuMode => {
                session.options.performance.gpu = match session.options.performance.gpu {
                    GpuMode::Off => GpuMode::Auto,
                    GpuMode::Auto => GpuMode::On,
                    GpuMode::On => GpuMode::Off,
                };
                session.rebuild_backend()?;
            }
        }
    }

    if changed {
        session.sync_backend_labels();
        session.touch_runtime();
        storage.update_run(&mut session.run)?;
        session.report(reporter)?;
    }
    Ok(should_stop)
}

fn apply_profile(session: &mut SearchSession<'_>, profile: PerformanceProfile) {
    let performance = &session.options.performance;
    session.options.performance = PerformanceSettings::from_profile(
        profile,
        performance.backend,
        performance.generator_backend,
        performance.gpu,
        performance.gpu_device.clone(),
        performance.thermal_mode,
        performance.stress_test,
        performance.show_metrics,
        session.options.match_mode,
        PerformanceOverrides::default(),
    );
    session.options.chunk_windows = session.options.performance.limits.chunk_size;
    session.options.checkpoint_every =
        Duration::from_secs(session.options.performance.limits.checkpoint_every_secs);
}

/// Chunk size moves in quarters of its current value, so one keypress is a
/// meaningful change whether the chunk is a thousand windows or a million.
fn adjust_chunk_size(session: &mut SearchSession<'_>, delta: i32) {
    let current = session.options.performance.limits.chunk_size as i64;
    let step = (current / 4).max(1);
    let chunk = current.saturating_add(step * delta as i64).max(1) as usize;
    session.options.performance.limits.chunk_size = chunk;
    session.options.chunk_windows = chunk;
}

/// How far ahead of the search the generator should stay. Reading right at the
/// generator's edge would stall on every chunk, so each profile keeps a buffer
/// sized to how fast it consumes digits — capped by the memory budget.
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

fn request_growing_prefetch(session: &SearchSession<'_>, offset: u64) -> Result<()> {
    if !session.source.is_growing() {
        return Ok(());
    }
    let target_digits = offset
        .saturating_add(session.window_len as u64)
        .saturating_add(growing_prefetch_extra(&session.options, session.window_len) as u64);
    session.source.request_prefetch(target_digits)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::performance::{
        GeneratorBackendChoice, PerformanceOverrides, SearchBackendChoice, ThermalMode,
    };
    use crate::search::types::MatchMode;

    fn options(profile: PerformanceProfile, chunk: usize, memory_mb: usize) -> SearchOptions {
        SearchOptions {
            max_offset: None,
            limit: None,
            match_mode: MatchMode::Emergence,
            canvas_width: 24,
            canvas_height: 24,
            threshold: 5,
            invert: false,
            workers: None,
            checkpoint_every: Duration::from_secs(5),
            top_n: 10,
            keep_going_after_perfect: false,
            chunk_windows: chunk,
            performance: PerformanceSettings::from_profile(
                profile,
                SearchBackendChoice::Cpu,
                GeneratorBackendChoice::Cpu,
                GpuMode::Off,
                None,
                ThermalMode::Normal,
                false,
                false,
                MatchMode::Emergence,
                PerformanceOverrides {
                    chunk_size: Some(chunk),
                    memory_limit_mb: Some(memory_mb),
                    ..PerformanceOverrides::default()
                },
            ),
        }
    }

    #[test]
    fn prefetch_grows_with_the_profile() {
        let eco = growing_prefetch_extra(&options(PerformanceProfile::Eco, 1_000, 4_096), 576);
        let max = growing_prefetch_extra(&options(PerformanceProfile::Max, 1_000, 4_096), 576);
        assert!(max > eco);
    }

    #[test]
    fn prefetch_never_exceeds_the_memory_budget() {
        // A 1 MB budget cannot justify buffering twenty million digits.
        let extra = growing_prefetch_extra(&options(PerformanceProfile::Max, 1_000, 1), 576);
        assert!(extra <= 1024 * 1024 / 4);
    }

    #[test]
    fn prefetch_always_covers_at_least_one_window() {
        let extra = growing_prefetch_extra(&options(PerformanceProfile::Eco, 1, 0), 4_096);
        assert!(extra >= 4_096);
    }
}
