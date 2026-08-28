use std::sync::{Arc, atomic::AtomicBool};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use num_traits::ToPrimitive;

use crate::art::Bitmap;
use crate::benchmark_contract::BenchmarkBounds;
use crate::benchmark_report::{BenchmarkReport, RepetitionReport};
use crate::benchmark_stats::{aggregate, aggregate_stage, aggregate_waits};
use crate::cli::{BenchmarkArgs, BenchmarkSourceMode};
use crate::digits::{DigitSource, DigitSourceSpec};
use crate::performance::PerformanceSettings;
use crate::pi::PiCache;
use crate::search::{
    FinishReason, MatchMode, ResourceBudget, SearchOptions, run_search_with_budget,
};
use crate::storage::{NewRun, Storage};

mod telemetry;

use telemetry::FinishReporter;

pub fn execute_repetition_with_budget(
    args: &BenchmarkArgs,
    settings: &PerformanceSettings,
    bounds: &BenchmarkBounds,
    target: &Bitmap,
    cache: PiCache,
    repetition: u32,
    budget: Arc<ResourceBudget>,
) -> Result<RepetitionReport> {
    let required_digits = bounds.source_end_exclusive;
    let first_published_digits = cache.info()?.published_digits;
    let cache_instance_id = cache
        .path()
        .parent()
        .and_then(|path| path.file_name())
        .and_then(|name| name.to_str())
        .context("benchmark cache instance path is not UTF-8")?
        .to_string();
    let selection =
        crate::pi::resolve_generator(args.generator_backend, args.y_cruncher_path.as_deref())?;
    let source_impl = crate::pi::CachedGrowingPiSource::from_selection(cache.clone(), &selection)?;
    if matches!(args.source_mode, BenchmarkSourceMode::Finite) {
        source_impl.request_generation(
            crate::pi::GenerationDemand {
                absolute_target: required_digits,
                window_len: 24 * 24,
                generator_fixed_bytes: crate::pi::GENERATOR_FIXED_BYTES,
                cpu_workers: settings.limits.cpu_workers,
            },
            Arc::clone(&budget) as Arc<dyn crate::pi::GenerationBudget>,
            Arc::new(AtomicBool::new(false)),
        )?;
    }
    let started = Instant::now();
    let generation = source_impl.metrics()?.generator_wait;
    let source = DigitSourceSpec::cache(cache.path().clone());
    let db_path = cache.path().with_file_name(format!("run-{repetition}.db"));
    let mut storage = Storage::open_path(db_path)?;
    let run = storage.create_run(NewRun {
        name: format!("benchmark-{repetition}"),
        source,
        template_name: Some(args.template.clone().unwrap_or_else(|| "arch".to_string())),
        art_hash: target.sha256(),
        width: u32::try_from(target.width)?,
        height: u32::try_from(target.height)?,
        canvas_width: 24,
        canvas_height: 24,
        match_mode: MatchMode::Emergence,
        threshold: 5,
        invert_enabled: false,
        start_offset: Some(args.start_offset),
        target_bitmap: target.clone(),
        generated_digit_count: cache.digit_count()?,
        params_json: "{}".to_string(),
    })?;
    let options = SearchOptions {
        max_offset: bounds.max_offset,
        work_windows: Some(bounds.work_windows),
        limit: Some(bounds.scanned_windows(args.start_offset)),
        match_mode: MatchMode::Emergence,
        canvas_width: 24,
        canvas_height: 24,
        threshold: 5,
        invert: false,
        workers: Some(settings.limits.cpu_workers),
        checkpoint_every: Duration::from_secs(settings.limits.checkpoint_every_secs),
        top_n: 3,
        keep_going_after_perfect: true,
        chunk_windows: settings.limits.chunk_size,
        performance: settings.clone(),
    };
    let mut reporter = FinishReporter::default();
    let final_run = run_search_with_budget(
        &mut storage,
        run,
        &source_impl,
        options,
        &mut reporter,
        budget,
    )?;
    let elapsed = started.elapsed().as_secs_f64().max(f64::EPSILON);
    let scanned = final_run.scanned_windows;
    let source_digits = scanned.saturating_add(u64::from(scanned > 0) * 575);
    let logical_digits = scanned.saturating_mul(576);
    let scanned_rate = scanned
        .to_f64()
        .context("scanned window count cannot be represented as f64")?
        / elapsed;
    let source_rate = source_digits
        .to_f64()
        .context("source digit count cannot be represented as f64")?
        / elapsed;
    let logical_rate = logical_digits
        .to_f64()
        .context("logical digit count cannot be represented as f64")?
        / elapsed;
    let final_telemetry = reporter.telemetry.unwrap_or_default();
    let generation_metrics = source_impl.generation_metrics().unwrap_or_default();
    let generation_ms = duration_ms(generation);
    let mut stage_timings = final_telemetry.stage_timings;
    stage_timings.generation_wait_ms = stage_timings
        .generation_wait_ms
        .saturating_add(generation_ms);
    let mut waits = final_telemetry.waits;
    waits.generator_ms = waits.generator_ms.saturating_add(generation_ms);
    Ok(RepetitionReport {
        schema_version: 1,
        status: "ok".to_string(),
        repetition,
        cache_instance_id,
        cache_reset: matches!(args.cache_state, crate::cli::BenchmarkCacheState::Cold),
        warm_up_completed: matches!(args.cache_state, crate::cli::BenchmarkCacheState::Warm)
            && args.warmup > 0,
        first_published_digits,
        scanned_windows: scanned,
        source_digits_read: source_digits,
        logical_window_digits: logical_digits,
        scanned_windows_per_second: scanned_rate,
        source_digits_per_second: source_rate,
        logical_window_digits_per_second: logical_rate,
        elapsed_seconds: elapsed,
        stop_reason: stop_reason(args, bounds, reporter.reason),
        resolved_backend: final_telemetry.resolved_backend,
        backend_device: final_telemetry.backend_device,
        backend_feature_available: final_telemetry.backend_feature_available,
        backend_fault_status: final_telemetry.backend_fault_status,
        fallback: final_telemetry.fallback,
        fallback_reason: final_telemetry.fallback_reason,
        fallback_count: final_telemetry.fallback_count,
        stage_timings,
        waits,
        source: final_telemetry.source,
        overlap_wait_ms: 0,
        cache_write_ms: duration_ms(generation_metrics.cache_write),
        producer_epochs: generation_metrics.producer_epochs,
        coalesced_request_count: generation_metrics.coalesced_request_count,
        generation_batches: generation_metrics.generation_batches,
        event_wake_latency_ms: duration_ms(generation_metrics.event_wake_latency),
        lead_digits: generation_metrics.lead_digits,
        high_water_digits: generation_metrics.high_water_digits,
        source_lag_digits: final_telemetry.source_lag_digits,
        generator_digits_per_second: final_telemetry.generator_digits_per_second,
        gpu_submissions: final_telemetry.gpu_submissions,
        gpu_completions: final_telemetry.gpu_completions,
        gpu_buffer_creations: final_telemetry.gpu_buffer_creations,
        gpu_bind_group_creations: final_telemetry.gpu_bind_group_creations,
        gpu_resource_reuses: final_telemetry.gpu_resource_reuses,
        gpu_overlap_ms: final_telemetry.gpu_overlap_ms,
        gpu_max_in_flight: final_telemetry.gpu_max_in_flight,
        gpu_overlap_events: final_telemetry.gpu_overlap_events,
        gpu_test_only_mock: final_telemetry.gpu_test_only_mock,
        gpu_duty_wait_ms: final_telemetry.gpu_duty_wait_ms,
        gpu_initial_submission_wait_ms: final_telemetry.gpu_initial_submission_wait_ms,
        active_submission_ratio: final_telemetry.active_submission_ratio,
        dispatch_quantum_ratio: final_telemetry.dispatch_quantum_ratio,
        telemetry_enabled: final_telemetry.telemetry_enabled,
        best_score: final_run.best_score,
        queue: final_telemetry.queue,
        memory: final_telemetry.memory,
        reducer: final_telemetry.reducer,
        cpu_permits_in_use: final_telemetry.cpu_permits_in_use,
        cpu_permits_peak: final_telemetry.cpu_permits_peak,
        cpu_permits_max: final_telemetry.cpu_permits_max,
    })
}

pub fn apply_runs(report: &mut BenchmarkReport, runs: Vec<RepetitionReport>, warmed: bool) {
    let (median, p95) = aggregate(&runs);
    report.status = "ok".to_string();
    report.warm_up_completed = warmed;
    report.scanned_windows = runs.first().map_or(0, |run| run.scanned_windows);
    report.scanned_windows_per_second = median.scanned_windows_per_second;
    report.source_digits_per_second = median.source_digits_per_second;
    report.logical_window_digits_per_second = median.logical_window_digits_per_second;
    report.elapsed_seconds = median.elapsed_seconds;
    report.stop_reason = runs
        .first()
        .map_or_else(String::new, |run| run.stop_reason.clone());
    report.best_score = runs.iter().map(|run| run.best_score).fold(0.0, f64::max);
    report.stage_timings = aggregate_stage(&runs);
    report.waits = aggregate_waits(&runs);
    report.overlap_wait_ms = median.overlap_wait_ms;
    report.cache_write_ms = median.cache_write_ms;
    report.producer_epochs = runs
        .iter()
        .map(|run| run.producer_epochs)
        .max()
        .unwrap_or(0);
    report.coalesced_request_count = runs
        .iter()
        .map(|run| run.coalesced_request_count)
        .max()
        .unwrap_or(0);
    report.generation_batches = runs
        .iter()
        .map(|run| run.generation_batches)
        .max()
        .unwrap_or(0);
    report.event_wake_latency_ms = runs
        .iter()
        .map(|run| run.event_wake_latency_ms)
        .max()
        .unwrap_or(0);
    report.lead_digits = runs.iter().map(|run| run.lead_digits).max().unwrap_or(0);
    report.high_water_digits = runs
        .iter()
        .map(|run| run.high_water_digits)
        .max()
        .unwrap_or(0);
    if let Some(run) = runs.first() {
        report.resolved_backend = Some(run.resolved_backend.clone());
        report.backend_device = run.backend_device.clone();
        report.backend_feature_available = run.backend_feature_available;
        report.source_lag_digits = run.source_lag_digits;
        report.generator_digits_per_second = run.generator_digits_per_second;
        report.telemetry_enabled = run.telemetry_enabled;
    }
    let runtime_fallback = runs.iter().find(|run| run.fallback);
    report.fallback_count = runs.iter().map(|run| run.fallback_count).max().unwrap_or(0);
    if let Some(run) = runtime_fallback {
        report.fallback = true;
        report.fallback_reason = run.fallback_reason.clone();
        report.backend_fault_status = run.backend_fault_status.clone();
    }
    report.gpu.submissions = runs
        .iter()
        .map(|run| run.gpu_submissions)
        .max()
        .unwrap_or(0);
    report.gpu.completions = runs
        .iter()
        .map(|run| run.gpu_completions)
        .max()
        .unwrap_or(0);
    report.gpu.buffer_creations = runs
        .iter()
        .map(|run| run.gpu_buffer_creations)
        .max()
        .unwrap_or(0);
    report.gpu.bind_group_creations = runs
        .iter()
        .map(|run| run.gpu_bind_group_creations)
        .max()
        .unwrap_or(0);
    report.gpu.resource_reuses = runs
        .iter()
        .map(|run| run.gpu_resource_reuses)
        .max()
        .unwrap_or(0);
    report.gpu.overlap_ms = runs.iter().map(|run| run.gpu_overlap_ms).max().unwrap_or(0);
    report.gpu.max_in_flight = runs
        .iter()
        .map(|run| run.gpu_max_in_flight)
        .max()
        .unwrap_or(0);
    report.gpu.overlap_events = runs
        .iter()
        .map(|run| run.gpu_overlap_events)
        .max()
        .unwrap_or(0);
    report.gpu.test_only_mock = runs.iter().any(|run| run.gpu_test_only_mock);
    report.test_only_mock = report.gpu.test_only_mock;
    if let Some(run) = runs.first() {
        report.gpu_duty_wait_ms = run.gpu_duty_wait_ms;
        report.gpu_initial_submission_wait_ms = run.gpu_initial_submission_wait_ms;
        report.active_submission_ratio = run.active_submission_ratio;
        report.dispatch_quantum_ratio = run.dispatch_quantum_ratio;
    }
    report.gpu.fallback_count = report.fallback_count;
    report.cache_instance_id = if matches!(report.cache_state.as_str(), "warm") {
        runs.first()
            .map_or_else(String::new, |run| run.cache_instance_id.clone())
    } else {
        String::new()
    };
    report.source = runs
        .first()
        .map_or_else(Default::default, |run| run.source.clone());
    if let Some(run) = runs.first() {
        report.queue = run.queue.clone();
        report.memory = run.memory.clone();
        report.reducer = run.reducer.clone();
        report.cpu_permits_in_use = run.cpu_permits_in_use;
        report.cpu_permits_peak = run.cpu_permits_peak;
        report.cpu_permits_max = run.cpu_permits_max;
    }
    for run in &runs {
        report.queue.current_occupancy = report
            .queue
            .current_occupancy
            .max(run.queue.current_occupancy);
        report.queue.max_occupancy = report.queue.max_occupancy.max(run.queue.max_occupancy);
        report.queue.global_limit = report.queue.global_limit.max(run.queue.global_limit);
        report.memory.logical_reserved_mb = report
            .memory
            .logical_reserved_mb
            .max(run.memory.logical_reserved_mb);
        report.memory.logical_peak_mb = report
            .memory
            .logical_peak_mb
            .max(run.memory.logical_peak_mb);
        report.memory.logical_budget_mb = report
            .memory
            .logical_budget_mb
            .max(run.memory.logical_budget_mb);
        report.memory.logical_reserved_bytes = report
            .memory
            .logical_reserved_bytes
            .max(run.memory.logical_reserved_bytes);
        report.memory.logical_peak_bytes = report
            .memory
            .logical_peak_bytes
            .max(run.memory.logical_peak_bytes);
        report.memory.logical_budget_bytes = report
            .memory
            .logical_budget_bytes
            .max(run.memory.logical_budget_bytes);
        report.memory.rss_peak_mb = report.memory.rss_peak_mb.max(run.memory.rss_peak_mb);
        report.memory.rss_margin_mb = report.memory.rss_margin_mb.max(run.memory.rss_margin_mb);
        report.reducer.contiguous_completed_offsets = report
            .reducer
            .contiguous_completed_offsets
            .max(run.reducer.contiguous_completed_offsets);
        report.reducer.max_reorder_depth = report
            .reducer
            .max_reorder_depth
            .max(run.reducer.max_reorder_depth);
        report.reducer.ordered &= run.reducer.ordered;
        report.cpu_permits_in_use = report.cpu_permits_in_use.max(run.cpu_permits_in_use);
        report.cpu_permits_peak = report.cpu_permits_peak.max(run.cpu_permits_peak);
        report.cpu_permits_max = report.cpu_permits_max.max(run.cpu_permits_max);
    }
    report.median = median;
    report.p95 = p95;
    report.raw_runs = runs;
}

fn stop_reason(
    args: &BenchmarkArgs,
    bounds: &BenchmarkBounds,
    reason: Option<FinishReason>,
) -> String {
    if bounds.scanned_windows(args.start_offset) == 0 {
        return "empty_range".to_string();
    }
    if bounds.max_offset == Some(bounds.effective_end)
        && bounds.effective_end < args.start_offset.saturating_add(bounds.work_windows)
    {
        return "max_offset".to_string();
    }
    match reason {
        Some(FinishReason::SourceExhausted) => "source_exhausted",
        Some(FinishReason::Interrupted) => "interrupted",
        Some(FinishReason::PerfectFound) => "perfect_match",
        Some(FinishReason::LimitReached | FinishReason::MaxOffsetReached) | None => "work_windows",
    }
    .to_string()
}

fn duration_ms(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}
