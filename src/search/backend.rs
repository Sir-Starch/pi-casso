//! Compute backends. A backend takes a slab of digits and returns one score per
//! window; whether it does that on the CPU thread pool or on the GPU is an
//! implementation detail the engine never has to know about.

use std::sync::Arc;

use anyhow::{Result, anyhow, bail};
use rayon::prelude::*;

use crate::art::Bitmap;
use crate::gpu::{GpuSearchEngine, GpuWindowScore};
use crate::performance::{GpuMode, PerformanceProfile, SearchBackendChoice};
use crate::search::scoring::{EmergencePlan, quantize_score, score_candidate_window};
use crate::search::session::resource_budget::ResourceBudget;
use crate::search::types::{EmergenceStatistics, MatchMode, SearchOptions, WindowScore};

const MAX_ACCELERATOR_CHUNK_WINDOWS: usize = 262_144;

pub(crate) struct SearchChunkParams<'a> {
    pub(crate) target: &'a Bitmap,
    pub(crate) mode: MatchMode,
    pub(crate) canvas_width: usize,
    pub(crate) canvas_height: usize,
    pub(crate) threshold: u8,
    pub(crate) invert: bool,
    pub(crate) window_len: usize,
    pub(crate) emergence_plan: Option<&'a EmergencePlan>,
}

pub(crate) trait SearchBackend: Send {
    fn name(&self) -> &'static str;
    fn gpu_status(&self) -> String;
    fn cpu_worker_width(&self) -> usize;
    fn gpu_ring_depth(&self) -> usize {
        0
    }
    fn search_chunk(
        &mut self,
        digits: &[u8],
        actual_windows: usize,
        params: &SearchChunkParams<'_>,
    ) -> Result<Vec<WindowScore>>;
    fn reconfigure(&mut self, workers: usize) -> Result<()>;
}

pub(crate) fn chunk_windows_for_backend(chunk_windows: usize, backend_name: &str) -> usize {
    if matches!(backend_name, "gpu" | "cuda") {
        chunk_windows.min(MAX_ACCELERATOR_CHUNK_WINDOWS)
    } else {
        chunk_windows
    }
}

pub(crate) struct CpuSearchBackend {
    pool: Option<Arc<rayon::ThreadPool>>,
    shared_pool: Option<Arc<rayon::ThreadPool>>,
    workers: usize,
}

impl CpuSearchBackend {
    fn new(workers: usize, shared_pool: Option<Arc<rayon::ThreadPool>>) -> Result<Self> {
        let workers = workers.max(1);
        let pool = if workers == 1 {
            None
        } else {
            Some(Arc::clone(shared_pool.as_ref().ok_or_else(|| {
                anyhow!("multi-worker CPU backend requires a resource-budget pool")
            })?))
        };
        Ok(Self {
            pool,
            shared_pool,
            workers,
        })
    }
}

impl SearchBackend for CpuSearchBackend {
    fn name(&self) -> &'static str {
        "cpu"
    }

    fn gpu_status(&self) -> String {
        "disabled".to_string()
    }

    fn cpu_worker_width(&self) -> usize {
        self.workers
    }

    fn search_chunk(
        &mut self,
        digits: &[u8],
        actual_windows: usize,
        params: &SearchChunkParams<'_>,
    ) -> Result<Vec<WindowScore>> {
        let score = |index| {
            score_candidate_window(
                index,
                &digits[index..index + params.window_len],
                params.target,
                params.mode,
                params.canvas_width,
                params.canvas_height,
                params.threshold,
                params.invert,
                params.emergence_plan,
            )
        };
        let scores = match &self.pool {
            None => (0..actual_windows).map(score).collect(),
            Some(pool) => pool.install(|| (0..actual_windows).into_par_iter().map(score).collect()),
        };
        Ok(scores)
    }

    fn reconfigure(&mut self, workers: usize) -> Result<()> {
        *self = Self::new(workers, self.shared_pool.clone())?;
        Ok(())
    }
}

pub(crate) struct MockGpuSearchBackend {
    queue_depth: usize,
}

impl SearchBackend for MockGpuSearchBackend {
    fn name(&self) -> &'static str {
        "gpu"
    }

    fn gpu_status(&self) -> String {
        "active: test-only mock wgpu".to_string()
    }

    fn cpu_worker_width(&self) -> usize {
        1
    }

    fn gpu_ring_depth(&self) -> usize {
        self.queue_depth
    }

    fn search_chunk(
        &mut self,
        _digits: &[u8],
        actual_windows: usize,
        _params: &SearchChunkParams<'_>,
    ) -> Result<Vec<WindowScore>> {
        crate::gpu::record_mock_ring(crate::gpu_ring::run_mock_ring(
            actual_windows,
            self.queue_depth,
        ));
        if crate::gpu_ring::test_runtime_fault_for("wgpu") {
            bail!("injected wgpu post-preflight execution failure");
        }
        Ok((0..actual_windows).map(WindowScore::empty).collect())
    }

    fn reconfigure(&mut self, _workers: usize) -> Result<()> {
        Ok(())
    }
}

pub(crate) struct HybridGpuSearchBackend {
    gpu: GpuSearchEngine,
    cpu: CpuSearchBackend,
}

impl HybridGpuSearchBackend {
    fn new(
        workers: usize,
        device_filter: Option<&str>,
        queue_depth: usize,
        shared_pool: Option<Arc<rayon::ThreadPool>>,
    ) -> Result<Self> {
        Ok(Self {
            gpu: GpuSearchEngine::new_with_depth(device_filter, queue_depth)?,
            cpu: CpuSearchBackend::new(workers, shared_pool)?,
        })
    }
}

impl SearchBackend for HybridGpuSearchBackend {
    fn name(&self) -> &'static str {
        "gpu"
    }

    fn gpu_status(&self) -> String {
        format!("active: {}", self.gpu.device_name())
    }

    fn cpu_worker_width(&self) -> usize {
        self.cpu.cpu_worker_width()
    }

    fn gpu_ring_depth(&self) -> usize {
        self.gpu.ring_depth()
    }

    fn search_chunk(
        &mut self,
        digits: &[u8],
        actual_windows: usize,
        params: &SearchChunkParams<'_>,
    ) -> Result<Vec<WindowScore>> {
        if params.mode != MatchMode::Emergence {
            return self.cpu.search_chunk(digits, actual_windows, params);
        }

        match self.gpu.emergence_scores(
            digits,
            actual_windows,
            params.target,
            params.canvas_width,
            params.canvas_height,
        ) {
            Ok(scores) => scores
                .into_iter()
                .enumerate()
                .map(gpu_score_to_window_score)
                .collect(),
            Err(error) => Err(error),
        }
    }

    fn reconfigure(&mut self, workers: usize) -> Result<()> {
        self.cpu.reconfigure(workers)
    }
}

pub(crate) fn create_search_backend(
    options: &SearchOptions,
    target: &Bitmap,
    budget: &ResourceBudget,
) -> Result<Box<dyn SearchBackend>> {
    #[cfg(feature = "cuda-native")]
    if options.performance.backend == SearchBackendChoice::Cuda {
        return Ok(Box::new(super::cuda_backend::CudaSearchBackend::new(
            options.performance.limits.queue_depth,
        )?));
    }
    if crate::gpu_ring::test_mock_enabled()
        && options.match_mode == MatchMode::Emergence
        && options.performance.gpu != GpuMode::Off
        && options.performance.backend != SearchBackendChoice::Cpu
    {
        return Ok(Box::new(MockGpuSearchBackend {
            queue_depth: options.performance.limits.queue_depth.max(1),
        }));
    }
    let explicit_gpu = options.performance.backend == SearchBackendChoice::Gpu
        || options.performance.gpu == GpuMode::On;
    let wants_gpu = explicit_gpu
        || (options.performance.gpu == GpuMode::Auto && should_auto_use_gpu(options, target));
    let shared_pool = budget.cpu_pool()?;
    if wants_gpu && options.match_mode == MatchMode::Emergence {
        match HybridGpuSearchBackend::new(
            options.performance.limits.cpu_workers,
            options.performance.gpu_device.as_deref(),
            options.performance.limits.queue_depth,
            shared_pool.clone(),
        ) {
            Ok(backend) => return Ok(Box::new(backend)),
            Err(err) => {
                if explicit_gpu {
                    eprintln!("warning: GPU backend requested but unavailable; using CPU: {err:#}");
                }
            }
        }
    }
    Ok(Box::new(CpuSearchBackend::new(
        options.performance.limits.cpu_workers,
        shared_pool,
    )?))
}

pub(crate) fn should_auto_use_gpu(options: &SearchOptions, target: &Bitmap) -> bool {
    if options.match_mode != MatchMode::Emergence {
        return false;
    }
    if options.performance.stress_test || options.performance.profile == PerformanceProfile::Max {
        return true;
    }
    let canvas_pixels = options.canvas_width.saturating_mul(options.canvas_height);
    let target_pixels = target.width.saturating_mul(target.height);
    let placement_width = options
        .canvas_width
        .saturating_sub(target.width)
        .saturating_add(1);
    let placement_height = options
        .canvas_height
        .saturating_sub(target.height)
        .saturating_add(1);
    let estimated_work = options
        .performance
        .limits
        .chunk_size
        .saturating_mul(placement_width.saturating_mul(placement_height))
        .saturating_mul(target_pixels);
    canvas_pixels >= 4_096
        && options.performance.limits.chunk_size >= 50_000
        && estimated_work >= 5_000_000_000
}

pub(crate) fn gpu_score_to_window_score(
    (index, score): (usize, GpuWindowScore),
) -> Result<WindowScore> {
    let statistics = if let Some(statistics) = score.statistics {
        Some(EmergenceStatistics {
            covered: usize::try_from(statistics.covered)
                .map_err(|_| anyhow!("GPU covered count does not fit the host usize"))?,
            total: usize::try_from(statistics.total)
                .map_err(|_| anyhow!("GPU total count does not fit the host usize"))?,
            leaked: usize::try_from(statistics.leaked)
                .map_err(|_| anyhow!("GPU leaked count does not fit the host usize"))?,
            background_total: usize::try_from(statistics.background_total)
                .map_err(|_| anyhow!("GPU background total count does not fit the host usize"))?,
        })
    } else {
        None
    };

    Ok(WindowScore {
        index,
        score: score.score,
        score_q: quantize_score(score.score),
        inverted: false,
        digit: Some(score.digit),
        x: Some(score.x),
        y: Some(score.y),
        coverage: Some(score.coverage),
        leakage: Some(score.leakage),
        coverage_q: Some(quantize_score(score.coverage)),
        leakage_q: Some(quantize_score(score.leakage)),
        statistics,
    })
}

#[cfg(any(feature = "cuda-native", test))]
pub(crate) fn host_recompute_scores(
    transported: &[WindowScore],
    digits: &[u8],
    params: &SearchChunkParams<'_>,
) -> Result<Vec<WindowScore>> {
    transported
        .iter()
        .enumerate()
        .map(|(expected_index, candidate)| {
            if candidate.index != expected_index {
                bail!(
                    "backend candidate index {} did not match expected source index {expected_index}",
                    candidate.index
                );
            }
            if candidate.score_q > 1_000_000
                || candidate.coverage_q.is_some_and(|value| value > 1_000_000)
                || candidate.leakage_q.is_some_and(|value| value > 1_000_000)
            {
                bail!("backend candidate contained an out-of-range score diagnostic");
            }
            if candidate.statistics.as_ref().is_some_and(|statistics| {
                statistics.covered > statistics.total
                    || statistics.leaked > statistics.background_total
            }) {
                bail!("backend candidate contained invalid sufficient statistics");
            }
            let end = candidate
                .index
                .checked_add(params.window_len)
                .filter(|end| *end <= digits.len());
            let Some(end) = end else {
                bail!(
                    "backend candidate index {} is outside the source window range",
                    candidate.index
                );
            };
            if params.mode == MatchMode::Emergence && candidate.statistics.is_none() {
                bail!(
                    "backend emergence candidate {} omitted exact sufficient statistics",
                    candidate.index
                );
            }
            if params.mode != MatchMode::Emergence && candidate.statistics.is_some() {
                bail!(
                    "backend non-emergence candidate {} carried emergence statistics",
                    candidate.index
                );
            }
            let recomputed = score_candidate_window(
                candidate.index,
                &digits[candidate.index..end],
                params.target,
                params.mode,
                params.canvas_width,
                params.canvas_height,
                params.threshold,
                params.invert,
                params.emergence_plan,
            );
            if params.mode == MatchMode::Emergence {
                let Some(received) = candidate.statistics.as_ref() else {
                    unreachable!("emergence statistics were checked above");
                };
                let Some(expected) = recomputed.statistics.as_ref() else {
                    bail!(
                        "backend emergence candidate {} has no canonical sufficient statistics",
                        candidate.index
                    );
                };
                if received != expected {
                    bail!(
                        "backend candidate {} carried mismatched sufficient statistics",
                        candidate.index
                    );
                }
            }
            Ok(recomputed)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gpu::GpuEmergenceStatistics;
    use crate::performance::{
        GeneratorBackendChoice, PerformanceOverrides, PerformanceSettings, ThermalMode,
    };
    use std::time::Duration;

    fn cpu_backend(workers: usize) -> CpuSearchBackend {
        let budget = crate::search::ResourceBudget::new(1, 64, workers).unwrap();
        CpuSearchBackend::new(workers, budget.cpu_pool().unwrap()).unwrap()
    }

    fn options(match_mode: MatchMode, chunk_size: usize) -> SearchOptions {
        SearchOptions {
            max_offset: None,
            work_windows: None,
            limit: None,
            match_mode,
            canvas_width: 24,
            canvas_height: 24,
            threshold: 5,
            invert: false,
            workers: None,
            checkpoint_every: Duration::from_secs(60),
            top_n: 10,
            keep_going_after_perfect: false,
            chunk_windows: chunk_size,
            performance: PerformanceSettings::from_profile(
                PerformanceProfile::Custom,
                SearchBackendChoice::Cpu,
                GeneratorBackendChoice::Cpu,
                GpuMode::Auto,
                None,
                ThermalMode::Normal,
                false,
                false,
                match_mode,
                PerformanceOverrides {
                    chunk_size: Some(chunk_size),
                    ..PerformanceOverrides::default()
                },
            ),
        }
    }

    fn gpu_statistics(score: &WindowScore) -> GpuEmergenceStatistics {
        let statistics = score.statistics.as_ref().unwrap();
        GpuEmergenceStatistics {
            covered: u32::try_from(statistics.covered).unwrap(),
            total: u32::try_from(statistics.total).unwrap(),
            leaked: u32::try_from(statistics.leaked).unwrap(),
            background_total: u32::try_from(statistics.background_total).unwrap(),
        }
    }

    #[test]
    fn gpu_is_never_auto_selected_outside_emergence_mode() {
        // Threshold and exact matching have no GPU kernel; auto-selecting one
        // would just cost a device init and fall back.
        let target = Bitmap::blank(12, 12);
        assert!(!should_auto_use_gpu(
            &options(MatchMode::Threshold, 1_000_000),
            &target
        ));
        assert!(!should_auto_use_gpu(
            &options(MatchMode::Exact, 1_000_000),
            &target
        ));
    }

    #[test]
    fn tiny_workloads_stay_on_the_cpu() {
        let target = Bitmap::blank(4, 4);
        let mut opts = options(MatchMode::Emergence, 128);
        opts.canvas_width = 8;
        opts.canvas_height = 8;
        assert!(!should_auto_use_gpu(&opts, &target));
    }

    #[test]
    fn cpu_backend_scores_every_window() {
        let mut backend = cpu_backend(2);
        let target = Bitmap::new(2, 2, vec![0, 1, 1, 0]).unwrap();
        let digits = vec![1, 1, 0, 6, 6, 0, 1, 2, 3];
        let params = SearchChunkParams {
            target: &target,
            mode: MatchMode::Threshold,
            canvas_width: 2,
            canvas_height: 2,
            threshold: 5,
            invert: false,
            window_len: 4,
            emergence_plan: None,
        };
        let scores = backend.search_chunk(&digits, 6, &params).unwrap();
        assert_eq!(scores.len(), 6);
        assert!(scores.iter().all(|score| score.statistics.is_none()));
        // "0660" at index 2 is exactly the target under threshold 5.
        assert_eq!(scores[2].score, 1.0);
    }

    #[test]
    fn reconfiguring_workers_keeps_the_backend_usable() {
        let budget = crate::search::ResourceBudget::new(1, 64, 3).unwrap();
        let mut backend = CpuSearchBackend::new(1, budget.cpu_pool().unwrap()).unwrap();
        backend.reconfigure(3).unwrap();
        assert_eq!(backend.name(), "cpu");
    }

    #[test]
    fn cpu_gpu_score_contract_matches_reference() {
        // Given: GPU-shaped telemetry where every candidate lies in one
        // score_q bucket and every transported f64/placement is deliberately wrong.
        let target = Bitmap::new(2, 2, vec![1, 0, 0, 1]).unwrap();
        let digits = vec![1, 2, 3, 1, 4, 1, 5];
        let plan = EmergencePlan::new(&target, 2, 2);
        let params = SearchChunkParams {
            target: &target,
            mode: MatchMode::Emergence,
            canvas_width: 2,
            canvas_height: 2,
            threshold: 5,
            invert: false,
            window_len: 4,
            emergence_plan: Some(&plan),
        };
        let mut cpu = cpu_backend(2);
        let canonical = cpu.search_chunk(&digits, 4, &params).unwrap();
        let transported = (0..4)
            .map(|index| {
                gpu_score_to_window_score((
                    index,
                    GpuWindowScore {
                        score: 0.500_000_1,
                        digit: 9,
                        x: 99,
                        y: 99,
                        coverage: 0.500_000_1,
                        leakage: 0.500_000_1,
                        statistics: Some(gpu_statistics(&canonical[index])),
                    },
                ))
                .unwrap()
            })
            .collect::<Vec<_>>();
        assert!(
            transported
                .windows(2)
                .all(|pair| pair[0].score_q == pair[1].score_q)
        );

        // When: host canonicalization consumes the original source digits.
        let rescored = host_recompute_scores(&transported, &digits, &params).unwrap();

        // Then: all candidates, including every collision member, are exact CPU results.
        assert_eq!(rescored, canonical);
        assert!(rescored.iter().all(|score| score.digit != Some(9)));
    }

    #[test]
    fn cpu_gpu_score_contract_rejects_mismatched_tie() {
        // Given: equal-score transport candidates whose stable source indexes are swapped.
        let target = Bitmap::new(1, 1, vec![1]).unwrap();
        let digits = vec![3, 4];
        let plan = EmergencePlan::new(&target, 1, 1);
        let params = SearchChunkParams {
            target: &target,
            mode: MatchMode::Emergence,
            canvas_width: 1,
            canvas_height: 1,
            threshold: 5,
            invert: false,
            window_len: 1,
            emergence_plan: Some(&plan),
        };
        let transport_tie = vec![
            gpu_score_to_window_score((
                1,
                GpuWindowScore {
                    score: 0.5,
                    digit: 3,
                    x: 0,
                    y: 0,
                    coverage: 0.5,
                    leakage: 0.0,
                    statistics: None,
                },
            ))
            .unwrap(),
            gpu_score_to_window_score((
                0,
                GpuWindowScore {
                    score: 0.5,
                    digit: 4,
                    x: 0,
                    y: 0,
                    coverage: 0.5,
                    leakage: 0.0,
                    statistics: None,
                },
            ))
            .unwrap(),
        ];

        // When: host canonicalization validates tie metadata before reduction.
        let error = host_recompute_scores(&transport_tie, &digits, &params).unwrap_err();

        // Then: telemetry cannot reorder equal candidates or prove a winner.
        assert!(error.to_string().contains("candidate index"));
    }

    #[test]
    fn host_recompute_rejects_out_of_range_quantized_diagnostics() {
        // Given: a source-valid transport candidate with an impossible score_q value.
        let target = Bitmap::new(1, 1, vec![1]).unwrap();
        let digits = vec![3];
        let plan = EmergencePlan::new(&target, 1, 1);
        let params = SearchChunkParams {
            target: &target,
            mode: MatchMode::Emergence,
            canvas_width: 1,
            canvas_height: 1,
            threshold: 5,
            invert: false,
            window_len: 1,
            emergence_plan: Some(&plan),
        };
        let mut malformed = vec![
            gpu_score_to_window_score((
                0,
                GpuWindowScore {
                    score: 1.0,
                    digit: 3,
                    x: 0,
                    y: 0,
                    coverage: 1.0,
                    leakage: 0.0,
                    statistics: None,
                },
            ))
            .unwrap(),
        ];
        malformed[0].score_q = 1_000_001;

        // When: host canonicalization parses the transport diagnostics.
        let error = host_recompute_scores(&malformed, &digits, &params).unwrap_err();

        // Then: malformed telemetry is rejected before canonical reduction.
        assert!(error.to_string().contains("score diagnostic"));
    }

    #[test]
    fn emergence_transport_requires_exact_sufficient_statistics() {
        // Given: an otherwise well-formed emergence candidate with no exact source counts.
        let target = Bitmap::new(1, 1, vec![1]).unwrap();
        let digits = vec![3];
        let plan = EmergencePlan::new(&target, 1, 1);
        let params = SearchChunkParams {
            target: &target,
            mode: MatchMode::Emergence,
            canvas_width: 1,
            canvas_height: 1,
            threshold: 5,
            invert: false,
            window_len: 1,
            emergence_plan: Some(&plan),
        };
        let transported = vec![
            gpu_score_to_window_score((
                0,
                GpuWindowScore {
                    score: 1.0,
                    digit: 3,
                    x: 0,
                    y: 0,
                    coverage: 1.0,
                    leakage: 0.0,
                    statistics: None,
                },
            ))
            .unwrap(),
        ];

        // When: host canonicalization receives the GPU result.
        let error = host_recompute_scores(&transported, &digits, &params).unwrap_err();

        // Then: quantized diagnostics cannot substitute for exact sufficient statistics.
        assert!(error.to_string().contains("sufficient statistics"));
    }

    #[test]
    fn gpu_statistics_survive_transport_and_host_validation() {
        // Given: a canonical emergence candidate and exact counts supplied by the GPU transport.
        let target = Bitmap::new(2, 2, vec![1, 0, 0, 1]).unwrap();
        let digits = vec![1, 2, 3, 1];
        let plan = EmergencePlan::new(&target, 2, 2);
        let params = SearchChunkParams {
            target: &target,
            mode: MatchMode::Emergence,
            canvas_width: 2,
            canvas_height: 2,
            threshold: 5,
            invert: false,
            window_len: 4,
            emergence_plan: Some(&plan),
        };
        let mut cpu = cpu_backend(1);
        let canonical = cpu.search_chunk(&digits, 1, &params).unwrap();
        let expected_statistics = canonical[0].statistics.clone().unwrap();
        let transported = vec![
            gpu_score_to_window_score((
                0,
                GpuWindowScore {
                    score: 0.123_456,
                    digit: 9,
                    x: 99,
                    y: 99,
                    coverage: 0.123_456,
                    leakage: 0.654_321,
                    statistics: Some(GpuEmergenceStatistics {
                        covered: u32::try_from(expected_statistics.covered).unwrap(),
                        total: u32::try_from(expected_statistics.total).unwrap(),
                        leaked: u32::try_from(expected_statistics.leaked).unwrap(),
                        background_total: u32::try_from(expected_statistics.background_total)
                            .unwrap(),
                    }),
                },
            ))
            .unwrap(),
        ];

        // When: the host converts, validates, and recomputes the transported candidate.
        assert_eq!(
            transported[0].statistics.as_ref(),
            Some(&expected_statistics)
        );
        let rescored = host_recompute_scores(&transported, &digits, &params).unwrap();

        // Then: exact f64 output and source-derived metadata remain canonical.
        assert_eq!(rescored, canonical);

        // Given: a transport tuple with valid bounds but an incorrect covered count.
        let mut invalid = transported;
        invalid[0].statistics.as_mut().unwrap().covered = 1;

        // When: host validation checks the sufficient statistics against source digits.
        let error = host_recompute_scores(&invalid, &digits, &params).unwrap_err();

        // Then: quantized telemetry cannot hide a sufficient-statistics mismatch.
        assert!(
            error
                .to_string()
                .contains("mismatched sufficient statistics")
        );
    }

    #[test]
    fn one_cpu_worker_uses_direct_execution_without_a_rayon_pool() {
        // Given: the canonical benchmark's bounded single-worker configuration.
        let backend = cpu_backend(1);

        // When: the backend selects its execution strategy at construction.
        // Then: no cold Rayon pool exists in the measured request path.
        assert!(backend.pool.is_none());
        assert_eq!(backend.cpu_worker_width(), 1);
    }

    #[test]
    fn cpu_backends_reuse_the_rayon_pool_from_one_resource_budget() {
        // Given: two CPU backends created with the same multi-worker budget.
        let budget = crate::search::ResourceBudget::new(1, 64, 2).unwrap();
        let first = CpuSearchBackend::new(2, budget.cpu_pool().unwrap()).unwrap();
        let second = CpuSearchBackend::new(2, budget.cpu_pool().unwrap()).unwrap();

        // When: both backends are inspected for their shared execution pool.
        let first_pool = first.pool.as_ref().expect("multi-worker backend pool");
        let second_pool = second.pool.as_ref().expect("multi-worker backend pool");

        // Then: both instances point at the exact same Rayon pool.
        assert!(std::sync::Arc::ptr_eq(first_pool, second_pool));
    }

    #[test]
    fn cpu_worker_counts_return_byte_equivalent_scores_and_ties() {
        // Given: the same complete emergence chunk for worker counts 1, 2, and 4.
        let target = Bitmap::new(2, 2, vec![1, 0, 0, 1]).unwrap();
        let digits = vec![1, 2, 3, 1, 4, 1, 5];
        let plan = EmergencePlan::new(&target, 2, 2);
        let params = SearchChunkParams {
            target: &target,
            mode: MatchMode::Emergence,
            canvas_width: 2,
            canvas_height: 2,
            threshold: 5,
            invert: false,
            window_len: 4,
            emergence_plan: Some(&plan),
        };

        // When: each explicit Rayon pool scores every source offset.
        let results = [1, 2, 4].map(|workers| {
            cpu_backend(workers)
                .search_chunk(&digits, 4, &params)
                .unwrap()
        });

        // Then: exact f64 values, metadata, diagnostics, and tie candidates match.
        assert_eq!(results[0], results[1]);
        assert_eq!(results[0], results[2]);
    }

    #[test]
    fn available_gpu_matches_cpu_with_device_side_exact_ordering() {
        // Given: a non-trivial count-rank table and an optional real GPU device.
        let budget = crate::search::ResourceBudget::new(1, 64, 2).unwrap();
        let Ok(mut gpu) = HybridGpuSearchBackend::new(2, None, 1, budget.cpu_pool().unwrap())
        else {
            return;
        };
        let target = Bitmap::new(3, 3, vec![1, 0, 1, 0, 1, 0, 1, 0, 0]).unwrap();
        let canvas_width = 5;
        let canvas_height = 4;
        let window_len = canvas_width * canvas_height;
        let actual_windows = 128;
        let digits = (0..actual_windows + window_len - 1)
            .map(|index| ((index * 7 + 3) % 10) as u8)
            .collect::<Vec<_>>();
        let plan = EmergencePlan::new(&target, canvas_width, canvas_height);
        let params = SearchChunkParams {
            target: &target,
            mode: MatchMode::Emergence,
            canvas_width,
            canvas_height,
            threshold: 5,
            invert: false,
            window_len,
            emergence_plan: Some(&plan),
        };

        // When: CPU and available GPU score the same source windows.
        let mut cpu = cpu_backend(2);
        let expected = cpu.search_chunk(&digits, actual_windows, &params).unwrap();
        let actual = gpu.search_chunk(&digits, actual_windows, &params).unwrap();

        // Then: GPU-side exact ordering preserves all canonical result fields.
        assert_eq!(actual, expected);
    }

    #[test]
    fn accelerator_chunk_size_is_bounded_without_changing_cpu_chunks() {
        assert_eq!(chunk_windows_for_backend(524_288, "gpu"), 262_144);
        assert_eq!(chunk_windows_for_backend(524_288, "cuda"), 262_144);
        assert_eq!(chunk_windows_for_backend(8_192, "gpu"), 8_192);
        assert_eq!(chunk_windows_for_backend(524_288, "cpu"), 524_288);
    }
}
