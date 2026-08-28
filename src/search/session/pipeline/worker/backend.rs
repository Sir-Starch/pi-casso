use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, SyncSender};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use anyhow::{Result, anyhow};

use crate::art::Bitmap;
use crate::gpu::GpuChunkTelemetry;
use crate::performance::{GpuDutyMetrics, GpuDutyPolicy, GpuMode, SearchBackendChoice};
use crate::search::backend::{SearchBackend, SearchChunkParams, create_search_backend};
use crate::search::scoring::EmergencePlan;
use crate::search::session::resource_budget::ResourceBudget;
use crate::search::types::{MatchMode, SearchOptions, WindowScore};

use super::producer::SourceChunk;

pub(crate) struct BackendConfig {
    pub target: Arc<Bitmap>,
    pub mode: MatchMode,
    pub canvas_width: usize,
    pub canvas_height: usize,
    pub threshold: u8,
    pub invert: bool,
    pub window_len: usize,
    pub gpu_utilization: u8,
    pub fallback_options: SearchOptions,
}

pub(crate) struct BackendResult {
    pub chunk: SourceChunk,
    pub scores: Result<Vec<WindowScore>>,
    pub processing: Duration,
    pub gpu: GpuChunkTelemetry,
    pub used_accelerator: bool,
    pub fallback_reason: Option<String>,
}

pub(crate) struct BackendSummary {
    pub active_backend: String,
    pub gpu_status: String,
    pub gpu_duty: GpuDutyMetrics,
}

pub(crate) type ActiveBackend = Arc<Mutex<Box<dyn SearchBackend>>>;

pub(crate) struct SharedBackend {
    active: ActiveBackend,
}

impl SharedBackend {
    pub(crate) fn new(active: ActiveBackend) -> Self {
        Self { active }
    }
}

impl SearchBackend for SharedBackend {
    fn name(&self) -> &'static str {
        lock_backend(&self.active).map_or("unavailable", |backend| backend.name())
    }

    fn gpu_status(&self) -> String {
        lock_backend(&self.active).map_or_else(
            |error| format!("unavailable: {error:#}"),
            |backend| backend.gpu_status(),
        )
    }

    fn cpu_worker_width(&self) -> usize {
        lock_backend(&self.active).map_or(1, |backend| backend.cpu_worker_width())
    }

    fn gpu_ring_depth(&self) -> usize {
        lock_backend(&self.active).map_or(0, |backend| backend.gpu_ring_depth())
    }

    fn search_chunk(
        &mut self,
        digits: &[u8],
        actual_windows: usize,
        params: &SearchChunkParams<'_>,
    ) -> Result<Vec<WindowScore>> {
        lock_backend(&self.active)?.search_chunk(digits, actual_windows, params)
    }

    fn reconfigure(&mut self, workers: usize) -> Result<()> {
        lock_backend(&self.active)?.reconfigure(workers)
    }
}

fn lock_backend(backend: &ActiveBackend) -> Result<MutexGuard<'_, Box<dyn SearchBackend>>> {
    backend
        .lock()
        .map_err(|_| anyhow!("active search backend was poisoned"))
}

pub(crate) fn run_backend(
    rx: Receiver<SourceChunk>,
    tx: SyncSender<BackendResult>,
    backend: ActiveBackend,
    config: BackendConfig,
    budget: Arc<ResourceBudget>,
    stop: Arc<AtomicBool>,
) -> Result<BackendSummary> {
    let synthetic_backend = crate::gpu_ring::test_mode_enabled()
        .then(|| std::env::var("PI_CASSO_TEST_BACKEND_FAIL_AFTER_PREFLIGHT").ok())
        .flatten();
    let mut synthetic_chunks = 0_u64;
    let mut gpu_duty = GpuDutyPolicy::new(config.gpu_utilization);

    while let Ok(chunk) = rx.recv() {
        if stop.load(Ordering::SeqCst) {
            break;
        }
        let mut active = lock_backend(&backend)?;
        let backend_name = active.name();
        let cpu_worker_width = active.cpu_worker_width();
        let gpu_ring_depth = active.gpu_ring_depth();
        let uses_accelerator = matches!(backend_name, "gpu" | "cuda");
        let runtime_fault_backend = if backend_name == "gpu" {
            "wgpu"
        } else {
            backend_name
        };
        let injected_runtime_fault = uses_accelerator
            && crate::gpu_ring::test_runtime_fault_for(runtime_fault_backend)
            && crate::gpu_ring::test_backend_mock_enabled(runtime_fault_backend);
        let synthetic = synthetic_backend.as_deref().is_some_and(|value| {
            (value == backend_name || (value == "wgpu" && backend_name == "gpu"))
                && crate::gpu_ring::test_backend_mock_enabled(value)
        });
        let synthetic_accelerator = synthetic && synthetic_chunks == 0;
        let synthetic_cpu_fallback = synthetic && synthetic_chunks == 1;
        synthetic_chunks = synthetic_chunks.saturating_add(1);
        let target = Arc::clone(&config.target);
        let emergence_plan = if config.mode == MatchMode::Emergence {
            Some(EmergencePlan::new(
                &target,
                config.canvas_width,
                config.canvas_height,
            ))
        } else {
            None
        };
        let params = SearchChunkParams {
            target: &target,
            mode: config.mode,
            canvas_width: config.canvas_width,
            canvas_height: config.canvas_height,
            threshold: config.threshold,
            invert: config.invert,
            window_len: config.window_len,
            emergence_plan: emergence_plan.as_ref(),
        };
        if uses_accelerator {
            gpu_duty.wait_before_submission();
        }
        let mut gpu_leases = Vec::with_capacity(gpu_ring_depth);
        if uses_accelerator {
            for _ in 0..gpu_ring_depth {
                let (lease, _) = budget.acquire_gpu()?;
                gpu_leases.push(lease);
            }
        }
        let cpu_lease = cpu_permits_for_backend(backend_name, cpu_worker_width)
            .map(|permits| budget.acquire_cpu(permits))
            .transpose()?
            .map(|(lease, _)| lease);
        let started = Instant::now();
        let _ = crate::gpu::take_chunk_telemetry();
        let initial = if synthetic_cpu_fallback {
            Err(anyhow!(
                "synthetic {} backend failure",
                synthetic_backend.as_deref().unwrap_or(backend_name)
            ))
        } else {
            active.search_chunk(&chunk.digits, chunk.actual_windows, &params)
        };
        drop(active);
        let gpu = crate::gpu::take_chunk_telemetry();
        drop(cpu_lease);
        drop(gpu_leases);
        let processing = started.elapsed();
        if uses_accelerator {
            gpu_duty.record_submission(processing);
        }
        match initial {
            Ok(scores) => {
                if tx
                    .send(BackendResult {
                        chunk,
                        scores: Ok(scores),
                        processing,
                        gpu,
                        used_accelerator: matches!(backend_name, "gpu" | "cuda")
                            || synthetic_accelerator,
                        fallback_reason: None,
                    })
                    .is_err()
                {
                    break;
                }
            }
            Err(error) if injected_runtime_fault => {
                let result = BackendResult {
                    chunk,
                    scores: Err(error),
                    processing,
                    gpu,
                    used_accelerator: true,
                    fallback_reason: None,
                };
                let _ = tx.send(result);
                break;
            }
            Err(error) if backend_name != "cpu" || synthetic_cpu_fallback => {
                let reason = format!("{error:#}");
                let mut fallback_options = config.fallback_options.clone();
                fallback_options.performance.backend = SearchBackendChoice::Cpu;
                fallback_options.performance.gpu = GpuMode::Off;
                fallback_options.performance.limits.cpu_workers = cpu_worker_width;
                let fallback_backend =
                    create_search_backend(&fallback_options, &config.target, budget.as_ref())?;
                let fallback_width = fallback_backend.cpu_worker_width();
                *lock_backend(&backend)? = fallback_backend;
                let (fallback_lease, _) = budget.acquire_cpu(fallback_width)?;
                let fallback_started = Instant::now();
                let fallback = lock_backend(&backend)?.search_chunk(
                    &chunk.digits,
                    chunk.actual_windows,
                    &params,
                );
                drop(fallback_lease);
                let total_processing = processing.saturating_add(fallback_started.elapsed());
                let result = BackendResult {
                    chunk,
                    scores: fallback,
                    processing: total_processing,
                    gpu,
                    used_accelerator: synthetic_accelerator,
                    fallback_reason: Some(reason),
                };
                let failed = result.scores.is_err();
                if tx.send(result).is_err() || failed {
                    break;
                }
            }
            Err(error) => {
                let result = BackendResult {
                    chunk,
                    scores: Err(error),
                    processing,
                    gpu,
                    used_accelerator: false,
                    fallback_reason: None,
                };
                let _ = tx.send(result);
                break;
            }
        }
    }

    let backend = lock_backend(&backend)?;
    Ok(BackendSummary {
        active_backend: backend.name().to_string(),
        gpu_status: backend.gpu_status(),
        gpu_duty: gpu_duty.metrics(),
    })
}

const fn cpu_permits_for_backend(backend_name: &str, cpu_worker_width: usize) -> Option<usize> {
    match backend_name.as_bytes() {
        b"gpu" | b"cuda" => None,
        _ => Some(cpu_worker_width),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct RecordingBackend {
        workers: usize,
    }

    impl SearchBackend for RecordingBackend {
        fn name(&self) -> &'static str {
            "recording"
        }

        fn gpu_status(&self) -> String {
            "disabled".to_string()
        }

        fn cpu_worker_width(&self) -> usize {
            self.workers
        }

        fn search_chunk(
            &mut self,
            _digits: &[u8],
            _actual_windows: usize,
            _params: &SearchChunkParams<'_>,
        ) -> Result<Vec<WindowScore>> {
            Ok(Vec::new())
        }

        fn reconfigure(&mut self, workers: usize) -> Result<()> {
            self.workers = workers;
            Ok(())
        }
    }

    #[test]
    fn live_reconfiguration_reaches_the_worker_owned_backend() {
        // Given: a backend shared by the pipeline worker and session control proxy.
        let active = Arc::new(Mutex::new(
            Box::new(RecordingBackend { workers: 1 }) as Box<dyn SearchBackend>
        ));
        let mut controls = SharedBackend::new(Arc::clone(&active));

        // When: a live worker-count control is applied through the session backend.
        controls.reconfigure(3).expect("live reconfiguration");

        // Then: the exact backend used by the worker observes the new width.
        assert_eq!(active.lock().expect("active backend").cpu_worker_width(), 3);
    }

    #[test]
    fn accelerator_wait_does_not_reserve_cpu_permits() {
        // Given: CPU and accelerator backend identities with the same fallback width.
        // When: the worker chooses the lease required around backend execution.
        // Then: CPU work is bounded, while accelerator waiting leaves CPU capacity available.
        assert_eq!(cpu_permits_for_backend("cpu", 4), Some(4));
        assert_eq!(cpu_permits_for_backend("gpu", 4), None);
        assert_eq!(cpu_permits_for_backend("cuda", 4), None);
    }
}
