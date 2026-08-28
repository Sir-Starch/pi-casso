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
use std::time::{Duration, Instant};

use anyhow::Result;
use chrono::Utc;

use crate::digits::DigitSource;
use crate::performance::{GpuMode, PerformanceOverrides, PerformanceProfile, PerformanceSettings};
use crate::search::scoring::{build_match_output, merge_top_match};
use crate::search::session::{ResourceBudget, SearchSession};
use crate::search::types::{SearchCommand, SearchOptions, SearchReporter, TopMatch};
use crate::storage::{BestEventRecord, RunRecord, RunStatus, Storage};

pub fn run_search<R: SearchReporter>(
    storage: &mut Storage,
    run: RunRecord,
    source: &dyn DigitSource,
    options: SearchOptions,
    reporter: &mut R,
) -> Result<RunRecord> {
    run_search_inner(storage, run, source, options, reporter, None, None)
}

pub fn run_search_with_budget<R: SearchReporter>(
    storage: &mut Storage,
    run: RunRecord,
    source: &dyn DigitSource,
    options: SearchOptions,
    reporter: &mut R,
    budget: Arc<ResourceBudget>,
) -> Result<RunRecord> {
    run_search_inner(storage, run, source, options, reporter, None, Some(budget))
}

pub fn run_search_controlled<R: SearchReporter>(
    storage: &mut Storage,
    run: RunRecord,
    source: &dyn DigitSource,
    options: SearchOptions,
    reporter: &mut R,
    control: mpsc::Receiver<SearchCommand>,
) -> Result<RunRecord> {
    run_search_inner(storage, run, source, options, reporter, Some(control), None)
}

fn run_search_inner<R: SearchReporter>(
    storage: &mut Storage,
    run: RunRecord,
    source: &dyn DigitSource,
    options: SearchOptions,
    reporter: &mut R,
    control: Option<mpsc::Receiver<SearchCommand>>,
    budget: Option<Arc<ResourceBudget>>,
) -> Result<RunRecord> {
    let mut session = SearchSession::new_with_budget(storage, run, source, options, budget)?;

    // Ctrl+C must leave a resumable checkpoint rather than killing the process
    // mid-chunk. Registration can fail if a handler is already installed (the
    // TUI installs its own), which is fine — the control channel covers that case.
    let stop = Arc::new(AtomicBool::new(false));
    let stop_for_handler = stop.clone();
    let _ = ctrlc::set_handler(move || {
        stop_for_handler.store(true, AtomicOrdering::SeqCst);
    });

    session.run.status = RunStatus::Running;
    let persistence_started = Instant::now();
    storage.update_run(&mut session.run)?;
    session
        .telemetry
        .record_persistence(persistence_started.elapsed());
    session.report(reporter)?;

    let reason = crate::search::session::run_pipeline(
        &mut session,
        storage,
        reporter,
        control.as_ref(),
        Arc::clone(&stop),
    )?;
    session.finish(storage, reporter, reason)?;
    Ok(session.run)
}

/// Persists every window that beats the running best. Returns true when a
/// perfect match should end the search.
#[allow(clippy::too_many_arguments)]
pub(crate) fn record_new_bests<R: SearchReporter>(
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
pub(crate) fn handle_control_commands<R: SearchReporter>(
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
                let workers = session
                    .options
                    .performance
                    .limits
                    .cpu_workers
                    .min(session.budget.cpu_permits_max()?);
                session.options.performance.limits.cpu_workers = workers;
                session.backend.reconfigure(workers)?;
            }
            SearchCommand::AdjustWorkers(delta) => {
                let current = session.options.performance.limits.cpu_workers as i32;
                let workers =
                    ((current + delta).max(1) as usize).min(session.budget.cpu_permits_max()?);
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
