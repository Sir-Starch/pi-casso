use serde::{Deserialize, Serialize};

use crate::benchmark_contract::{BackendCandidate, WorkloadIdentity};
use crate::capability::GpuCapability;
use crate::pi::UnavailableGenerator;

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct StageTimings {
    pub read_ms: u64,
    pub parse_ms: u64,
    pub queue_wait_ms: u64,
    pub backend_compute_ms: u64,
    pub gpu_allocation_ms: u64,
    pub gpu_upload_ms: u64,
    pub gpu_dispatch_ms: u64,
    pub gpu_readback_map_ms: u64,
    pub reduction_ms: u64,
    pub persistence_ms: u64,
    pub generation_wait_ms: u64,
    pub throttle_wait_ms: u64,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct Waits {
    pub source_ms: u64,
    pub queue_ms: u64,
    pub generator_ms: u64,
    pub throttle_ms: u64,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct MemoryReport {
    pub logical_reserved_mb: f64,
    pub logical_peak_mb: f64,
    pub logical_budget_mb: f64,
    pub logical_reserved_bytes: u64,
    pub logical_peak_bytes: u64,
    pub logical_budget_bytes: u64,
    pub rss_peak_mb: f64,
    pub rss_baseline_mb: f64,
    pub rss_margin_mb: f64,
    pub gpu_vram_status: String,
    pub gpu_vram_baseline_mb: f64,
    pub gpu_vram_margin_mb: f64,
    pub gpu_vram_peak_mb: f64,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct SourceReport {
    pub reader_pool_size: u64,
    pub reader_open_count: u64,
    pub reader_reuse_count: u64,
    pub cache_hit_ms: u64,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct QueueReport {
    pub current_occupancy: u64,
    pub max_occupancy: u64,
    pub permits: u64,
    pub global_limit: u64,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct ReducerReport {
    pub ordered: bool,
    pub contiguous_completed_offsets: u64,
    pub max_reorder_depth: u64,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct GpuReport {
    pub kernel_arch: String,
    pub kernel_sha256: String,
    pub kernel_source_sha256: String,
    pub buffer_creations: u64,
    pub bind_group_creations: u64,
    pub resource_reuses: u64,
    pub overlap_ms: u64,
    pub submissions: u64,
    pub completions: u64,
    pub fallback_count: u64,
    pub max_in_flight: u64,
    pub overlap_events: u64,
    pub test_only_mock: bool,
    pub capability: Option<GpuCapability>,
}

#[derive(Clone, Debug, Serialize)]
pub struct BenchmarkConfig {
    pub profile: String,
    pub cpu_workers: usize,
    pub cpu_utilization: u8,
    pub gpu_utilization: u8,
    pub chunk_size: usize,
    pub queue_depth: usize,
    pub memory_limit_mb: usize,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct AggregateMetrics {
    pub scanned_windows_per_second: f64,
    pub source_digits_per_second: f64,
    pub logical_window_digits_per_second: f64,
    pub elapsed_seconds: f64,
    pub overlap_wait_ms: u64,
    pub cache_write_ms: u64,
    pub generation_wait_ms: u64,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct RepetitionReport {
    pub schema_version: u32,
    pub status: String,
    pub repetition: u32,
    pub cache_instance_id: String,
    pub cache_reset: bool,
    pub warm_up_completed: bool,
    pub first_published_digits: u64,
    pub scanned_windows: u64,
    pub source_digits_read: u64,
    pub logical_window_digits: u64,
    pub scanned_windows_per_second: f64,
    pub source_digits_per_second: f64,
    pub logical_window_digits_per_second: f64,
    pub elapsed_seconds: f64,
    pub stop_reason: String,
    pub resolved_backend: String,
    pub backend_device: String,
    pub backend_feature_available: bool,
    pub backend_fault_status: String,
    pub fallback: bool,
    pub fallback_reason: String,
    pub fallback_count: u64,
    pub stage_timings: StageTimings,
    pub waits: Waits,
    #[serde(skip_serializing)]
    pub source: SourceReport,
    pub overlap_wait_ms: u64,
    pub cache_write_ms: u64,
    pub producer_epochs: u64,
    pub coalesced_request_count: u64,
    pub generation_batches: u64,
    pub event_wake_latency_ms: u64,
    pub lead_digits: u64,
    pub high_water_digits: u64,
    pub source_lag_digits: u64,
    pub generator_digits_per_second: f64,
    pub gpu_submissions: u64,
    pub gpu_completions: u64,
    pub gpu_buffer_creations: u64,
    pub gpu_bind_group_creations: u64,
    pub gpu_resource_reuses: u64,
    pub gpu_overlap_ms: u64,
    pub gpu_max_in_flight: u64,
    pub gpu_overlap_events: u64,
    pub gpu_test_only_mock: bool,
    pub gpu_duty_wait_ms: u64,
    pub gpu_initial_submission_wait_ms: u64,
    pub active_submission_ratio: f64,
    pub dispatch_quantum_ratio: f64,
    pub telemetry_enabled: bool,
    pub best_score: f64,
    pub queue: QueueReport,
    pub memory: MemoryReport,
    pub reducer: ReducerReport,
    pub cpu_permits_in_use: u64,
    pub cpu_permits_peak: u64,
    pub cpu_permits_max: u64,
}

impl RepetitionReport {
    pub fn aggregate(&self) -> AggregateMetrics {
        AggregateMetrics {
            scanned_windows_per_second: self.scanned_windows_per_second,
            source_digits_per_second: self.source_digits_per_second,
            logical_window_digits_per_second: self.logical_window_digits_per_second,
            elapsed_seconds: self.elapsed_seconds,
            overlap_wait_ms: self.overlap_wait_ms,
            cache_write_ms: self.cache_write_ms,
            generation_wait_ms: self.stage_timings.generation_wait_ms,
        }
    }
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct MachineIdentity {
    pub os: String,
    pub cpu: String,
    pub gpu: String,
    pub driver: String,
    pub rustc: String,
    pub power_policy: String,
    pub thermal_policy: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct BenchmarkReport {
    pub schema_version: u32,
    pub status: String,
    pub reason: String,
    pub skip_reason: String,
    pub requested_backend: String,
    pub requested_generator_backend: String,
    pub selected_variant: String,
    pub generator_executable_sha256: String,
    pub unavailable_backends: Vec<UnavailableGenerator>,
    pub resolved_backend: Option<String>,
    pub backend_fault_status: String,
    pub fallback: bool,
    pub fallback_reason: String,
    pub fallback_count: u64,
    pub backend_device: String,
    pub backend_feature_available: bool,
    pub auto_min_work_windows: u64,
    pub backend_candidates: Vec<BackendCandidate>,
    pub workload_id: String,
    pub workload_identity: WorkloadIdentity,
    pub source_mode: String,
    pub cache_state: String,
    pub cache_reset: bool,
    pub cache_instance_id: String,
    pub warm_up_completed: bool,
    pub page_cache_control: String,
    pub start_offset: u64,
    pub effective_end: u64,
    pub source_end_exclusive: u64,
    pub window_len: usize,
    pub scanned_windows: u64,
    pub scanned_windows_per_second: f64,
    pub source_digits_per_second: f64,
    pub logical_window_digits_per_second: f64,
    pub elapsed_seconds: f64,
    pub stop_reason: String,
    pub best_score: f64,
    pub repetitions: u32,
    pub warmup: u32,
    pub median: AggregateMetrics,
    pub p95: AggregateMetrics,
    pub stage_timings: StageTimings,
    pub waits: Waits,
    pub overlap_wait_ms: u64,
    pub cache_write_ms: u64,
    pub producer_epochs: u64,
    pub coalesced_request_count: u64,
    pub generation_batches: u64,
    pub event_wake_latency_ms: u64,
    pub lead_digits: u64,
    pub high_water_digits: u64,
    pub source_lag_digits: u64,
    pub generator_digits_per_second: f64,
    pub telemetry_enabled: bool,
    pub test_only_mock: bool,
    pub config: BenchmarkConfig,
    pub memory: MemoryReport,
    pub source: SourceReport,
    pub queue: QueueReport,
    pub gpu: GpuReport,
    pub reducer: ReducerReport,
    pub cpu_permits_in_use: u64,
    pub cpu_permits_peak: u64,
    pub cpu_permits_max: u64,
    pub gpu_duty_policy_percent: u8,
    pub gpu_duty_window_ms: u64,
    pub gpu_duty_wait_ms: u64,
    pub gpu_initial_submission_wait_ms: u64,
    pub active_submission_ratio: f64,
    pub dispatch_quantum_ratio: f64,
    pub raw_run_paths: Vec<String>,
    pub raw_runs: Vec<RepetitionReport>,
    pub git_sha: String,
    pub machine: MachineIdentity,
}
