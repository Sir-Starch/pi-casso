use anyhow::{Result, bail};

use crate::cuda_engine::CudaSearchEngine;
use crate::gpu;
use crate::search::backend::{
    SearchBackend, SearchChunkParams, gpu_score_to_window_score, host_recompute_scores,
};
use crate::search::types::{MatchMode, WindowScore};

enum Execution {
    Native(CudaSearchEngine),
    Mock,
}

pub(crate) struct CudaSearchBackend {
    execution: Execution,
    queue_depth: usize,
}

impl CudaSearchBackend {
    pub(crate) fn new(queue_depth: usize) -> Result<Self> {
        let execution = if crate::cuda::fake_execution_enabled() {
            Execution::Mock
        } else {
            Execution::Native(CudaSearchEngine::new()?)
        };
        Ok(Self {
            execution,
            queue_depth: queue_depth.max(1),
        })
    }
}

impl SearchBackend for CudaSearchBackend {
    fn name(&self) -> &'static str {
        "cuda"
    }

    fn gpu_status(&self) -> String {
        match &self.execution {
            Execution::Native(engine) => format!("active: {}", engine.device_name()),
            Execution::Mock => "active: test-only mock CUDA".to_string(),
        }
    }

    fn cpu_worker_width(&self) -> usize {
        1
    }

    fn gpu_ring_depth(&self) -> usize {
        self.queue_depth
    }

    fn search_chunk(
        &mut self,
        digits: &[u8],
        actual_windows: usize,
        params: &SearchChunkParams<'_>,
    ) -> Result<Vec<WindowScore>> {
        if params.mode != MatchMode::Emergence {
            bail!("CUDA supports emergence matching only");
        }
        let scores = match &self.execution {
            Execution::Native(engine) => engine.emergence_scores(
                digits,
                actual_windows,
                params.target,
                params.canvas_width,
                params.canvas_height,
            )?,
            Execution::Mock => {
                gpu::record_mock_ring(crate::gpu_ring::run_mock_ring(
                    actual_windows,
                    self.queue_depth,
                ));
                if crate::gpu_ring::test_runtime_fault_for("cuda") {
                    bail!("injected cuda post-preflight execution failure");
                }
                return Ok((0..actual_windows).map(WindowScore::empty).collect());
            }
        };
        let transported = scores
            .into_iter()
            .enumerate()
            .map(gpu_score_to_window_score)
            .collect::<Result<Vec<_>>>()?;
        host_recompute_scores(&transported, digits, params)
    }

    fn reconfigure(&mut self, _workers: usize) -> Result<()> {
        Ok(())
    }
}
