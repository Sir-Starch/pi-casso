use std::sync::{Arc, Barrier, atomic::AtomicBool};
use std::thread;
use std::time::Instant;

use anyhow::{Result, anyhow};
use num_traits::ToPrimitive;

use crate::cli::{BenchmarkArgs, BenchmarkCacheState, BenchmarkSourceMode, PiBenchmarkArgs};
use crate::performance::{GpuMode, PerformanceProfile, SearchBackendChoice};
use crate::pi::{
    CachedGrowingPiSource, GENERATOR_FIXED_BYTES, GenerationBudget, GenerationDemand,
    GenerationMetrics, GeneratorSelection, PiCache,
};
use crate::search::ResourceBudget;

use super::report::{PiBenchmarkRun, PiMemory};

const DEMAND_WINDOW_LEN: usize = 4096;
const MEMORY_LIMIT_MB: usize = 512;

pub(crate) fn measured_run(
    args: &PiBenchmarkArgs,
    selection: &GeneratorSelection,
    repetition: u32,
) -> Result<PiBenchmarkRun> {
    match args.demand_mode {
        crate::cli::PiDemandMode::Serial => serial(args, selection, repetition),
        crate::cli::PiDemandMode::Concurrent => concurrent(args, selection, repetition),
        crate::cli::PiDemandMode::SearchOverlap => search_overlap(args),
    }
}

fn serial(
    args: &PiBenchmarkArgs,
    selection: &GeneratorSelection,
    repetition: u32,
) -> Result<PiBenchmarkRun> {
    let cache = fresh_cache("serial", repetition)?;
    let source = CachedGrowingPiSource::from_selection(cache.clone(), selection)?;
    let budget = ResourceBudget::new(1, MEMORY_LIMIT_MB, args.workers)?;
    let started = Instant::now();
    let mut metrics = GenerationMetrics::default();
    for target in &args.targets {
        metrics = source.request_generation(
            demand(*target, args.workers),
            Arc::clone(&budget) as Arc<dyn GenerationBudget>,
            Arc::new(AtomicBool::new(false)),
        )?;
    }
    let elapsed = started.elapsed().as_secs_f64().max(f64::EPSILON);
    let correctness = prefix_is_correct(&cache)?;
    source.shutdown()?;
    Ok(from_generation(
        metrics,
        elapsed,
        correctness,
        budget.snapshot(),
        0,
    ))
}

fn concurrent(
    args: &PiBenchmarkArgs,
    selection: &GeneratorSelection,
    repetition: u32,
) -> Result<PiBenchmarkRun> {
    let cache = fresh_cache("concurrent", repetition)?;
    let source = Arc::new(CachedGrowingPiSource::paused_from_selection(
        cache.clone(),
        selection,
    )?);
    let budget = ResourceBudget::new(1, MEMORY_LIMIT_MB, args.workers)?;
    let barrier = Arc::new(Barrier::new(args.targets.len().saturating_add(1)));
    let started = Instant::now();
    let mut handles = Vec::with_capacity(args.targets.len());
    for target in &args.targets {
        let source = Arc::clone(&source);
        let budget = Arc::clone(&budget);
        let barrier = Arc::clone(&barrier);
        let demand = demand(*target, args.workers);
        handles.push(thread::spawn(move || {
            barrier.wait();
            source.request_generation(
                demand,
                budget as Arc<dyn GenerationBudget>,
                Arc::new(AtomicBool::new(false)),
            )
        }));
    }
    barrier.wait();
    source.wait_for_pending_requests(args.targets.len())?;
    source.resume_producer();
    let mut metrics = GenerationMetrics::default();
    for handle in handles {
        metrics = handle
            .join()
            .map_err(|_| anyhow!("pi_benchmark_worker_panicked"))??;
    }
    let elapsed = started.elapsed().as_secs_f64().max(f64::EPSILON);
    let correctness = prefix_is_correct(&cache)?;
    source.shutdown()?;
    Ok(from_generation(
        metrics,
        elapsed,
        correctness,
        budget.snapshot(),
        0,
    ))
}

fn search_overlap(args: &PiBenchmarkArgs) -> Result<PiBenchmarkRun> {
    let outcome = crate::benchmark_runner::run(search_args(args))?;
    if outcome.exit_code != 0 {
        return Err(anyhow!(outcome.report.reason));
    }
    let report = outcome.report;
    let generated = report
        .high_water_digits
        .max(args.targets.iter().copied().max().unwrap_or_default());
    let generation_wait_ms = report.stage_timings.generation_wait_ms;
    let rate = if report.generator_digits_per_second > 0.0 {
        report.generator_digits_per_second
    } else {
        generated.to_f64().unwrap_or(0.0) / report.elapsed_seconds.max(f64::EPSILON)
    };
    Ok(PiBenchmarkRun {
        generated_source_digits: generated,
        generated_source_digits_per_second: rate,
        scanned_windows_per_second: report.scanned_windows_per_second,
        generation_wait_ms,
        overlap_wait_ms: report.overlap_wait_ms,
        cache_write_ms: report.cache_write_ms,
        producer_epochs: report.producer_epochs,
        coalesced_request_count: report.coalesced_request_count,
        generation_batches: report.generation_batches,
        chudnovsky_target_computations: 0,
        recomputed_source_digits: 0,
        skipped_source_digits: 0,
        concurrent_requests: 0,
        search_work_windows: args.search_work_windows,
        correctness: report.scanned_windows == args.search_work_windows,
        memory: PiMemory {
            logical_peak_mb: report.memory.logical_peak_mb,
            rss_peak_mb: report.memory.rss_peak_mb,
            rss_baseline_mb: report.memory.rss_baseline_mb,
            rss_margin_mb: report.memory.rss_margin_mb,
            gpu_vram_status: report.memory.gpu_vram_status,
            gpu_vram_baseline_mb: report.memory.gpu_vram_baseline_mb,
            gpu_vram_margin_mb: report.memory.gpu_vram_margin_mb,
            gpu_vram_peak_mb: report.memory.gpu_vram_peak_mb,
        },
    })
}

fn from_generation(
    metrics: GenerationMetrics,
    elapsed: f64,
    correctness: bool,
    budget: crate::search::ResourceBudgetSnapshot,
    search_work_windows: u64,
) -> PiBenchmarkRun {
    PiBenchmarkRun {
        generated_source_digits: metrics.generated_source_digits,
        generated_source_digits_per_second: metrics.generated_source_digits.to_f64().unwrap_or(0.0)
            / elapsed,
        scanned_windows_per_second: 0.0,
        generation_wait_ms: duration_ms(metrics.generator_wait),
        overlap_wait_ms: 0,
        cache_write_ms: duration_ms(metrics.cache_write),
        producer_epochs: metrics.producer_epochs,
        coalesced_request_count: metrics.coalesced_request_count,
        generation_batches: metrics.generation_batches,
        chudnovsky_target_computations: metrics.chudnovsky_target_computations,
        recomputed_source_digits: metrics.recomputed_source_digits,
        skipped_source_digits: metrics.skipped_source_digits,
        concurrent_requests: metrics.concurrent_requests,
        search_work_windows,
        correctness,
        memory: PiMemory {
            logical_peak_mb: budget.memory_peak_bytes.to_f64().unwrap_or(0.0) / 1_048_576.0,
            rss_peak_mb: budget.rss_peak_mb,
            rss_baseline_mb: budget.rss_baseline_mb,
            rss_margin_mb: budget.rss_margin_mb,
            gpu_vram_status: "unavailable".to_string(),
            gpu_vram_baseline_mb: 0.0,
            gpu_vram_margin_mb: 0.0,
            gpu_vram_peak_mb: 0.0,
        },
    }
}

fn demand(absolute_target: u64, workers: usize) -> GenerationDemand {
    GenerationDemand {
        absolute_target,
        window_len: DEMAND_WINDOW_LEN,
        generator_fixed_bytes: GENERATOR_FIXED_BYTES,
        cpu_workers: workers,
    }
}

fn fresh_cache(mode: &str, repetition: u32) -> Result<PiCache> {
    let path = crate::storage::app_data_dir()?
        .join("benchmark-cache")
        .join(format!("pi-{mode}-{repetition}-{}", uuid::Uuid::new_v4()))
        .join("pi-cache.txt");
    let cache = PiCache::new(path);
    cache.validate_reset_lock()?;
    Ok(cache)
}

fn prefix_is_correct(cache: &PiCache) -> Result<bool> {
    const PREFIX: &[u8] = &[3, 1, 4, 1, 5, 9, 2, 6, 5, 3, 5, 8, 9, 7, 9, 3];
    let available = usize::try_from(cache.digit_count()?)?.min(PREFIX.len());
    Ok(available > 0 && cache.read_range(0, available)? == PREFIX[..available])
}

fn search_args(args: &PiBenchmarkArgs) -> BenchmarkArgs {
    BenchmarkArgs {
        template: Some("arch".to_string()),
        seconds: 10,
        work_windows: Some(args.search_work_windows),
        start_offset: 0,
        max_offset: None,
        source_mode: BenchmarkSourceMode::Growing,
        cache_state: BenchmarkCacheState::Cold,
        repetitions: 1,
        warmup: 0,
        profile: PerformanceProfile::Performance,
        backend: Some(SearchBackendChoice::Cpu),
        generator_backend: args.generator_backend,
        y_cruncher_path: args.y_cruncher_path.clone(),
        gpu: Some(GpuMode::Off),
        gpu_device: None,
        cpu_utilization: Some(100),
        gpu_utilization: None,
        cpu_workers: Some(args.workers),
        chunk_size: Some(4096),
        queue_depth: Some(1),
        memory_limit_mb: Some(MEMORY_LIMIT_MB),
        show_metrics: true,
    }
}

fn duration_ms(duration: std::time::Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}
