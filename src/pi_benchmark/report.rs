use serde::Serialize;

use crate::benchmark_contract::WorkloadIdentity;
use crate::pi::UnavailableGenerator;

#[derive(Clone, Debug, Default, Serialize)]
pub(crate) struct PiMetrics {
    pub generated_source_digits_per_second: f64,
    pub generation_wait_ms: u64,
    pub scanned_windows_per_second: f64,
}

#[derive(Clone, Debug, Default, Serialize)]
pub(crate) struct PiMemory {
    pub logical_peak_mb: f64,
    pub rss_peak_mb: f64,
    pub rss_baseline_mb: f64,
    pub rss_margin_mb: f64,
    pub gpu_vram_status: String,
    pub gpu_vram_baseline_mb: f64,
    pub gpu_vram_margin_mb: f64,
    pub gpu_vram_peak_mb: f64,
}

#[derive(Clone, Debug, Default, Serialize)]
pub(crate) struct PiBenchmarkRun {
    pub generated_source_digits: u64,
    pub generated_source_digits_per_second: f64,
    pub scanned_windows_per_second: f64,
    pub generation_wait_ms: u64,
    pub overlap_wait_ms: u64,
    pub cache_write_ms: u64,
    pub producer_epochs: u64,
    pub coalesced_request_count: u64,
    pub generation_batches: u64,
    pub chudnovsky_target_computations: u64,
    pub recomputed_source_digits: u64,
    pub skipped_source_digits: u64,
    pub concurrent_requests: u64,
    pub search_work_windows: u64,
    pub correctness: bool,
    pub memory: PiMemory,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct PiBenchmarkReport {
    pub schema_version: u32,
    pub status: String,
    pub reason: String,
    pub targets: Vec<u64>,
    pub demand_mode: String,
    pub concurrent_requests: u64,
    pub search_work_windows: u64,
    pub generation_wait_ms: u64,
    pub overlap_wait_ms: u64,
    pub cache_write_ms: u64,
    pub producer_epochs: u64,
    pub coalesced_request_count: u64,
    pub generation_batches: u64,
    pub chudnovsky_target_computations: u64,
    pub recomputed_source_digits: u64,
    pub skipped_source_digits: u64,
    pub cpu_workers: usize,
    pub correctness: bool,
    pub median: PiMetrics,
    pub p95: PiMetrics,
    pub memory: PiMemory,
    pub requested_generator_backend: String,
    pub selected_backend: String,
    pub selected_variant: String,
    pub generator_executable_sha256: String,
    pub fallback: bool,
    pub fallback_reason: String,
    pub unavailable_backends: Vec<UnavailableGenerator>,
    pub workload_id: String,
    pub workload_identity: WorkloadIdentity,
    pub raw_runs: Vec<PiBenchmarkRun>,
}

pub(crate) fn nearest_rank_f64(
    runs: &[PiBenchmarkRun],
    percentile: usize,
    field: impl Fn(&PiBenchmarkRun) -> f64,
) -> f64 {
    let mut values: Vec<_> = runs.iter().map(field).collect();
    values.sort_by(|left, right| left.total_cmp(right));
    values[nearest_rank_index(values.len(), percentile)]
}

pub(crate) fn nearest_rank_u64(
    runs: &[PiBenchmarkRun],
    percentile: usize,
    field: impl Fn(&PiBenchmarkRun) -> u64,
) -> u64 {
    let mut values: Vec<_> = runs.iter().map(field).collect();
    values.sort_unstable();
    values[nearest_rank_index(values.len(), percentile)]
}

fn nearest_rank_index(length: usize, percentile: usize) -> usize {
    length
        .saturating_mul(percentile)
        .div_ceil(100)
        .saturating_sub(1)
        .min(length.saturating_sub(1))
}
