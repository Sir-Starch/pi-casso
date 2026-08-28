use anyhow::Result;

use crate::benchmark_report::{
    MemoryReport, QueueReport, ReducerReport, SourceReport, StageTimings, Waits,
};
use crate::search::{FinishReason, SearchReporter, SearchSnapshot};
use crate::storage::BestEventRecord;

#[derive(Default)]
pub(super) struct FinishReporter {
    pub(super) reason: Option<FinishReason>,
    pub(super) telemetry: Option<FinalTelemetry>,
}

#[derive(Default)]
pub(super) struct FinalTelemetry {
    pub(super) resolved_backend: String,
    pub(super) backend_device: String,
    pub(super) backend_feature_available: bool,
    pub(super) backend_fault_status: String,
    pub(super) fallback: bool,
    pub(super) fallback_reason: String,
    pub(super) fallback_count: u64,
    pub(super) stage_timings: StageTimings,
    pub(super) waits: Waits,
    pub(super) source: SourceReport,
    pub(super) queue: QueueReport,
    pub(super) memory: MemoryReport,
    pub(super) reducer: ReducerReport,
    pub(super) cpu_permits_in_use: u64,
    pub(super) cpu_permits_peak: u64,
    pub(super) cpu_permits_max: u64,
    pub(super) source_lag_digits: u64,
    pub(super) generator_digits_per_second: f64,
    pub(super) gpu_submissions: u64,
    pub(super) gpu_completions: u64,
    pub(super) gpu_buffer_creations: u64,
    pub(super) gpu_bind_group_creations: u64,
    pub(super) gpu_resource_reuses: u64,
    pub(super) gpu_overlap_ms: u64,
    pub(super) gpu_max_in_flight: u64,
    pub(super) gpu_overlap_events: u64,
    pub(super) gpu_test_only_mock: bool,
    pub(super) gpu_duty_wait_ms: u64,
    pub(super) gpu_initial_submission_wait_ms: u64,
    pub(super) active_submission_ratio: f64,
    pub(super) dispatch_quantum_ratio: f64,
    pub(super) telemetry_enabled: bool,
}

impl SearchReporter for FinishReporter {
    fn on_update(&mut self, _snapshot: &SearchSnapshot) -> Result<()> {
        Ok(())
    }

    fn on_new_best(&mut self, _snapshot: &SearchSnapshot, _event: &BestEventRecord) -> Result<()> {
        Ok(())
    }

    fn on_finish(&mut self, snapshot: &SearchSnapshot, reason: FinishReason) -> Result<()> {
        self.reason = Some(reason);
        self.telemetry = Some(FinalTelemetry {
            resolved_backend: snapshot.metrics.resolved_backend.clone(),
            backend_device: snapshot.metrics.backend_device.clone(),
            backend_feature_available: snapshot.metrics.backend_feature_available,
            backend_fault_status: snapshot.metrics.backend_fault_status.clone(),
            fallback: snapshot.metrics.fallback,
            fallback_reason: snapshot.metrics.fallback_reason.clone(),
            fallback_count: snapshot.metrics.fallback_count,
            stage_timings: snapshot.metrics.stage_timings.clone(),
            waits: snapshot.metrics.waits.clone(),
            source: snapshot.metrics.source.clone(),
            queue: snapshot.metrics.queue.clone(),
            memory: snapshot.metrics.memory.clone(),
            reducer: snapshot.metrics.reducer.clone(),
            cpu_permits_in_use: snapshot.metrics.cpu_permits_in_use,
            cpu_permits_peak: snapshot.metrics.cpu_permits_peak,
            cpu_permits_max: snapshot.metrics.cpu_permits_max,
            source_lag_digits: snapshot.cache_gap_digits,
            generator_digits_per_second: snapshot.metrics.generator_digits_per_second,
            gpu_submissions: snapshot.metrics.gpu_submissions,
            gpu_completions: snapshot.metrics.gpu_completions,
            gpu_buffer_creations: snapshot.metrics.gpu_buffer_creations,
            gpu_bind_group_creations: snapshot.metrics.gpu_bind_group_creations,
            gpu_resource_reuses: snapshot.metrics.gpu_resource_reuses,
            gpu_overlap_ms: snapshot.metrics.gpu_overlap_ms,
            gpu_max_in_flight: snapshot.metrics.gpu_max_in_flight,
            gpu_overlap_events: snapshot.metrics.gpu_overlap_events,
            gpu_test_only_mock: snapshot.metrics.gpu_test_only_mock,
            gpu_duty_wait_ms: snapshot.metrics.gpu_duty_wait_ms,
            gpu_initial_submission_wait_ms: snapshot.metrics.gpu_initial_submission_wait_ms,
            active_submission_ratio: snapshot.metrics.active_submission_ratio,
            dispatch_quantum_ratio: snapshot.metrics.dispatch_quantum_ratio,
            telemetry_enabled: snapshot.metrics.telemetry_enabled,
        });
        Ok(())
    }
}
