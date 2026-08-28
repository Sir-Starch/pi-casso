use anyhow::Result;
use num_traits::ToPrimitive;

use crate::art::Bitmap;
use crate::benchmark_contract::{
    BackendResolution, BenchmarkBounds, WorkloadIdentity, cache_state, resolved_generator,
    source_mode,
};
use crate::benchmark_report::{
    AggregateMetrics, BenchmarkConfig, BenchmarkReport, GpuReport, MemoryReport, QueueReport,
    SourceReport, StageTimings, Waits,
};
use crate::benchmark_stats::{git_sha, machine_identity};
use crate::capability::GpuCapability;
use crate::cli::BenchmarkArgs;
use crate::performance::PerformanceSettings;

pub fn identity(
    args: &BenchmarkArgs,
    target: &Bitmap,
    settings: &PerformanceSettings,
    bounds: &BenchmarkBounds,
    backend: &BackendResolution,
) -> WorkloadIdentity {
    WorkloadIdentity {
        template: args.template.clone().unwrap_or_else(|| "arch".to_string()),
        match_mode: "emergence".to_string(),
        canvas_width: 24,
        canvas_height: 24,
        target_width: target.width,
        target_height: target.height,
        target_bitmap_sha256: target.sha256(),
        start_offset: args.start_offset,
        work_windows: bounds.work_windows,
        max_offset: bounds
            .max_offset
            .and_then(|value| i64::try_from(value).ok())
            .unwrap_or(-1),
        chunk_size: settings.limits.chunk_size,
        source_mode: source_mode(args.source_mode).to_string(),
        cache_state: cache_state(args.cache_state).to_string(),
        profile: args.profile.as_str().to_string(),
        requested_backend: backend.requested.to_string(),
        gpu_mode: backend.gpu_mode.to_string(),
        gpu_device: args
            .gpu_device
            .clone()
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(|| "auto".to_string()),
        generator_backend: resolved_generator(args.generator_backend).to_string(),
        selected_variant: String::new(),
        y_cruncher_path_present: false,
        y_cruncher_executable_sha256: String::new(),
        cpu_workers: settings.limits.cpu_workers,
        cpu_utilization: settings.limits.cpu_utilization,
        queue_depth: settings.limits.queue_depth,
        memory_limit_mb: settings.limits.memory_limit_mb,
    }
}

pub fn apply_generator_identity(
    identity: &mut WorkloadIdentity,
    selection: &crate::pi::GeneratorSelection,
) {
    identity.generator_backend = selection.effective_backend().to_string();
    identity.selected_variant = selection
        .selected_variant
        .map_or_else(String::new, |variant| variant.as_str().to_string());
    identity.y_cruncher_path_present = selection.y_cruncher_path_present;
    identity.y_cruncher_executable_sha256 = selection.y_cruncher_executable_sha256.clone();
}

pub fn empty_report(
    args: &BenchmarkArgs,
    settings: &PerformanceSettings,
    bounds: &BenchmarkBounds,
    backend: &BackendResolution,
    identity: WorkloadIdentity,
    workload_id: String,
    gpu_capability: Option<GpuCapability>,
) -> BenchmarkReport {
    let status = backend.status.to_string();
    let reason = backend.reason.clone();
    BenchmarkReport {
        schema_version: 1,
        status,
        reason,
        skip_reason: String::new(),
        requested_backend: backend.requested.to_string(),
        requested_generator_backend: args.generator_backend.as_str().to_string(),
        selected_variant: identity.selected_variant.clone(),
        generator_executable_sha256: String::new(),
        unavailable_backends: Vec::new(),
        resolved_backend: backend.resolved.map(str::to_string),
        backend_fault_status: "none".to_string(),
        fallback: backend.fallback,
        fallback_reason: if backend.fallback {
            backend.reason.clone()
        } else {
            String::new()
        },
        fallback_count: 0,
        backend_device: backend.resolved.unwrap_or("unavailable").to_string(),
        backend_feature_available: backend.resolved.is_some(),
        auto_min_work_windows: backend.auto_min_work_windows,
        backend_candidates: backend.backend_candidates.clone(),
        workload_id,
        workload_identity: identity,
        source_mode: source_mode(args.source_mode).to_string(),
        cache_state: cache_state(args.cache_state).to_string(),
        cache_reset: matches!(args.cache_state, crate::cli::BenchmarkCacheState::Cold),
        cache_instance_id: String::new(),
        warm_up_completed: false,
        page_cache_control: "uncontrolled".to_string(),
        start_offset: args.start_offset,
        effective_end: bounds.effective_end,
        source_end_exclusive: bounds.source_end_exclusive,
        window_len: 24 * 24,
        scanned_windows: 0,
        scanned_windows_per_second: 0.0,
        source_digits_per_second: 0.0,
        logical_window_digits_per_second: 0.0,
        elapsed_seconds: 0.0,
        stop_reason: String::new(),
        best_score: 0.0,
        repetitions: args.repetitions,
        warmup: args.warmup,
        median: AggregateMetrics::default(),
        p95: AggregateMetrics::default(),
        stage_timings: StageTimings::default(),
        waits: Waits::default(),
        overlap_wait_ms: 0,
        cache_write_ms: 0,
        producer_epochs: 0,
        coalesced_request_count: 0,
        generation_batches: 0,
        event_wake_latency_ms: 0,
        lead_digits: 0,
        high_water_digits: 0,
        source_lag_digits: 0,
        generator_digits_per_second: 0.0,
        telemetry_enabled: args.show_metrics,
        test_only_mock: false,
        config: BenchmarkConfig {
            profile: args.profile.as_str().to_string(),
            cpu_workers: settings.limits.cpu_workers,
            cpu_utilization: settings.limits.cpu_utilization,
            gpu_utilization: settings.effective_gpu_utilization(),
            chunk_size: settings.limits.chunk_size,
            queue_depth: settings.limits.queue_depth,
            memory_limit_mb: settings.limits.memory_limit_mb,
        },
        memory: MemoryReport {
            logical_peak_mb: settings
                .estimated_memory_mb(24 * 24)
                .to_f64()
                .unwrap_or(f64::MAX),
            gpu_vram_status: "unavailable".to_string(),
            ..MemoryReport::default()
        },
        source: SourceReport::default(),
        queue: QueueReport {
            current_occupancy: 0,
            max_occupancy: 0,
            permits: u64::try_from(settings.limits.queue_depth).unwrap_or(u64::MAX),
            global_limit: u64::try_from(settings.limits.queue_depth).unwrap_or(u64::MAX),
        },
        reducer: Default::default(),
        cpu_permits_in_use: 0,
        cpu_permits_peak: 0,
        cpu_permits_max: u64::try_from(settings.limits.cpu_workers).unwrap_or(u64::MAX),
        gpu: {
            let capability =
                gpu_capability.unwrap_or_else(|| GpuCapability::unavailable("not_requested"));
            GpuReport {
                kernel_arch: capability.kernel_arch.clone(),
                kernel_sha256: capability.kernel_sha256.clone(),
                kernel_source_sha256: capability.kernel_source_sha256.clone(),
                capability: Some(capability),
                ..GpuReport::default()
            }
        },
        gpu_duty_policy_percent: settings.effective_gpu_utilization(),
        gpu_duty_window_ms: crate::performance::GPU_DUTY_WINDOW_MS,
        gpu_duty_wait_ms: 0,
        gpu_initial_submission_wait_ms: 0,
        active_submission_ratio: 0.0,
        dispatch_quantum_ratio: 0.0,
        raw_run_paths: Vec::new(),
        raw_runs: Vec::new(),
        git_sha: git_sha(),
        machine: machine_identity("unavailable", "unavailable"),
    }
}

pub fn selection_error_report(args: &BenchmarkArgs, reason: &str) -> Result<BenchmarkReport> {
    let target = crate::art::load_template(args.template.as_deref().unwrap_or("arch"), 12, 12)?;
    let settings = super::benchmark_runner::settings(args);
    let bounds = BenchmarkBounds {
        work_windows: args.work_windows.unwrap_or(0),
        max_offset: None,
        effective_end: args.start_offset,
        source_end_exclusive: args.start_offset,
    };
    let backend = BackendResolution {
        status: "selection_error",
        requested: "",
        resolved: None,
        gpu_mode: "",
        fallback: false,
        reason: reason.to_string(),
        auto_min_work_windows: crate::benchmark_contract::AUTO_MIN_WORK_WINDOWS,
        backend_candidates: Vec::new(),
    };
    let identity = identity(args, &target, &settings, &bounds, &backend);
    let workload_id = identity.workload_id()?;
    Ok(empty_report(
        args,
        &settings,
        &bounds,
        &backend,
        identity,
        workload_id,
        None,
    ))
}
