//! `benchmark` and `stress-test`: measuring throughput, and deliberately
//! loading the machine while doing it.

use std::sync::Arc;
use std::thread;

use anyhow::{Result, anyhow};

use crate::cli::{
    BenchmarkArgs, BenchmarkCacheState, BenchmarkSourceMode, StressTarget, StressTestArgs,
};
use crate::commands::hunt::confirm_max_stress;
use crate::commands::{CommandContext, CommandExit, print_json};
use crate::performance::{GeneratorBackendChoice, GpuMode, SearchBackendChoice};
use crate::search::{ResourceBudget, ResourceBudgetSnapshot};

pub fn benchmark(args: BenchmarkArgs, context: &CommandContext) -> Result<()> {
    let outcome = crate::benchmark_runner::run(args)?;
    if context.json {
        print_json(&outcome.report)?;
    } else {
        println!("benchmark status={}", outcome.report.status);
        println!(
            "windows/sec: {:.0}",
            outcome.report.scanned_windows_per_second
        );
        println!(
            "source digits/sec: {:.0}",
            outcome.report.source_digits_per_second
        );
        println!("best score: {:.2}%", outcome.report.best_score * 100.0);
    }
    if outcome.exit_code == 0 {
        Ok(())
    } else {
        Err(anyhow!(CommandExit(outcome.exit_code)))
    }
}

pub fn stress_test(args: StressTestArgs, context: &CommandContext) -> Result<()> {
    confirm_max_stress(&args, context.json)?;
    let work_windows = match args.stress_duration.unwrap_or(10).checked_mul(10_000) {
        Some(value) => value,
        None => return stress_selection_error(context, "stress_duration work budget overflowed"),
    };
    let cpu_workers = match (args.workers, args.cpu_workers) {
        (Some(legacy), Some(current)) if legacy != current => {
            return stress_selection_error(context, "workers and cpu_workers must match");
        }
        (Some(value), None) | (None, Some(value)) | (Some(value), Some(_)) => Some(value),
        (None, None) => None,
    };
    if let Err(reason) = validate_stress_selection(&args) {
        return stress_selection_error(context, reason);
    }
    match args.stress_target {
        StressTarget::Cpu => {
            let report = run_stress_cpu(&args, work_windows, cpu_workers)?;
            print_json(&serde_json::json!({
                "schema_version": 1,
                "status": report.status,
                "requested_backend": "cpu",
                "resolved_backend": report.resolved_backend,
                "fallback": false,
                "scanned_windows": report.scanned_windows,
                "work_windows": work_windows,
                "cpu_workers": report.config.cpu_workers,
                "queue_depth": report.config.queue_depth,
                "memory_limit_mb": report.config.memory_limit_mb,
                "stop_reason": report.stop_reason,
            }))
        }
        StressTarget::Gpu => {
            let requested = args.backend.unwrap_or(SearchBackendChoice::Gpu);
            let report = run_stress_backend(&args, work_windows, cpu_workers, requested)?;
            let backend_name = match requested {
                SearchBackendChoice::Cuda => "cuda",
                _ => "wgpu",
            };
            if report.report.status == "runtime_fault" {
                let capability = report.report.gpu.capability.clone().unwrap_or_else(|| {
                    crate::capability::GpuCapability::unavailable("runtime_fault")
                });
                print_json(&serde_json::json!({
                    "schema_version": 1,
                    "status": "error",
                    "aggregate": {
                        "status": "error",
                        "queue": report.report.queue,
                        "memory": report.report.memory,
                        "cpu_permits_max": report.report.cpu_permits_max,
                    },
                    "lanes": [{
                        "lane": "gpu",
                        "requested_backend": backend_name,
                        "resolved_backend": report.report.resolved_backend,
                        "status": "runtime_fault",
                        "reason": report.report.reason,
                        "fallback": false,
                        "test_only_mock": report.report.gpu.test_only_mock
                            || crate::gpu_ring::test_backend_mock_enabled(backend_name),
                        "scanned_windows": report.report.scanned_windows,
                        "capability": capability,
                    }]
                }))?;
                return Err(anyhow!(CommandExit(1)));
            }
            print_json(&report.report)?;
            if report.exit_code == 0 {
                Ok(())
            } else {
                Err(anyhow!(CommandExit(report.exit_code)))
            }
        }
        StressTarget::Both => {
            let cpu_args = stress_benchmark_args(
                &args,
                work_windows,
                cpu_workers,
                SearchBackendChoice::Cpu,
                GpuMode::Off,
            );
            let gpu_args = stress_benchmark_args(
                &args,
                work_windows,
                cpu_workers,
                SearchBackendChoice::Auto,
                GpuMode::Auto,
            );
            let settings = crate::benchmark_runner::settings(&cpu_args);
            let budget = ResourceBudget::new(
                settings.limits.queue_depth,
                settings.limits.memory_limit_mb,
                settings.limits.cpu_workers,
            )?;
            let (cpu, gpu) = thread::scope(|scope| {
                let cpu_budget = Arc::clone(&budget);
                let gpu_budget = Arc::clone(&budget);
                let cpu = scope.spawn(move || {
                    crate::benchmark_runner::run_with_budget(cpu_args, Some(cpu_budget))
                });
                let gpu = scope.spawn(move || {
                    crate::benchmark_runner::run_optional_accelerator_with_budget(
                        gpu_args, gpu_budget,
                    )
                });
                let cpu = cpu
                    .join()
                    .map_err(|_| anyhow!("CPU stress lane panicked"))??;
                let gpu = gpu
                    .join()
                    .map_err(|_| anyhow!("GPU stress lane panicked"))??;
                Ok::<_, anyhow::Error>((cpu, gpu))
            })?;
            let snapshot = budget.snapshot();
            let runtime_fault = gpu.report.status == "runtime_fault";
            let optional_lane_completed = matches!(gpu.report.status.as_str(), "ok" | "skip");
            let status = if !runtime_fault && cpu.report.status == "ok" && optional_lane_completed {
                "ok"
            } else {
                "error"
            };
            let gpu_lane = if runtime_fault {
                let resolved_backend = gpu.report.resolved_backend.as_deref().unwrap_or("");
                let fault_capability = gpu.report.gpu.capability.clone().unwrap_or_else(|| {
                    crate::capability::GpuCapability::unavailable("runtime_fault")
                });
                serde_json::json!({
                    "lane": "gpu",
                    "requested_backend": "auto",
                    "resolved_backend": resolved_backend,
                    "status": "runtime_fault",
                    "reason": gpu.report.reason,
                    "fallback": false,
                    "test_only_mock": true,
                    "scanned_windows": gpu.report.scanned_windows,
                    "capability": fault_capability,
                    "auto_candidate_order": ["cuda", "wgpu", "cpu"],
                    "auto_min_work_windows": gpu.report.auto_min_work_windows,
                    "backend_candidates": gpu.report.backend_candidates,
                    "candidate_capability_states": auto_candidate_states(&gpu.report),
                    "resource_budget": {
                        "shared": true,
                        "gpu_permits_acquired": snapshot.gpu_permits_peak
                    }
                })
            } else {
                stress_auto_lane_report(&gpu.report, &snapshot)
            };
            print_json(&serde_json::json!({
                "schema_version": 1,
                "status": status,
                "aggregate": aggregate_stress_report(&snapshot, status),
                "lanes": [
                    stress_lane_report("cpu", "cpu", &cpu.report, &snapshot),
                    gpu_lane
                ]
            }))?;
            if runtime_fault {
                Err(anyhow!(CommandExit(1)))
            } else {
                Ok(())
            }
        }
    }
}

fn stress_auto_lane_report(
    report: &crate::benchmark_report::BenchmarkReport,
    snapshot: &ResourceBudgetSnapshot,
) -> serde_json::Value {
    serde_json::json!({
        "lane": "gpu",
        "requested_backend": "auto",
        "resolved_backend": report.resolved_backend.as_deref().unwrap_or(""),
        "status": report.status,
        "skip_reason": report.skip_reason,
        "fallback": false,
        "test_only_mock": report.gpu.test_only_mock,
        "scanned_windows": report.scanned_windows,
        "capability": report.gpu.capability,
        "auto_candidate_order": ["cuda", "wgpu", "cpu"],
        "auto_min_work_windows": report.auto_min_work_windows,
        "backend_candidates": report.backend_candidates,
        "candidate_capability_states": auto_candidate_states(report),
        "resource_budget": {
            "shared": true,
            "gpu_permits_acquired": snapshot.gpu_permits_peak,
            "cpu_permits_in_use": snapshot.cpu_permits_in_use,
            "cpu_permits_peak": snapshot.cpu_permits_peak,
            "cpu_permits_max": snapshot.cpu_permits_max
        }
    })
}

fn auto_candidate_states(
    report: &crate::benchmark_report::BenchmarkReport,
) -> Vec<serde_json::Value> {
    report
        .backend_candidates
        .iter()
        .map(|candidate| {
            let capability_state = match (candidate.backend, candidate.status, candidate.reason) {
                ("cpu", _, _) => "not_applicable",
                (_, "selected", _) => "preflight_ok",
                (_, _, "below_auto_min_work_windows_before_capability_probe") => "not_probed",
                (_, _, _) => "unavailable",
            };
            serde_json::json!({
                "backend": candidate.backend,
                "capability_state": capability_state,
                "reason": candidate.reason,
            })
        })
        .collect()
}

fn run_stress_cpu(
    args: &StressTestArgs,
    work_windows: u64,
    cpu_workers: Option<usize>,
) -> Result<crate::benchmark_report::BenchmarkReport> {
    Ok(crate::benchmark_runner::run(stress_benchmark_args(
        args,
        work_windows,
        cpu_workers,
        SearchBackendChoice::Cpu,
        GpuMode::Off,
    ))?
    .report)
}

fn stress_lane_report(
    lane: &str,
    requested_backend: &str,
    report: &crate::benchmark_report::BenchmarkReport,
    snapshot: &ResourceBudgetSnapshot,
) -> serde_json::Value {
    serde_json::json!({
        "lane": lane,
        "requested_backend": requested_backend,
        "resolved_backend": report.resolved_backend,
        "status": report.status,
        "fallback": report.fallback,
        "test_only_mock": report.gpu.test_only_mock,
        "scanned_windows": report.scanned_windows,
        "stop_reason": report.stop_reason,
        "resource_budget": {
            "shared": true,
            "gpu_permits_acquired": snapshot.gpu_permits_peak,
            "cpu_permits_in_use": snapshot.cpu_permits_in_use,
            "cpu_permits_peak": snapshot.cpu_permits_peak,
            "cpu_permits_max": snapshot.cpu_permits_max
        }
    })
}

fn aggregate_stress_report(snapshot: &ResourceBudgetSnapshot, status: &str) -> serde_json::Value {
    serde_json::json!({
        "status": status,
        "resource_budget": {
            "shared": true,
            "memory": {
                "logical_reserved_bytes": snapshot.memory_reserved_bytes,
                "logical_peak_bytes": snapshot.memory_peak_bytes,
                "logical_budget_bytes": snapshot.memory_limit_bytes
            },
            "queue": {
                "current_occupancy": snapshot.queue_current,
                "max_occupancy": snapshot.queue_peak,
                "global_limit": snapshot.queue_limit
            },
            "gpu_permits_acquired": snapshot.gpu_permits_peak
        },
        "queue": {
            "current_occupancy": snapshot.queue_current,
            "max_occupancy": snapshot.queue_peak,
            "global_limit": snapshot.queue_limit
        },
        "cpu_permits_in_use": snapshot.cpu_permits_in_use,
        "cpu_permits_peak": snapshot.cpu_permits_peak,
        "cpu_permits_max": snapshot.cpu_permits_max,
        "gpu_permits_acquired": snapshot.gpu_permits_peak
    })
}

fn run_stress_backend(
    args: &StressTestArgs,
    work_windows: u64,
    cpu_workers: Option<usize>,
    backend: SearchBackendChoice,
) -> Result<crate::benchmark_runner::BenchmarkOutcome> {
    crate::benchmark_runner::run(stress_benchmark_args(
        args,
        work_windows,
        cpu_workers,
        backend,
        GpuMode::On,
    ))
}

fn stress_benchmark_args(
    args: &StressTestArgs,
    work_windows: u64,
    cpu_workers: Option<usize>,
    backend: SearchBackendChoice,
    gpu: GpuMode,
) -> BenchmarkArgs {
    BenchmarkArgs {
        template: args.template.clone(),
        seconds: args.stress_duration.unwrap_or(10),
        work_windows: Some(work_windows),
        start_offset: 0,
        max_offset: None,
        source_mode: BenchmarkSourceMode::Growing,
        cache_state: BenchmarkCacheState::Cold,
        repetitions: 1,
        warmup: 0,
        profile: args.profile,
        backend: Some(backend),
        generator_backend: GeneratorBackendChoice::Auto,
        y_cruncher_path: None,
        gpu: Some(gpu),
        gpu_device: args.gpu_device.clone(),
        cpu_utilization: Some(100),
        gpu_utilization: None,
        cpu_workers,
        chunk_size: None,
        queue_depth: args.queue_depth,
        memory_limit_mb: args.memory_limit_mb,
        show_metrics: true,
    }
}

fn validate_stress_selection(args: &StressTestArgs) -> std::result::Result<(), &'static str> {
    match args.stress_target {
        StressTarget::Cpu => match (args.backend, args.gpu) {
            (None | Some(SearchBackendChoice::Cpu), None | Some(GpuMode::Off)) => Ok(()),
            _ => Err("cpu stress requires backend=cpu and gpu=off"),
        },
        StressTarget::Gpu => match (args.backend, args.gpu) {
            (
                None | Some(SearchBackendChoice::Gpu | SearchBackendChoice::Cuda),
                None | Some(GpuMode::On),
            ) => Ok(()),
            _ => Err("gpu stress requires a strict gpu or cuda selection"),
        },
        StressTarget::Both => match (args.backend, args.gpu) {
            (None, None) | (Some(SearchBackendChoice::Auto), Some(GpuMode::Auto)) => Ok(()),
            _ => Err("both stress accepts only omitted selection or backend=auto with gpu=auto"),
        },
    }
}

fn stress_selection_error(context: &CommandContext, reason: &str) -> Result<()> {
    if context.json {
        print_json(&serde_json::json!({
            "schema_version":1,
            "status":"selection_error",
            "reason":reason,
            "requested_backend":"",
            "resolved_backend":null,
            "fallback":false,
            "scanned_windows":0
        }))?;
    }
    Err(anyhow!(CommandExit(2)))
}
