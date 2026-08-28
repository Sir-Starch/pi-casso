use std::cmp::Ordering;
use std::process::Command;

use crate::benchmark_report::{
    AggregateMetrics, MachineIdentity, RepetitionReport, StageTimings, Waits,
};

pub fn aggregate(runs: &[RepetitionReport]) -> (AggregateMetrics, AggregateMetrics) {
    if runs.is_empty() {
        return (AggregateMetrics::default(), AggregateMetrics::default());
    }
    let median = AggregateMetrics {
        scanned_windows_per_second: median_f64(runs, |run| run.scanned_windows_per_second),
        source_digits_per_second: median_f64(runs, |run| run.source_digits_per_second),
        logical_window_digits_per_second: median_f64(runs, |run| {
            run.logical_window_digits_per_second
        }),
        elapsed_seconds: median_f64(runs, |run| run.elapsed_seconds),
        overlap_wait_ms: median_u64(runs, |run| run.overlap_wait_ms),
        cache_write_ms: median_u64(runs, |run| run.cache_write_ms),
        generation_wait_ms: median_u64(runs, |run| run.stage_timings.generation_wait_ms),
    };
    let mut by_elapsed: Vec<_> = runs.iter().collect();
    by_elapsed.sort_by(|left, right| {
        left.elapsed_seconds
            .partial_cmp(&right.elapsed_seconds)
            .unwrap_or(Ordering::Equal)
    });
    let index = nearest_rank_index(by_elapsed.len(), 95);
    let p95 = by_elapsed[index].aggregate();
    (median, p95)
}

pub fn aggregate_stage(runs: &[RepetitionReport]) -> StageTimings {
    StageTimings {
        read_ms: median_u64(runs, |run| run.stage_timings.read_ms),
        parse_ms: median_u64(runs, |run| run.stage_timings.parse_ms),
        queue_wait_ms: median_u64(runs, |run| run.stage_timings.queue_wait_ms),
        backend_compute_ms: median_u64(runs, |run| run.stage_timings.backend_compute_ms),
        gpu_allocation_ms: median_u64(runs, |run| run.stage_timings.gpu_allocation_ms),
        gpu_upload_ms: median_u64(runs, |run| run.stage_timings.gpu_upload_ms),
        gpu_dispatch_ms: median_u64(runs, |run| run.stage_timings.gpu_dispatch_ms),
        gpu_readback_map_ms: median_u64(runs, |run| run.stage_timings.gpu_readback_map_ms),
        reduction_ms: median_u64(runs, |run| run.stage_timings.reduction_ms),
        persistence_ms: median_u64(runs, |run| run.stage_timings.persistence_ms),
        generation_wait_ms: median_u64(runs, |run| run.stage_timings.generation_wait_ms),
        throttle_wait_ms: median_u64(runs, |run| run.stage_timings.throttle_wait_ms),
    }
}

pub fn aggregate_waits(runs: &[RepetitionReport]) -> Waits {
    Waits {
        source_ms: median_u64(runs, |run| run.waits.source_ms),
        queue_ms: median_u64(runs, |run| run.waits.queue_ms),
        generator_ms: median_u64(runs, |run| run.waits.generator_ms),
        throttle_ms: median_u64(runs, |run| run.waits.throttle_ms),
    }
}

fn median_f64(runs: &[RepetitionReport], field: impl Fn(&RepetitionReport) -> f64) -> f64 {
    let mut values: Vec<_> = runs.iter().map(field).collect();
    values.sort_by(|left, right| left.partial_cmp(right).unwrap_or(Ordering::Equal));
    values[nearest_rank_index(values.len(), 50)]
}

fn median_u64(runs: &[RepetitionReport], field: impl Fn(&RepetitionReport) -> u64) -> u64 {
    if runs.is_empty() {
        return 0;
    }
    let mut values: Vec<_> = runs.iter().map(field).collect();
    values.sort_unstable();
    values[nearest_rank_index(values.len(), 50)]
}

fn nearest_rank_index(length: usize, percentile: usize) -> usize {
    let rank = length.saturating_mul(percentile).div_ceil(100);
    rank.saturating_sub(1).min(length.saturating_sub(1))
}

pub fn machine_identity(gpu: &str, driver: &str) -> MachineIdentity {
    MachineIdentity {
        os: format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH),
        cpu: cpu_model(),
        gpu: gpu.to_string(),
        driver: driver.to_string(),
        rustc: command_output("rustc", &["--version"]),
        power_policy: command_output("powerprofilesctl", &["get"]),
        thermal_policy: thermal_policy(),
    }
}

pub fn git_sha() -> String {
    command_output("git", &["rev-parse", "HEAD"])
}

fn command_output(program: &str, args: &[&str]) -> String {
    Command::new(program)
        .args(args)
        .output()
        .ok()
        .filter(|output| output.status.success())
        .and_then(|output| String::from_utf8(output.stdout).ok())
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "unavailable".to_string())
}

fn cpu_model() -> String {
    std::fs::read_to_string("/proc/cpuinfo")
        .ok()
        .and_then(|contents| {
            contents.lines().find_map(|line| {
                line.strip_prefix("model name")
                    .and_then(|value| value.split_once(':'))
                    .map(|(_, value)| value.trim().to_string())
            })
        })
        .unwrap_or_else(|| "unavailable".to_string())
}

fn thermal_policy() -> String {
    std::fs::read_to_string("/sys/devices/system/cpu/cpu0/cpufreq/scaling_governor")
        .map(|value| value.trim().to_string())
        .unwrap_or_else(|_| "unavailable".to_string())
}
