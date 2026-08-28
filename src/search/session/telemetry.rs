use std::time::Duration;

use crate::benchmark_report::{SourceReport, StageTimings, Waits};

#[derive(Debug, Default)]
struct BackendUsage {
    accelerator_chunks: u64,
    cpu_chunks: u64,
    fallback_count: u64,
    fallback_reason: String,
    gpu_submissions: u64,
    gpu_completions: u64,
    gpu_buffer_creations: u64,
    gpu_bind_group_creations: u64,
    gpu_resource_reuses: u64,
    gpu_overlap: Duration,
    gpu_max_in_flight: u64,
    gpu_overlap_events: u64,
    gpu_test_only_mock: bool,
    gpu_duty: crate::performance::GpuDutyMetrics,
}

#[derive(Debug, Default)]
struct Durations {
    read: Duration,
    parse: Duration,
    queue_wait: Duration,
    backend_compute: Duration,
    gpu_allocation: Duration,
    gpu_upload: Duration,
    gpu_dispatch: Duration,
    gpu_readback_map: Duration,
    reduction: Duration,
    persistence: Duration,
    generation_wait: Duration,
    throttle_wait: Duration,
    source_wait: Duration,
    cache_hit: Duration,
}

#[derive(Debug, Default)]
struct SourceUsage {
    reader_pool_size: u64,
    reader_open_count: u64,
    reader_reuse_count: u64,
}

#[derive(Debug)]
pub(crate) struct SessionTelemetry {
    enabled: bool,
    durations: Durations,
    backend: BackendUsage,
    source: SourceUsage,
}

impl SessionTelemetry {
    pub(crate) fn new(enabled: bool) -> Self {
        Self {
            enabled,
            durations: Durations::default(),
            backend: BackendUsage::default(),
            source: SourceUsage::default(),
        }
    }

    pub(crate) const fn enabled(&self) -> bool {
        self.enabled
    }

    pub(crate) fn record_read(&mut self, duration: Duration) {
        if self.enabled {
            self.durations.read += duration;
        }
    }

    pub(crate) fn record_parse(&mut self, duration: Duration) {
        if self.enabled {
            self.durations.parse += duration;
        }
    }

    pub(crate) fn record_cache_hit(&mut self, duration: Duration) {
        if self.enabled {
            self.durations.cache_hit += duration;
        }
    }

    pub(crate) fn record_source_pool(
        &mut self,
        telemetry: super::source_reader::ReaderPoolTelemetry,
    ) {
        self.source.reader_pool_size = telemetry.reader_pool_size;
        self.source.reader_open_count = telemetry.reader_open_count;
        self.source.reader_reuse_count = telemetry.reader_reuse_count;
    }

    pub(crate) fn record_backend_compute(&mut self, duration: Duration) {
        if self.enabled {
            self.durations.backend_compute += duration;
        }
    }

    pub(crate) fn record_gpu_stages(&mut self, telemetry: &crate::gpu::GpuChunkTelemetry) {
        if self.enabled {
            self.durations.gpu_allocation += telemetry.allocation;
            self.durations.gpu_upload += telemetry.upload;
            self.durations.gpu_dispatch += telemetry.dispatch;
            self.durations.gpu_readback_map += telemetry.readback_map;
        }
        self.backend.gpu_submissions = self
            .backend
            .gpu_submissions
            .saturating_add(telemetry.submissions);
        self.backend.gpu_completions = self
            .backend
            .gpu_completions
            .saturating_add(telemetry.completions);
        self.backend.gpu_buffer_creations = self
            .backend
            .gpu_buffer_creations
            .saturating_add(telemetry.buffer_creations);
        self.backend.gpu_bind_group_creations = self
            .backend
            .gpu_bind_group_creations
            .saturating_add(telemetry.bind_group_creations);
        self.backend.gpu_resource_reuses = self
            .backend
            .gpu_resource_reuses
            .saturating_add(telemetry.resource_reuses);
        self.backend.gpu_overlap += telemetry.overlap;
        self.backend.gpu_max_in_flight =
            self.backend.gpu_max_in_flight.max(telemetry.max_in_flight);
        self.backend.gpu_overlap_events = self
            .backend
            .gpu_overlap_events
            .saturating_add(telemetry.overlap_events);
        self.backend.gpu_test_only_mock |= telemetry.test_only_mock;
    }

    pub(crate) fn record_gpu_duty(&mut self, metrics: crate::performance::GpuDutyMetrics) {
        self.backend.gpu_duty = metrics;
        if self.enabled {
            self.durations.throttle_wait += metrics.wait;
        }
    }

    pub(crate) fn record_reduction(&mut self, duration: Duration) {
        if self.enabled {
            self.durations.reduction += duration;
        }
    }

    pub(crate) fn record_persistence(&mut self, duration: Duration) {
        if self.enabled {
            self.durations.persistence += duration;
        }
    }

    pub(crate) fn record_generator_wait(&mut self, duration: Duration) {
        if self.enabled {
            self.durations.generation_wait += duration;
        }
    }

    pub(crate) fn record_source_wait(&mut self, duration: Duration) {
        if self.enabled {
            self.durations.source_wait += duration;
        }
    }

    pub(crate) fn record_queue_wait(&mut self, duration: Duration) {
        if self.enabled {
            self.durations.queue_wait += duration;
        }
    }

    pub(crate) fn record_throttle_wait(&mut self, duration: Duration) {
        if self.enabled {
            self.durations.throttle_wait += duration;
        }
    }

    pub(crate) fn record_cpu_chunk(&mut self) {
        self.backend.cpu_chunks = self.backend.cpu_chunks.saturating_add(1);
    }

    pub(crate) fn record_accelerator_chunk(&mut self, completions: u64) {
        self.backend.accelerator_chunks = self.backend.accelerator_chunks.saturating_add(1);
        if completions == 0 {
            self.backend.gpu_submissions = self.backend.gpu_submissions.saturating_add(1);
            self.backend.gpu_completions = self.backend.gpu_completions.saturating_add(1);
        }
    }

    pub(crate) fn record_fallback(&mut self, reason: String) {
        self.backend.fallback_count = self.backend.fallback_count.saturating_add(1);
        if self.backend.fallback_reason.is_empty() {
            self.backend.fallback_reason = reason;
        }
    }

    pub(crate) fn stages(&self) -> StageTimings {
        StageTimings {
            read_ms: duration_ms(self.durations.read),
            parse_ms: duration_ms(self.durations.parse),
            queue_wait_ms: queue_duration_ms(self.durations.queue_wait),
            backend_compute_ms: duration_ms(self.durations.backend_compute),
            gpu_allocation_ms: duration_ms(self.durations.gpu_allocation),
            gpu_upload_ms: duration_ms(self.durations.gpu_upload),
            gpu_dispatch_ms: duration_ms(self.durations.gpu_dispatch),
            gpu_readback_map_ms: duration_ms(self.durations.gpu_readback_map),
            reduction_ms: duration_ms(self.durations.reduction),
            persistence_ms: duration_ms(self.durations.persistence),
            generation_wait_ms: duration_ms(self.durations.generation_wait),
            throttle_wait_ms: duration_ms(self.durations.throttle_wait),
        }
    }

    pub(crate) fn waits(&self) -> Waits {
        Waits {
            source_ms: duration_ms(self.durations.source_wait),
            queue_ms: queue_duration_ms(self.durations.queue_wait),
            generator_ms: duration_ms(self.durations.generation_wait),
            throttle_ms: duration_ms(self.durations.throttle_wait),
        }
    }

    pub(crate) fn source_report(&self) -> SourceReport {
        SourceReport {
            reader_pool_size: self.source.reader_pool_size,
            reader_open_count: self.source.reader_open_count,
            reader_reuse_count: self.source.reader_reuse_count,
            cache_hit_ms: duration_ms(self.durations.cache_hit),
        }
    }

    pub(crate) fn resolved_backend(&self, active_backend: &str) -> String {
        let reported_backend = if active_backend == "gpu" {
            "wgpu"
        } else {
            active_backend
        };
        match (
            self.backend.accelerator_chunks > 0,
            self.backend.cpu_chunks > 0,
        ) {
            (true, true) => "mixed".to_string(),
            (true, false) => reported_backend.to_string(),
            (false, true) => "cpu".to_string(),
            (false, false) => reported_backend.to_string(),
        }
    }

    pub(crate) const fn fallback_count(&self) -> u64 {
        self.backend.fallback_count
    }

    pub(crate) fn fallback_reason(&self) -> String {
        self.backend.fallback_reason.clone()
    }

    pub(crate) const fn gpu_submissions(&self) -> u64 {
        self.backend.gpu_submissions
    }

    pub(crate) const fn gpu_completions(&self) -> u64 {
        self.backend.gpu_completions
    }

    pub(crate) const fn gpu_buffer_creations(&self) -> u64 {
        self.backend.gpu_buffer_creations
    }

    pub(crate) const fn gpu_bind_group_creations(&self) -> u64 {
        self.backend.gpu_bind_group_creations
    }

    pub(crate) const fn gpu_resource_reuses(&self) -> u64 {
        self.backend.gpu_resource_reuses
    }

    pub(crate) const fn gpu_overlap(&self) -> Duration {
        self.backend.gpu_overlap
    }

    pub(crate) const fn gpu_max_in_flight(&self) -> u64 {
        self.backend.gpu_max_in_flight
    }

    pub(crate) const fn gpu_overlap_events(&self) -> u64 {
        self.backend.gpu_overlap_events
    }

    pub(crate) const fn gpu_test_only_mock(&self) -> bool {
        self.backend.gpu_test_only_mock
    }

    pub(crate) const fn gpu_duty(&self) -> crate::performance::GpuDutyMetrics {
        self.backend.gpu_duty
    }
}

fn duration_ms(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

fn queue_duration_ms(duration: Duration) -> u64 {
    if duration.is_zero() {
        0
    } else {
        duration_ms(duration).max(1)
    }
}
