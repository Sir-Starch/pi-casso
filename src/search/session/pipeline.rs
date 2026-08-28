use std::sync::atomic::AtomicBool;
use std::sync::{
    Arc, Mutex,
    mpsc::{self, Receiver},
};
use std::thread;

use anyhow::{Result, anyhow};

use crate::search::backend::chunk_windows_for_backend;
use crate::search::session::SearchSession;
use crate::search::types::{FinishReason, SearchCommand, SearchOptions, SearchReporter};
use crate::storage::Storage;

mod reducer;
mod worker;

use reducer::reduce;
use worker::{BackendConfig, ProducerConfig, SharedBackend};

pub(crate) fn run<R: SearchReporter>(
    session: &mut SearchSession<'_>,
    storage: &mut Storage,
    reporter: &mut R,
    control: Option<&Receiver<SearchCommand>>,
    stop: Arc<AtomicBool>,
) -> Result<FinishReason> {
    session.budget.validate_minimum(session.window_len)?;
    let queue_depth = session.options.performance.limits.queue_depth.max(1);
    let accelerator = matches!(session.backend.name(), "gpu" | "cuda");
    let chunk_windows =
        chunk_windows_for_backend(session.options.chunk_windows, session.backend.name());
    let producer_config = ProducerConfig {
        start_offset: session.run.current_offset,
        start_scanned: session.run.scanned_windows,
        max_offset: session.options.max_offset,
        limit: SearchOptions::intersect_count_bounds(
            session.options.work_windows,
            session.options.limit,
        ),
        chunk_windows,
        window_len: session.window_len,
        growing: session.source.is_growing(),
        accelerator,
    };
    let target = Arc::new(session.run.target_bitmap.clone());
    let initial_gpu_mode = session.options.performance.gpu;
    let backend_config = BackendConfig {
        target: Arc::clone(&target),
        mode: session.options.match_mode,
        canvas_width: session.canvas_width,
        canvas_height: session.canvas_height,
        threshold: session.options.threshold,
        invert: session.options.invert,
        window_len: session.window_len,
        gpu_utilization: session.options.performance.effective_gpu_utilization(),
        fallback_options: session.options.clone(),
    };
    let backend = std::mem::replace(
        &mut session.backend,
        Box::new(SharedBackend::new(Arc::new(Mutex::new(Box::new(
            UnavailableBackend,
        ))))),
    );
    let active_backend = Arc::new(Mutex::new(backend));
    session.backend = Box::new(SharedBackend::new(Arc::clone(&active_backend)));
    let reader_pool = session
        .reader_pool
        .take()
        .ok_or_else(|| anyhow!("search reader pool was already taken"))?;
    let budget = Arc::clone(&session.budget);
    let (chunk_tx, chunk_rx) = mpsc::sync_channel(queue_depth);
    let (result_tx, result_rx) = mpsc::sync_channel(queue_depth);
    let producer_stop = Arc::clone(&stop);
    let backend_stop = Arc::clone(&stop);
    let producer_budget = Arc::clone(&budget);
    let backend_budget = Arc::clone(&budget);
    let source = session.source;

    let scoped = thread::scope(|scope| {
        let producer = scope.spawn(move || {
            worker::produce(
                chunk_tx,
                reader_pool,
                source,
                producer_config,
                producer_budget,
                producer_stop,
            )
        });
        let backend = scope.spawn(move || {
            worker::run_backend(
                chunk_rx,
                result_tx,
                active_backend,
                backend_config,
                backend_budget,
                backend_stop,
            )
        });
        let reduction = reduce(
            session,
            storage,
            reporter,
            result_rx,
            Arc::clone(&budget),
            Arc::clone(&stop),
            control,
        );
        source.cancel_generation_waiters();
        let producer_result = producer
            .join()
            .map_err(|_| anyhow!("search producer panicked"))?;
        let backend_result = backend
            .join()
            .map_err(|_| anyhow!("search backend panicked"))?;
        Ok::<_, anyhow::Error>((reduction, producer_result, backend_result))
    })?;

    let (reduction, producer_result, backend_result) = scoped;
    let (reader_pool, producer_report) = producer_result?;
    session.reader_pool = Some(reader_pool);
    session
        .telemetry
        .record_queue_wait(producer_report.queue_wait);
    session
        .telemetry
        .record_source_wait(producer_report.source_wait);
    session
        .telemetry
        .record_generator_wait(producer_report.generator_wait);
    session.runtime.producer_epochs = producer_report.producer_epochs;
    session.runtime.coalesced_request_count = producer_report.coalesced_requests;
    session.runtime.generation_batches = producer_report.generation_batches;
    if let Some(reader_pool) = session.reader_pool.as_ref() {
        session
            .telemetry
            .record_source_pool(reader_pool.telemetry());
    }
    if let Ok(summary) = backend_result {
        session.telemetry.record_gpu_duty(summary.gpu_duty);
        session.runtime.active_backend = summary.active_backend;
        session.runtime.gpu_status = summary.gpu_status;
    }
    if session.options.performance.gpu != initial_gpu_mode {
        session.rebuild_backend()?;
    }
    reduction
}

struct UnavailableBackend;

impl crate::search::backend::SearchBackend for UnavailableBackend {
    fn name(&self) -> &'static str {
        "unavailable"
    }

    fn gpu_status(&self) -> String {
        "unavailable".to_string()
    }

    fn cpu_worker_width(&self) -> usize {
        1
    }

    fn search_chunk(
        &mut self,
        _digits: &[u8],
        _actual_windows: usize,
        _params: &crate::search::backend::SearchChunkParams<'_>,
    ) -> Result<Vec<crate::search::types::WindowScore>> {
        Err(anyhow!("search backend handoff was unavailable"))
    }

    fn reconfigure(&mut self, _workers: usize) -> Result<()> {
        Err(anyhow!("search backend handoff was unavailable"))
    }
}
