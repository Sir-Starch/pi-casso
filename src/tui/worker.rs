//! The background search thread and the channel that carries its progress back
//! to the UI.

use std::sync::mpsc::{self, Receiver, Sender};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use anyhow::Result;

use crate::search::{
    FinishReason, SearchCommand, SearchOptions, SearchReporter, SearchSnapshot,
    run_search_controlled,
};
use crate::storage::{BestEventRecord, RunRecord, RunStatus, Storage};

pub enum WorkerEvent {
    Snapshot(Box<SearchSnapshot>),
    NewBest(Box<BestEventRecord>),
    Finished(Box<SearchSnapshot>, FinishReason),
    Error(String),
}

pub struct SearchWorker {
    control: Sender<SearchCommand>,
    events: Receiver<WorkerEvent>,
    handle: Option<JoinHandle<()>>,
    pub latest: Option<SearchSnapshot>,
    pub paused: bool,
    pub finished: Option<FinishReason>,
    pub error: Option<String>,
    /// Set when the user asked to leave the app; the worker is given the chance
    /// to checkpoint before the process exits.
    pub quit_after_stop: bool,
}

impl SearchWorker {
    pub fn start(run: RunRecord, options: SearchOptions) -> Self {
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
            latest: None,
            paused: false,
            finished: None,
            error: None,
            quit_after_stop: false,
        }
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
