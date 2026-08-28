use std::sync::Arc;

use anyhow::Result;

use crate::art;
use crate::benchmark_build::{
    apply_generator_identity, empty_report, identity, selection_error_report,
};
use crate::benchmark_contract::{
    AUTO_MIN_WORK_WINDOWS, BackendPreflightRequest, BenchmarkBounds, CudaPreflight, WgpuPreflight,
    cuda_preflight, resolve_backend_preflight,
};
use crate::benchmark_execute::{apply_runs, execute_repetition_with_budget};
use crate::benchmark_report::BenchmarkReport;
use crate::capability::GpuCapability;
use crate::cli::{BenchmarkArgs, BenchmarkCacheState, BenchmarkSourceMode};
use crate::performance::{
    GpuMode, PerformanceOverrides, PerformanceSettings, SearchBackendChoice, ThermalMode,
};
use crate::pi::{GENERATOR_FIXED_BYTES, PiCache};
use crate::search::MatchMode;
use crate::search::{DigitReaderPool, ResourceBudget};

pub struct BenchmarkOutcome {
    pub report: BenchmarkReport,
    pub exit_code: i32,
}

pub fn run(args: BenchmarkArgs) -> Result<BenchmarkOutcome> {
    run_with_budget_mode(args, None, false)
}

pub fn run_with_budget(
    args: BenchmarkArgs,
    shared_budget: Option<Arc<ResourceBudget>>,
) -> Result<BenchmarkOutcome> {
    run_with_budget_mode(args, shared_budget, false)
}

pub fn run_optional_accelerator_with_budget(
    args: BenchmarkArgs,
    shared_budget: Arc<ResourceBudget>,
) -> Result<BenchmarkOutcome> {
    run_with_budget_mode(args, Some(shared_budget), true)
}

fn run_with_budget_mode(
    args: BenchmarkArgs,
    shared_budget: Option<Arc<ResourceBudget>>,
    auto_cpu_is_skip: bool,
) -> Result<BenchmarkOutcome> {
    if let Some(reason) = invalid_resource_reason(&args) {
        return Ok(BenchmarkOutcome {
            report: selection_error_report(&args, reason)?,
            exit_code: 2,
        });
    }
    if args.seconds == 0 && args.work_windows.is_some_and(|windows| windows > 0) {
        return Ok(BenchmarkOutcome {
            report: selection_error_report(
                &args,
                "seconds must be positive when work_windows is nonzero",
            )?,
            exit_code: 2,
        });
    }
    let template = args.template.as_deref().unwrap_or("arch");
    let target = art::load_template(template, 12, 12)?;
    let mut settings = settings(&args);
    let bounds = match BenchmarkBounds::parse(&args, settings.limits.chunk_size, 24 * 24) {
        Ok(bounds) => bounds,
        Err(error) => {
            return Ok(BenchmarkOutcome {
                report: selection_error_report(&args, &error.to_string())?,
                exit_code: 2,
            });
        }
    };
    let effective_work_windows = bounds.scanned_windows(args.start_offset);
    let force_test_wgpu = crate::gpu_ring::test_mock_enabled()
        && std::env::var("PI_CASSO_TEST_BACKEND_FAIL_AFTER_PREFLIGHT")
            .is_ok_and(|backend| backend == "wgpu");
    let preflight_work_windows = if force_test_wgpu {
        effective_work_windows.max(AUTO_MIN_WORK_WINDOWS)
    } else {
        effective_work_windows
    };
    let cuda_capability = if force_test_wgpu {
        None
    } else {
        cuda_capability(&args, preflight_work_windows)
    };
    let cuda = cuda_capability
        .as_ref()
        .map_or(CudaPreflight::NotProbed, cuda_preflight);
    let wgpu_capability = if matches!(cuda, CudaPreflight::Eligible) {
        None
    } else {
        wgpu_capability(&args, preflight_work_windows)
    };
    let wgpu = wgpu_capability
        .as_ref()
        .map_or(WgpuPreflight::NotProbed, wgpu_preflight);
    let backend = resolve_backend_preflight(BackendPreflightRequest {
        backend: args.backend,
        gpu: args.gpu,
        effective_work_windows: preflight_work_windows,
        cuda,
        wgpu,
    });
    let generator =
        crate::pi::resolve_generator(args.generator_backend, args.y_cruncher_path.as_deref())?;
    apply_resolved_backend(&mut settings, backend.resolved);
    let mut workload_identity = identity(&args, &target, &settings, &bounds, &backend);
    apply_generator_identity(&mut workload_identity, &generator);
    let workload_id = workload_identity.workload_id()?;
    let selected_variant = workload_identity.selected_variant.clone();
    let capability = match backend.resolved {
        Some("cuda") => cuda_capability,
        Some("wgpu") => wgpu_capability,
        Some("cpu") | Some(_) | None => cuda_capability.or(wgpu_capability),
    };
    let mut report = empty_report(
        &args,
        &settings,
        &bounds,
        &backend,
        workload_identity,
        workload_id,
        capability,
    );
    report.selected_variant = selected_variant;
    report.generator_executable_sha256 = generator.executable_sha256.clone();
    report.unavailable_backends = generator.unavailable_backends.clone();
    report.requested_generator_backend = generator.requested_backend.to_string();
    if auto_cpu_is_skip && backend.requested == "auto" && backend.resolved == Some("cpu") {
        report.status = "skip".to_string();
        report.skip_reason = if backend.reason == "auto_threshold_cpu" {
            backend.reason.clone()
        } else {
            backend
                .backend_candidates
                .iter()
                .rev()
                .find(|candidate| candidate.backend != "cpu" && !candidate.reason.is_empty())
                .map_or_else(
                    || backend.reason.clone(),
                    |candidate| candidate.reason.to_string(),
                )
        };
        report.resolved_backend = None;
        report.fallback = false;
        report.fallback_reason.clear();
        return Ok(BenchmarkOutcome {
            report,
            exit_code: 0,
        });
    }
    if !generator.is_available() {
        report.status = "unavailable".to_string();
        report.reason = generator.reason;
        report.resolved_backend = None;
        return Ok(BenchmarkOutcome {
            report,
            exit_code: 2,
        });
    }
    if backend.status != "ok" {
        let exit_code = if report
            .gpu
            .capability
            .as_ref()
            .is_some_and(|capability| capability.probe_exit_code() == 1)
        {
            1
        } else {
            2
        };
        return Ok(BenchmarkOutcome { report, exit_code });
    }
    let max_read_len = match settings.limits.chunk_size.checked_add(24 * 24 - 1) {
        Some(value) => value,
        None => {
            return Ok(resource_error_outcome(
                report,
                anyhow::anyhow!("digit reader buffer size overflowed"),
            ));
        }
    };
    let reader_capacity = match DigitReaderPool::configured_path_capacity_bytes(
        settings.limits.cpu_workers,
        settings.limits.queue_depth,
        max_read_len,
    ) {
        Ok(value) => value,
        Err(error) => return Ok(resource_error_outcome(report, error)),
    };
    if let Err(error) = ResourceBudget::validate_cpu_pool_size(settings.limits.cpu_workers) {
        return Ok(resource_error_outcome(report, error));
    }
    if let Err(error) = ResourceBudget::validate_reader_capacity_limit(
        settings.limits.memory_limit_mb,
        reader_capacity,
    ) {
        return Ok(resource_error_outcome(report, error));
    }
    let budget = match shared_budget {
        Some(budget) => budget,
        None => match ResourceBudget::new(
            settings.limits.queue_depth,
            settings.limits.memory_limit_mb,
            settings.limits.cpu_workers,
        ) {
            Ok(budget) => budget,
            Err(error) => return Ok(resource_error_outcome(report, error)),
        },
    };
    let budget_snapshot = budget.snapshot();
    report.memory.rss_peak_mb = budget_snapshot.rss_peak_mb;
    report.memory.rss_baseline_mb = budget_snapshot.rss_baseline_mb;
    report.memory.rss_margin_mb = budget_snapshot.rss_margin_mb;
    if let Err(error) = budget.validate_minimum(24 * 24) {
        return Ok(resource_error_outcome(report, error));
    }
    if let Err(error) = budget.validate_reader_capacity(reader_capacity) {
        return Ok(resource_error_outcome(report, error));
    }
    if matches!(args.source_mode, BenchmarkSourceMode::Growing) {
        if let Err(error) = budget.validate_generation_window(24 * 24, GENERATOR_FIXED_BYTES) {
            return Ok(resource_error_outcome(report, error));
        }
    }
    if bounds.effective_end <= args.start_offset {
        report.status = "ok".to_string();
        report.stop_reason = if bounds.work_windows == 0 && args.seconds == 0 {
            "baseline"
        } else {
            "empty_range"
        }
        .to_string();
        return Ok(BenchmarkOutcome {
            report,
            exit_code: 0,
        });
    }

    let warm_cache = if matches!(args.cache_state, BenchmarkCacheState::Warm) {
        Some(fresh_cache("warm")?)
    } else {
        None
    };
    for warmup in 0..args.warmup {
        let cache = match warm_cache.as_ref() {
            Some(cache) => cache.clone(),
            None => fresh_cache(&format!("warmup-{warmup}"))?,
        };
        let warmup_repetition = u32::MAX - warmup;
        let warmup_result = execute_repetition_with_budget(
            &args,
            &settings,
            &bounds,
            &target,
            cache,
            warmup_repetition,
            Arc::clone(&budget),
        );
        if let Err(error) = warmup_result {
            if report
                .resolved_backend
                .as_deref()
                .is_some_and(crate::gpu_ring::test_runtime_fault_for)
            {
                return Ok(runtime_fault_outcome(report, error));
            }
            return Err(error);
        }
    }

    let mut runs = Vec::with_capacity(usize::try_from(args.repetitions)?);
    for repetition in 0..args.repetitions {
        let cache = match warm_cache.as_ref() {
            Some(cache) => cache.clone(),
            None => fresh_cache(&format!("measured-{repetition}"))?,
        };
        let run = execute_repetition_with_budget(
            &args,
            &settings,
            &bounds,
            &target,
            cache,
            repetition,
            Arc::clone(&budget),
        );
        match run {
            Ok(run) => runs.push(run),
            Err(error)
                if report
                    .resolved_backend
                    .as_deref()
                    .is_some_and(crate::gpu_ring::test_runtime_fault_for) =>
            {
                return Ok(runtime_fault_outcome(report, error));
            }
            Err(error) => return Err(error),
        }
    }
    apply_runs(
        &mut report,
        runs,
        matches!(args.cache_state, BenchmarkCacheState::Warm) && args.warmup > 0,
    );
    if report.backend_fault_status == "runtime_fault" {
        if let Some(capability) = report.gpu.capability.as_mut() {
            capability.record_runtime_fault(&report.fallback_reason);
        }
    }
    Ok(BenchmarkOutcome {
        report,
        exit_code: 0,
    })
}

fn resource_error_outcome(mut report: BenchmarkReport, error: anyhow::Error) -> BenchmarkOutcome {
    report.status = "resource_error".to_string();
    report.reason = error.to_string();
    report.stop_reason = "resource_error".to_string();
    report.resolved_backend = None;
    report.scanned_windows = 0;
    BenchmarkOutcome {
        report,
        exit_code: 3,
    }
}

fn runtime_fault_outcome(mut report: BenchmarkReport, error: anyhow::Error) -> BenchmarkOutcome {
    let backend = report.resolved_backend.as_deref().unwrap_or("gpu");
    report.status = "runtime_fault".to_string();
    report.reason = format!("{error:#}");
    report.stop_reason = "runtime_fault".to_string();
    report.backend_fault_status = "runtime_fault".to_string();
    report.fallback = false;
    report.fallback_reason.clear();
    report.fallback_count = 0;
    report.scanned_windows = 0;
    report.test_only_mock = crate::gpu_ring::test_backend_mock_enabled(backend);
    report.gpu.test_only_mock = report.test_only_mock;
    report.gpu.fallback_count = 0;
    if let Some(capability) = report.gpu.capability.as_mut() {
        capability.record_runtime_fault(&report.reason);
    }
    BenchmarkOutcome {
        report,
        exit_code: 1,
    }
}

pub(crate) fn settings(args: &BenchmarkArgs) -> PerformanceSettings {
    PerformanceSettings::from_profile(
        args.profile,
        SearchBackendChoice::Cpu,
        args.generator_backend,
        GpuMode::Off,
        args.gpu_device.clone(),
        ThermalMode::Normal,
        false,
        args.show_metrics,
        MatchMode::Emergence,
        PerformanceOverrides {
            cpu_workers: args.cpu_workers,
            cpu_utilization: args.cpu_utilization,
            gpu_utilization: args.gpu_utilization,
            chunk_size: args.chunk_size,
            queue_depth: args.queue_depth,
            memory_limit_mb: args.memory_limit_mb,
            checkpoint_every_secs: Some(args.seconds.max(1)),
            ..PerformanceOverrides::default()
        },
    )
}

fn wgpu_capability(args: &BenchmarkArgs, effective_work_windows: u64) -> Option<GpuCapability> {
    let may_select_wgpu = matches!(
        (args.backend, args.gpu),
        (None, Some(GpuMode::On))
            | (Some(SearchBackendChoice::Gpu), None | Some(GpuMode::On))
            | (Some(SearchBackendChoice::Auto), None | Some(GpuMode::Auto))
            | (None, None | Some(GpuMode::Auto))
    );
    let automatic = matches!(
        (args.backend, args.gpu),
        (None, None | Some(GpuMode::Auto))
            | (Some(SearchBackendChoice::Auto), None | Some(GpuMode::Auto))
    );
    if !may_select_wgpu || (automatic && effective_work_windows < AUTO_MIN_WORK_WINDOWS) {
        return None;
    }

    Some(GpuCapability::detect_with_filter(
        args.gpu_device.as_deref(),
    ))
}

fn wgpu_preflight(capability: &GpuCapability) -> WgpuPreflight {
    if capability.capability_state == "preflight_ok" {
        WgpuPreflight::Eligible
    } else {
        WgpuPreflight::Unavailable(match capability.reason.as_str() {
            "adapter_unavailable" => "adapter_unavailable",
            "pipeline_preflight_unavailable" => "pipeline_preflight_unavailable",
            _ => "pipeline_preflight_unavailable",
        })
    }
}

fn cuda_capability(args: &BenchmarkArgs, effective_work_windows: u64) -> Option<GpuCapability> {
    let explicit = matches!(
        (args.backend, args.gpu),
        (Some(SearchBackendChoice::Cuda), None | Some(GpuMode::On))
    );
    let automatic = matches!(
        (args.backend, args.gpu),
        (None, None | Some(GpuMode::Auto))
            | (Some(SearchBackendChoice::Auto), None | Some(GpuMode::Auto))
    );
    if !explicit && (!automatic || effective_work_windows < AUTO_MIN_WORK_WINDOWS) {
        return None;
    }
    #[cfg(feature = "cuda-native")]
    {
        Some(crate::cuda::detect_capability())
    }
    #[cfg(not(feature = "cuda-native"))]
    {
        Some(GpuCapability::cuda_unavailable(
            "cuda_not_compiled",
            "not_attempted",
        ))
    }
}

fn apply_resolved_backend(settings: &mut PerformanceSettings, resolved: Option<&str>) {
    match resolved {
        Some("wgpu") => {
            settings.backend = SearchBackendChoice::Gpu;
            settings.gpu = GpuMode::On;
        }
        Some("cuda") => {
            settings.backend = SearchBackendChoice::Cuda;
            settings.gpu = GpuMode::On;
        }
        Some("cpu") | None => {
            settings.backend = SearchBackendChoice::Cpu;
            settings.gpu = GpuMode::Off;
        }
        Some(_) => {
            settings.backend = SearchBackendChoice::Cpu;
            settings.gpu = GpuMode::Off;
        }
    }
}

fn invalid_resource_reason(args: &BenchmarkArgs) -> Option<&'static str> {
    if matches!(args.cpu_workers, Some(0)) {
        return Some("cpu_workers must be at least 1");
    }
    if matches!(args.chunk_size, Some(0)) {
        return Some("chunk_size must be at least 1");
    }
    if matches!(args.queue_depth, Some(0)) {
        return Some("queue_depth must be at least 1");
    }
    if matches!(args.memory_limit_mb, Some(0)) {
        return Some("memory_limit_mb must be at least 1");
    }
    None
}

fn fresh_cache(label: &str) -> Result<PiCache> {
    let path = crate::storage::app_data_dir()?
        .join("benchmark-cache")
        .join(format!("{}-{label}", uuid::Uuid::new_v4()))
        .join("pi-cache.txt");
    let cache = PiCache::new(path);
    cache.validate_reset_lock()?;
    Ok(cache)
}
