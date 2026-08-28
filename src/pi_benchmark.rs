use anyhow::{Result, bail};

use crate::benchmark_contract::WorkloadIdentity;
use crate::cli::{PiBenchmarkArgs, PiDemandMode};
use crate::pi::{GeneratorSelection, resolve_generator};

mod report;
mod run;

pub(crate) use report::PiBenchmarkReport;
use report::{PiBenchmarkRun, PiMemory, PiMetrics, nearest_rank_f64, nearest_rank_u64};

pub(crate) struct PiBenchmarkOutcome {
    pub report: PiBenchmarkReport,
    pub exit_code: i32,
}

pub(crate) fn run(args: PiBenchmarkArgs) -> Result<PiBenchmarkOutcome> {
    validate_args(&args)?;
    let mut selection = resolve_generator(args.generator_backend, args.y_cruncher_path.as_deref())?;
    if !selection.is_available() {
        let identity = workload_identity(&args, &selection)?;
        let workload_id = identity.pi_workload_id()?;
        return Ok(PiBenchmarkOutcome {
            report: build_report(&args, &selection, identity, workload_id, Vec::new()),
            exit_code: 2,
        });
    }
    let runs = match execute_runs(&args, &selection) {
        Ok(runs) => runs,
        Err(error)
            if matches!(
                args.generator_backend,
                crate::performance::GeneratorBackendChoice::Auto
            ) =>
        {
            selection = selection.fallback_after_failure(&error.to_string())?;
            execute_runs(&args, &selection)?
        }
        Err(error) => {
            selection = selection.unavailable_after_failure(&error.to_string());
            let identity = workload_identity(&args, &selection)?;
            let workload_id = identity.pi_workload_id()?;
            return Ok(PiBenchmarkOutcome {
                report: build_report(&args, &selection, identity, workload_id, Vec::new()),
                exit_code: 2,
            });
        }
    };
    let identity = workload_identity(&args, &selection)?;
    let workload_id = identity.pi_workload_id()?;
    Ok(PiBenchmarkOutcome {
        report: build_report(&args, &selection, identity, workload_id, runs),
        exit_code: 0,
    })
}

fn execute_runs(
    args: &PiBenchmarkArgs,
    selection: &GeneratorSelection,
) -> Result<Vec<PiBenchmarkRun>> {
    for warmup in 0..args.warmup {
        let _ = run::measured_run(args, selection, u32::MAX - warmup)?;
    }
    let mut runs = Vec::with_capacity(usize::try_from(args.repetitions)?);
    for repetition in 0..args.repetitions {
        runs.push(run::measured_run(args, selection, repetition)?);
    }
    Ok(runs)
}

fn validate_args(args: &PiBenchmarkArgs) -> Result<()> {
    if args.targets.is_empty() || args.targets.contains(&0) {
        bail!("targets_must_be_positive");
    }
    if args.workers == 0 {
        bail!("workers_must_be_positive");
    }
    if args.targets.windows(2).any(|pair| pair[0] >= pair[1]) {
        bail!("targets_must_be_strictly_increasing");
    }
    if matches!(args.demand_mode, PiDemandMode::Concurrent) && args.targets.len() != 4 {
        bail!("concurrent_requires_four_targets");
    }
    if matches!(args.demand_mode, PiDemandMode::SearchOverlap) && args.search_work_windows == 0 {
        bail!("search_work_windows_must_be_positive");
    }
    Ok(())
}

fn build_report(
    args: &PiBenchmarkArgs,
    selection: &GeneratorSelection,
    identity: WorkloadIdentity,
    workload_id: String,
    runs: Vec<PiBenchmarkRun>,
) -> PiBenchmarkReport {
    let available = selection.is_available();
    let median = aggregate(&runs, 50);
    let p95 = aggregate(&runs, 95);
    let representative = runs
        .iter()
        .max_by_key(|run| run.generation_wait_ms)
        .cloned()
        .unwrap_or_default();
    let memory = aggregate_memory(&runs);
    PiBenchmarkReport {
        schema_version: 1,
        status: if available { "ok" } else { "unavailable" }.to_string(),
        reason: selection.reason.clone(),
        targets: args.targets.clone(),
        demand_mode: args.demand_mode.as_str().to_string(),
        concurrent_requests: representative.concurrent_requests,
        search_work_windows: if matches!(args.demand_mode, PiDemandMode::SearchOverlap) {
            args.search_work_windows
        } else {
            0
        },
        generation_wait_ms: median.generation_wait_ms,
        overlap_wait_ms: nearest_rank_or_zero(&runs, |run| run.overlap_wait_ms),
        cache_write_ms: nearest_rank_or_zero(&runs, |run| run.cache_write_ms),
        producer_epochs: representative.producer_epochs,
        coalesced_request_count: representative.coalesced_request_count,
        generation_batches: representative.generation_batches,
        chudnovsky_target_computations: representative.chudnovsky_target_computations,
        recomputed_source_digits: representative.recomputed_source_digits,
        skipped_source_digits: representative.skipped_source_digits,
        cpu_workers: args.workers,
        correctness: available && runs.iter().all(|run| run.correctness),
        median,
        p95,
        memory,
        requested_generator_backend: selection.requested_backend.to_string(),
        selected_backend: selection.selected_backend.to_string(),
        selected_variant: selection
            .selected_variant
            .map_or_else(String::new, |variant| variant.as_str().to_string()),
        generator_executable_sha256: selection.executable_sha256.clone(),
        fallback: selection.fallback,
        fallback_reason: selection.fallback_reason.clone(),
        unavailable_backends: selection.unavailable_backends.clone(),
        workload_id,
        workload_identity: identity,
        raw_runs: runs,
    }
}

fn aggregate(runs: &[PiBenchmarkRun], percentile: usize) -> PiMetrics {
    if runs.is_empty() {
        return PiMetrics::default();
    }
    PiMetrics {
        generated_source_digits_per_second: nearest_rank_f64(runs, percentile, |run| {
            run.generated_source_digits_per_second
        }),
        generation_wait_ms: nearest_rank_u64(runs, percentile, |run| run.generation_wait_ms),
        scanned_windows_per_second: nearest_rank_f64(runs, percentile, |run| {
            run.scanned_windows_per_second
        }),
    }
}

fn nearest_rank_or_zero(runs: &[PiBenchmarkRun], field: impl Fn(&PiBenchmarkRun) -> u64) -> u64 {
    if runs.is_empty() {
        0
    } else {
        nearest_rank_u64(runs, 50, field)
    }
}

fn aggregate_memory(runs: &[PiBenchmarkRun]) -> PiMemory {
    runs.iter().fold(
        PiMemory {
            gpu_vram_status: "unavailable".to_string(),
            ..PiMemory::default()
        },
        |mut peak, run| {
            peak.logical_peak_mb = peak.logical_peak_mb.max(run.memory.logical_peak_mb);
            peak.rss_peak_mb = peak.rss_peak_mb.max(run.memory.rss_peak_mb);
            peak.rss_baseline_mb = peak.rss_baseline_mb.max(run.memory.rss_baseline_mb);
            peak.rss_margin_mb = peak.rss_margin_mb.max(run.memory.rss_margin_mb);
            peak
        },
    )
}

fn workload_identity(
    args: &PiBenchmarkArgs,
    selection: &GeneratorSelection,
) -> Result<WorkloadIdentity> {
    let target = crate::art::load_template("arch", 12, 12)?;
    Ok(WorkloadIdentity {
        template: "arch".to_string(),
        match_mode: "emergence".to_string(),
        canvas_width: 24,
        canvas_height: 24,
        target_width: target.width,
        target_height: target.height,
        target_bitmap_sha256: target.sha256(),
        start_offset: 0,
        work_windows: if matches!(args.demand_mode, PiDemandMode::SearchOverlap) {
            args.search_work_windows
        } else {
            0
        },
        max_offset: -1,
        chunk_size: 4096,
        source_mode: "growing".to_string(),
        cache_state: "cold".to_string(),
        profile: "performance".to_string(),
        requested_backend: "cpu".to_string(),
        gpu_mode: "off".to_string(),
        gpu_device: "auto".to_string(),
        generator_backend: selection.effective_backend().to_string(),
        selected_variant: selection
            .selected_variant
            .map_or_else(String::new, |variant| variant.as_str().to_string()),
        y_cruncher_path_present: selection.y_cruncher_path_present,
        y_cruncher_executable_sha256: selection.y_cruncher_executable_sha256.clone(),
        cpu_workers: args.workers,
        cpu_utilization: 100,
        queue_depth: 1,
        memory_limit_mb: 512,
    })
}
