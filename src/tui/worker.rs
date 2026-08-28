//! The background search thread and the channel that carries its progress back
//! to the UI.

#[cfg(test)]
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use anyhow::Result;
use sha2::{Digest, Sha256};

use crate::benchmark_contract::BackendResolution;
use crate::performance::PerformanceSnapshot;
use crate::search::{
    FinishReason, SearchCommand, SearchOptions, SearchReporter, SearchSnapshot,
    run_search_controlled,
};
use crate::storage::{BestEventRecord, RunRecord, RunStatus, Storage};
use crate::tui::PreparedResume;
use crate::tui::tabs::hunt::PreparedStart;

#[cfg(test)]
static TEST_WORKER_START_COUNT: AtomicUsize = AtomicUsize::new(0);

pub enum WorkerEvent {
    Snapshot(Box<SearchSnapshot>),
    NewBest(Box<BestEventRecord>),
    Finished(Box<SearchSnapshot>, FinishReason),
    Error(String),
}

#[derive(Clone, Debug)]
struct PreparedWorkerHandoff {
    snapshot: PerformanceSnapshot,
    capability: BackendResolution,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorkerHandoffMarker {
    pub snapshot_sha256: String,
    pub capability_sha256: String,
    pub capability_status: &'static str,
    pub capability_requested: &'static str,
    pub capability_resolved: Option<&'static str>,
}

impl PreparedWorkerHandoff {
    fn marker(&self) -> WorkerHandoffMarker {
        let snapshot_json = self.snapshot.encode_value().to_string();
        let capability_json = serde_json::json!({
            "status": self.capability.status,
            "requested": self.capability.requested,
            "resolved": self.capability.resolved,
            "gpu_mode": self.capability.gpu_mode,
            "fallback": self.capability.fallback,
            "reason": self.capability.reason,
            "auto_min_work_windows": self.capability.auto_min_work_windows,
            "backend_candidates": self
                .capability
                .backend_candidates
                .iter()
                .map(|candidate| {
                    serde_json::json!({
                        "backend": candidate.backend,
                        "status": candidate.status,
                        "eligible": candidate.eligible,
                        "reason": candidate.reason,
                    })
                })
                .collect::<Vec<_>>(),
        });
        WorkerHandoffMarker {
            snapshot_sha256: format!("{:x}", Sha256::digest(snapshot_json.as_bytes())),
            capability_sha256: format!(
                "{:x}",
                Sha256::digest(capability_json.to_string().as_bytes())
            ),
            capability_status: self.capability.status,
            capability_requested: self.capability.requested,
            capability_resolved: self.capability.resolved,
        }
    }
}

impl WorkerHandoffMarker {
    pub fn telemetry_marker(&self) -> String {
        "worker_handoff_received=true".to_string()
    }
}

pub struct SearchWorker {
    control: Sender<SearchCommand>,
    events: Receiver<WorkerEvent>,
    handle: Option<JoinHandle<()>>,
    prepared_handoff: Option<PreparedWorkerHandoff>,
    pub latest: Option<SearchSnapshot>,
    pub paused: bool,
    pub finished: Option<FinishReason>,
    pub error: Option<String>,
    /// Set when the user asked to leave the app; the worker is given the chance
    /// to checkpoint before the process exits.
    pub quit_after_stop: bool,
}

impl SearchWorker {
    pub fn start_prepared_start(prepared: PreparedStart) -> Self {
        let PreparedStart {
            run,
            options,
            capability,
        } = prepared;
        let _validated_backend = capability.resolved;
        Self::start(run, options)
    }

    pub fn start_prepared(prepared: PreparedResume) -> Self {
        let PreparedResume {
            run,
            snapshot,
            options,
            capability,
        } = prepared;
        Self::start_with_handoff(
            run,
            options,
            Some(PreparedWorkerHandoff {
                snapshot,
                capability,
            }),
        )
    }

    pub fn start(run: RunRecord, options: SearchOptions) -> Self {
        Self::start_with_handoff(run, options, None)
    }

    fn start_with_handoff(
        run: RunRecord,
        options: SearchOptions,
        prepared_handoff: Option<PreparedWorkerHandoff>,
    ) -> Self {
        #[cfg(test)]
        TEST_WORKER_START_COUNT.fetch_add(1, Ordering::SeqCst);
        let (control, control_rx) = mpsc::channel();
        let (events_tx, events) = mpsc::channel();
        let handle = thread::spawn(move || {
            let result = (|| -> Result<()> {
                // The worker keeps its own connection: SQLite is in WAL mode, so
                // this coexists with the UI thread's reads.
                let mut storage = Storage::open_default()?;
                let source = run.source.open()?;
                let mut reporter = ChannelReporter::new(events_tx.clone());
                run_search_controlled(
                    &mut storage,
                    run,
                    source.as_ref(),
                    options,
                    &mut reporter,
                    control_rx,
                )
                .map(|_| ())
            })();
            if let Err(err) = result {
                let _ = events_tx.send(WorkerEvent::Error(format!("{err:#}")));
            }
        });

        Self {
            control,
            events,
            handle: Some(handle),
            prepared_handoff,
            latest: None,
            paused: false,
            finished: None,
            error: None,
            quit_after_stop: false,
        }
    }

    pub fn prepared_handoff_marker(&self) -> Option<WorkerHandoffMarker> {
        self.prepared_handoff
            .as_ref()
            .map(PreparedWorkerHandoff::marker)
    }

    /// Best-effort: a worker that has already exited simply ignores commands.
    pub fn send(&self, command: SearchCommand) {
        let _ = self.control.send(command);
    }

    pub fn is_running(&self) -> bool {
        self.finished.is_none() && self.error.is_none()
    }

    pub fn drain(&mut self) -> Vec<WorkerEvent> {
        let mut events = Vec::new();
        while let Ok(event) = self.events.try_recv() {
            events.push(event);
        }
        events
    }

    /// Asks the worker to stop and waits for it, so the final checkpoint is on
    /// disk before the process leaves.
    pub fn stop_and_join(&mut self) {
        self.send(SearchCommand::Stop);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

#[cfg(test)]
pub(crate) fn test_reset_worker_start_count() {
    TEST_WORKER_START_COUNT.store(0, Ordering::SeqCst);
}

#[cfg(test)]
pub(crate) fn test_worker_start_count() -> usize {
    TEST_WORKER_START_COUNT.load(Ordering::SeqCst)
}

/// Throttles snapshots to the refresh rate the active profile asks for; a fast
/// search can otherwise produce far more updates than any terminal can show.
struct ChannelReporter {
    tx: Sender<WorkerEvent>,
    last_update: Instant,
}

impl ChannelReporter {
    fn new(tx: Sender<WorkerEvent>) -> Self {
        Self {
            tx,
            last_update: Instant::now() - Duration::from_secs(1),
        }
    }
}

impl SearchReporter for ChannelReporter {
    fn on_update(&mut self, snapshot: &SearchSnapshot) -> Result<()> {
        // A pause must be shown immediately, whatever the refresh budget says.
        let due =
            self.last_update.elapsed() >= Duration::from_millis(snapshot.metrics.tui_refresh_ms);
        if due || snapshot.run.status == RunStatus::Paused {
            let _ = self
                .tx
                .send(WorkerEvent::Snapshot(Box::new(snapshot.clone())));
            self.last_update = Instant::now();
        }
        Ok(())
    }

    fn on_new_best(&mut self, snapshot: &SearchSnapshot, event: &BestEventRecord) -> Result<()> {
        let _ = self.tx.send(WorkerEvent::NewBest(Box::new(event.clone())));
        let _ = self
            .tx
            .send(WorkerEvent::Snapshot(Box::new(snapshot.clone())));
        self.last_update = Instant::now();
        Ok(())
    }

    fn on_finish(&mut self, snapshot: &SearchSnapshot, reason: FinishReason) -> Result<()> {
        let _ = self
            .tx
            .send(WorkerEvent::Finished(Box::new(snapshot.clone()), reason));
        Ok(())
    }
}
