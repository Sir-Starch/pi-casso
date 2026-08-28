use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, mpsc::Receiver};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Result, bail};

use crate::performance::on_battery_power;
use crate::search::engine::{handle_control_commands, record_new_bests};
use crate::search::scoring::merge_chunk_top_matches;
use crate::search::session::SearchSession;
use crate::search::session::resource_budget::{ResourceBudget, test_consumer_delay};
use crate::search::types::{FinishReason, SearchCommand, SearchOptions, SearchReporter};
use crate::storage::Storage;

use super::worker::BackendResult;

pub(super) fn reduce<R: SearchReporter>(
    session: &mut SearchSession<'_>,
    storage: &mut Storage,
    reporter: &mut R,
    rx: Receiver<BackendResult>,
    budget: Arc<ResourceBudget>,
    stop: Arc<AtomicBool>,
    control: Option<&Receiver<SearchCommand>>,
) -> Result<FinishReason> {
    let mut reorder = BTreeMap::new();
    let mut next_offset = session.run.current_offset;
    let mut completed_windows = 0_u64;
    let consumer_delay = test_consumer_delay();

    loop {
        if handle_control_commands(session, storage, reporter, control)? {
            stop.store(true, Ordering::SeqCst);
            return Ok(FinishReason::Interrupted);
        }
        if session.paused {
            session.report(reporter)?;
            thread::sleep(Duration::from_millis(100));
            continue;
        }
        let received = match rx.recv_timeout(Duration::from_millis(25)) {
            Ok(result) => result,
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                if stop.load(Ordering::SeqCst) {
                    return Ok(FinishReason::Interrupted);
                }
                if session.source.is_growing() {
                    session.report(reporter)?;
                }
                continue;
            }
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
        };
        let start = received.chunk.start_offset;
        reorder.insert(start, received);
        session.runtime.reducer_max_reorder_depth = session
            .runtime
            .reducer_max_reorder_depth
            .max(u64::try_from(reorder.len())?);

        while let Some(mut result) = reorder.remove(&next_offset) {
            if !consumer_delay.is_zero() {
                thread::sleep(consumer_delay);
            }
            session.telemetry.record_read(result.chunk.read);
            session.telemetry.record_parse(result.chunk.parse);
            session.telemetry.record_cache_hit(result.chunk.cache_hit);
            session.runtime.last_chunk_processing = result.processing;
            session.telemetry.record_gpu_stages(&result.gpu);
            session.telemetry.record_backend_compute(
                result
                    .processing
                    .saturating_sub(result.gpu.accounted_duration()),
            );
            if let Some(reason) = result.fallback_reason.clone() {
                session.telemetry.record_cpu_chunk();
                session.telemetry.record_fallback(reason);
            } else if !result.gpu.fallback_reason.is_empty() {
                session.telemetry.record_cpu_chunk();
                session
                    .telemetry
                    .record_fallback(result.gpu.fallback_reason.clone());
            } else if result.used_accelerator {
                session
                    .telemetry
                    .record_accelerator_chunk(result.gpu.completions);
            } else {
                session.telemetry.record_cpu_chunk();
            }

            let mut scores = match std::mem::replace(&mut result.scores, Ok(Vec::new())) {
                Ok(scores) => scores,
                Err(error) => {
                    stop.store(true, Ordering::SeqCst);
                    return Err(error);
                }
            };
            let target = session.run.target_bitmap.clone();
            let persistence_started = Instant::now();
            if session.options.performance.limits.pause_when_on_battery
                && matches!(on_battery_power(), Some(true))
            {
                session.runtime.battery_throttle_active = true;
                let battery_started = Instant::now();
                thread::sleep(Duration::from_secs(1));
                session
                    .telemetry
                    .record_throttle_wait(battery_started.elapsed());
            } else {
                session.runtime.battery_throttle_active = false;
            }
            let perfect_found = record_new_bests(
                session,
                storage,
                reporter,
                &result.chunk.digits,
                &scores,
                &target,
                result.chunk.start_offset,
                result.chunk.start_scanned,
            )?;
            session
                .telemetry
                .record_persistence(persistence_started.elapsed());
            if perfect_found {
                stop.store(true, Ordering::SeqCst);
                drop(result);
                return Ok(FinishReason::PerfectFound);
            }

            let reduction_started = Instant::now();
            merge_chunk_top_matches(
                &mut session.run.top_matches,
                &mut scores,
                &result.chunk.digits,
                result.chunk.start_offset,
                result.chunk.start_scanned,
                session.run.width as usize,
                session.run.height as usize,
                session.canvas_width,
                session.canvas_height,
                session.options.match_mode,
                session.options.threshold,
                session.options.top_n,
            )?;
            session
                .telemetry
                .record_reduction(reduction_started.elapsed());
            let actual_windows = u64::try_from(result.chunk.actual_windows)?;
            next_offset = next_offset.saturating_add(actual_windows);
            completed_windows = completed_windows.saturating_add(actual_windows);
            session.run.current_offset = next_offset;
            session.run.scanned_windows = result.chunk.start_scanned.saturating_add(actual_windows);
            session.record_progress();
            session.touch_runtime();
            session.checkpoint_if_due(storage)?;
            if request_bound_reached(session) {
                session.runtime.throttling_active = false;
            } else {
                let throttle_started = Instant::now();
                session.runtime.throttling_active = session
                    .options
                    .performance
                    .throttle_after_batch(result.processing);
                if session.runtime.throttling_active {
                    session
                        .telemetry
                        .record_throttle_wait(throttle_started.elapsed());
                }
            }
            session.runtime.reducer_contiguous_completed_offsets = completed_windows;
            session.report(reporter)?;
            drop(result);
        }
    }

    session.runtime.reducer_contiguous_completed_offsets = completed_windows;
    if stop.load(Ordering::SeqCst) {
        return Ok(FinishReason::Interrupted);
    }
    if max_offset_reached(session) {
        return Ok(FinishReason::MaxOffsetReached);
    }
    if count_bound_reached(session) {
        return Ok(FinishReason::LimitReached);
    }
    let snapshot = budget.snapshot();
    if snapshot.queue_current != 0 || snapshot.transient_memory_reserved_bytes() != 0 {
        bail!("resource budget did not drain before reducer completion");
    }
    Ok(FinishReason::SourceExhausted)
}

fn request_bound_reached(session: &SearchSession<'_>) -> bool {
    max_offset_reached(session) || count_bound_reached(session)
}

fn max_offset_reached(session: &SearchSession<'_>) -> bool {
    session
        .options
        .max_offset
        .is_some_and(|max_offset| session.run.current_offset >= max_offset)
}

fn count_bound_reached(session: &SearchSession<'_>) -> bool {
    SearchOptions::intersect_count_bounds(session.options.work_windows, session.options.limit)
        .is_some_and(|limit| session.scanned_this_invocation() >= limit)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc::{self, Sender};

    use anyhow::Result;
    use tempfile::tempdir;

    use crate::art::Bitmap;
    use crate::digits::{DigitSource, GenerationState};
    use crate::performance::{
        GeneratorBackendChoice, GpuMode, PerformanceOverrides, PerformanceProfile,
        PerformanceSettings, SearchBackendChoice, ThermalMode,
    };
    use crate::search::session::SearchSession;
    use crate::search::types::{MatchMode, SearchOptions, SearchReporter, SearchSnapshot};
    use crate::storage::{BestEventRecord, NewRun, Storage};

    struct GrowingSource;

    impl DigitSource for GrowingSource {
        fn kind(&self) -> &'static str {
            "test-growing"
        }

        fn len(&self) -> Result<u64> {
            Ok(0)
        }

        fn validate(&self) -> Result<()> {
            Ok(())
        }

        fn read_range(&self, _offset: u64, _len: usize) -> Result<Vec<u8>> {
            Ok(Vec::new())
        }

        fn is_growing(&self) -> bool {
            true
        }

        fn generation(&self) -> Option<GenerationState> {
            Some(GenerationState {
                active: true,
                target_digits: 1,
            })
        }

        fn generation_metrics(&self) -> Option<crate::pi::GenerationMetrics> {
            Some(crate::pi::GenerationMetrics {
                generated_source_digits: 100,
                generator_wait: Duration::from_secs(1),
                ..crate::pi::GenerationMetrics::default()
            })
        }
    }

    struct UpdateReporter {
        updates: Sender<SearchSnapshot>,
    }

    impl SearchReporter for UpdateReporter {
        fn on_update(&mut self, snapshot: &SearchSnapshot) -> Result<()> {
            self.updates.send(snapshot.clone()).map_err(Into::into)
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

    fn options() -> SearchOptions {
        SearchOptions {
            max_offset: None,
            work_windows: None,
            limit: None,
            match_mode: MatchMode::Threshold,
            canvas_width: 1,
            canvas_height: 1,
            threshold: 5,
            invert: false,
            workers: Some(1),
            checkpoint_every: Duration::from_secs(60),
            top_n: 1,
            keep_going_after_perfect: true,
            chunk_windows: 1,
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
                    chunk_size: Some(1),
                    checkpoint_every_secs: Some(60),
                    ..PerformanceOverrides::default()
                },
            ),
        }
    }

    #[test]
    fn reports_while_waiting_for_growing_source() -> Result<()> {
        let directory = tempdir()?;
        let mut storage = Storage::open_path(directory.path().join("state.db"))?;
        let target = Bitmap::new(1, 1, vec![1])?;
        let run = storage.create_run(NewRun {
            name: "growing-wait".to_string(),
            source: crate::digits::DigitSourceSpec::demo(),
            template_name: None,
            art_hash: target.sha256(),
            width: 1,
            height: 1,
            canvas_width: 1,
            canvas_height: 1,
            match_mode: MatchMode::Threshold,
            threshold: 5,
            invert_enabled: false,
            start_offset: Some(0),
            target_bitmap: target,
            generated_digit_count: 0,
            params_json: "{}".to_string(),
        })?;
        let source = GrowingSource;
        let budget = ResourceBudget::new(1, 64, 1)?;
        let mut session = SearchSession::new_with_budget(
            &storage,
            run,
            &source,
            options(),
            Some(Arc::clone(&budget)),
        )?;
        let (_result_tx, result_rx) = mpsc::sync_channel(1);
        let (update_tx, update_rx) = mpsc::channel();
        let stop = Arc::new(AtomicBool::new(false));

        let (updated, outcome) = thread::scope(|scope| {
            let reduce_stop = Arc::clone(&stop);
            let handle = scope.spawn(move || {
                let mut reporter = UpdateReporter { updates: update_tx };
                reduce(
                    &mut session,
                    &mut storage,
                    &mut reporter,
                    result_rx,
                    budget,
                    reduce_stop,
                    None,
                )
            });
            let snapshot = update_rx.recv_timeout(Duration::from_millis(250)).ok();
            stop.store(true, Ordering::SeqCst);
            let outcome = handle.join().expect("reducer thread joins");
            (snapshot, outcome)
        });

        let snapshot = updated.expect("growing-source wait must publish a live snapshot");
        assert!(snapshot.waiting_for_digits);
        let generation = snapshot.generation.expect("generation state");
        assert!(generation.active);
        assert_eq!(generation.digits_per_sec, 100.0);
        assert_eq!(outcome?, FinishReason::Interrupted);
        Ok(())
    }
}
