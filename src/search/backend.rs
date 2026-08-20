//! Compute backends. A backend takes a slab of digits and returns one score per
//! window; whether it does that on the CPU thread pool or on the GPU is an
//! implementation detail the engine never has to know about.

use anyhow::Result;
use rayon::prelude::*;

use crate::art::Bitmap;
use crate::gpu::{GpuSearchEngine, GpuWindowScore};
use crate::performance::{GpuMode, PerformanceProfile, SearchBackendChoice};
use crate::search::scoring::{EmergencePlan, score_candidate_window};
use crate::search::types::{MatchMode, SearchOptions, WindowScore};

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

pub(crate) trait SearchBackend {
    fn name(&self) -> &'static str;
    fn gpu_status(&self) -> String;
    fn search_chunk(
        &mut self,
        digits: &[u8],
        actual_windows: usize,
        params: &SearchChunkParams<'_>,
    ) -> Result<Vec<WindowScore>>;
    fn reconfigure(&mut self, workers: usize) -> Result<()>;
}

pub(crate) struct CpuSearchBackend {
    pool: rayon::ThreadPool,
}

impl CpuSearchBackend {
    fn new(workers: usize) -> Result<Self> {
        let workers = workers.max(1);
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(workers)
            .build()?;
        Ok(Self { pool })
    }
}

impl SearchBackend for CpuSearchBackend {
    fn name(&self) -> &'static str {
        "cpu"
    }

    fn gpu_status(&self) -> String {
        "disabled".to_string()
    }

    fn search_chunk(
        &mut self,
        digits: &[u8],
        actual_windows: usize,
        params: &SearchChunkParams<'_>,
    ) -> Result<Vec<WindowScore>> {
        Ok(self.pool.install(|| {
            (0..actual_windows)
                .into_par_iter()
                .map(|index| {
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
                })
                .collect()
        }))
    }

    fn reconfigure(&mut self, workers: usize) -> Result<()> {
        *self = Self::new(workers)?;
        Ok(())
    }
}

pub(crate) struct HybridGpuSearchBackend {
    gpu: GpuSearchEngine,
    cpu: CpuSearchBackend,
}

impl HybridGpuSearchBackend {
    fn new(workers: usize, device_filter: Option<&str>) -> Result<Self> {
        Ok(Self {
            gpu: GpuSearchEngine::new(device_filter)?,
            cpu: CpuSearchBackend::new(workers)?,
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
            Ok(scores) => Ok(scores
                .into_iter()
                .enumerate()
                .map(gpu_score_to_window_score)
                .collect()),
            Err(err) => {
                eprintln!(
                    "warning: GPU search failed; falling back to CPU for this chunk: {err:#}"
                );
                self.cpu.search_chunk(digits, actual_windows, params)
            }
        }
    }

    fn reconfigure(&mut self, workers: usize) -> Result<()> {
        self.cpu.reconfigure(workers)
    }
}

pub(crate) fn create_search_backend(
    options: &SearchOptions,
    target: &Bitmap,
) -> Result<Box<dyn SearchBackend>> {
    let explicit_gpu = options.performance.backend == SearchBackendChoice::Gpu
        || options.performance.gpu == GpuMode::On;
    let wants_gpu = explicit_gpu
        || (options.performance.gpu == GpuMode::Auto && should_auto_use_gpu(options, target));
    if wants_gpu && options.match_mode == MatchMode::Emergence {
        match HybridGpuSearchBackend::new(
            options.performance.limits.cpu_workers,
            options.performance.gpu_device.as_deref(),
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

pub(crate) fn gpu_score_to_window_score((index, score): (usize, GpuWindowScore)) -> WindowScore {
    WindowScore {
        index,
        score: score.score,
        inverted: false,
        digit: Some(score.digit),
        x: Some(score.x),
        y: Some(score.y),
        coverage: Some(score.coverage),
        leakage: Some(score.leakage),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::performance::{
        GeneratorBackendChoice, PerformanceOverrides, PerformanceSettings, ThermalMode,
    };
    use std::time::Duration;

    fn options(match_mode: MatchMode, chunk_size: usize) -> SearchOptions {
        SearchOptions {
            max_offset: None,
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
        let mut backend = CpuSearchBackend::new(2).unwrap();
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
        // "0660" at index 2 is exactly the target under threshold 5.
        assert_eq!(scores[2].score, 1.0);
    }

    #[test]
    fn reconfiguring_workers_keeps_the_backend_usable() {
        let mut backend = CpuSearchBackend::new(1).unwrap();
        backend.reconfigure(3).unwrap();
        assert_eq!(backend.name(), "cpu");
    }
}
