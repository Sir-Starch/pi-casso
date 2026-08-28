//! The interactive terminal application.
//!
//! - [`app`] — state and input routing
//! - [`actions`] — the one registry behind keys, palette and help
//! - [`tabs`] — one module per top-level view
//! - [`widgets`] — shared chrome
//! - [`live`] — the standalone search view used by the CLI commands

pub mod actions;
pub mod app;
pub mod form;
pub mod input;
pub mod live;
pub mod palette;
pub mod tabs;
pub mod terminal;
pub mod toast;
pub mod widgets;
pub mod worker;

use std::time::{Duration, Instant};

use anyhow::Result;
use crossterm::event::{self, Event, KeyEventKind};

use crate::benchmark_contract::BackendResolution;
use crate::commands::CommandContext;
use crate::performance::PerformanceSnapshot;
use crate::search::SearchOptions;
use crate::storage::RunRecord;
use crate::tui::app::App;
use crate::tui::terminal::{TerminalGuard, install_panic_hook};

/// Redraw at least this often while idle, so a clock or a slowly-growing cache
/// does not look frozen.
const IDLE_REDRAW: Duration = Duration::from_millis(500);

#[derive(Clone, Debug)]
pub struct PreparedResume {
    pub run: RunRecord,
    pub snapshot: PerformanceSnapshot,
    pub options: SearchOptions,
    pub capability: BackendResolution,
}

#[derive(Clone, Debug, Default)]
pub struct TuiLaunch {
    pub prepared_resume: Option<PreparedResume>,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct FramePolicy {
    max_fps: u32,
    ui_refresh_ms: u64,
}

impl FramePolicy {
    pub(crate) fn new(max_fps: u32, ui_refresh_ms: u64) -> Self {
        Self {
            max_fps: max_fps.clamp(1, 120),
            ui_refresh_ms: ui_refresh_ms.clamp(16, 60_000),
        }
    }

    pub(crate) fn from_prepared_resume(prepared: &PreparedResume) -> Self {
        let limits = &prepared.snapshot.settings.limits;
        Self::new(limits.max_fps, limits.ui_refresh_ms)
    }

    pub(crate) fn frame_interval(self) -> Duration {
        Duration::from_millis(u64::from(1_000_u32.div_ceil(self.max_fps)))
    }

    pub(crate) fn metrics_interval(self) -> Duration {
        Duration::from_millis(self.ui_refresh_ms)
    }

    pub(crate) fn redraw_due(self, elapsed: Duration, dirty: bool, full_redraw: bool) -> bool {
        full_redraw
            || (dirty || elapsed >= self.metrics_interval()) && elapsed >= self.frame_interval()
    }
}

pub fn run(context: CommandContext) -> Result<()> {
    run_with_launch(context, TuiLaunch::default())
}

pub fn run_with_launch(context: CommandContext, launch: TuiLaunch) -> Result<()> {
    install_panic_hook();
    let mut app = App::new_with_launch(context.config, context.theme, launch);
    if !app.toasts.is_empty() {
        // A config warning raised during startup is worth a moment on screen.
    }
    let mut guard = TerminalGuard::new(true)?;
    let result = event_loop(&mut app, &mut guard);
    app.shutdown();
    // The guard restores the terminal on drop, including on the error path.
    drop(guard);
    result
}

fn event_loop(app: &mut App, guard: &mut TerminalGuard) -> Result<()> {
    let mut last_draw = Instant::now() - IDLE_REDRAW;
    loop {
        if app.poll_worker() {
            app.dirty = true;
        }
        if app.toasts.prune() {
            app.dirty = true;
        }

        let policy = app.frame_policy();
        let elapsed = last_draw.elapsed();
        let full_redraw = app.take_handoff_full_redraw();
        if policy.redraw_due(elapsed, app.dirty, full_redraw) {
            if full_redraw {
                guard.terminal.clear()?;
            }
            guard.terminal.draw(|frame| app.draw(frame))?;
            app.dirty = false;
            last_draw = Instant::now();
        }

        if app.should_quit {
            return Ok(());
        }

        let frame_wait = policy.frame_interval().saturating_sub(last_draw.elapsed());
        let metrics_wait = policy
            .metrics_interval()
            .saturating_sub(last_draw.elapsed());
        let input_wait = Duration::from_millis(16);
        let poll_wait = frame_wait.min(metrics_wait).min(input_wait);
        if !event::poll(poll_wait)? {
            continue;
        }
        match event::read()? {
            // Windows reports both Press and Release for every key; without this
            // filter each keystroke would act twice there.
            Event::Key(key) if key.kind == KeyEventKind::Press => app.handle_key(key),
            Event::Mouse(mouse) => app.handle_mouse(mouse),
            Event::Resize(_, _) => app.dirty = true,
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_policy_when_snapshot_values_are_distinct_uses_each_value_for_its_own_deadline() {
        // Given: a restored resume snapshot with deliberately distinct UI values.
        let policy = FramePolicy::new(60, 1_000);

        // When: the event loop asks for its redraw and metrics deadlines.
        let frame_interval = policy.frame_interval();
        let metrics_interval = policy.metrics_interval();

        // Then: FPS governs frames while ui_refresh_ms governs metrics refresh.
        assert_eq!(frame_interval, Duration::from_millis(17));
        assert_eq!(metrics_interval, Duration::from_millis(1_000));
    }

    #[test]
    fn resume_handoff_redraw_bypasses_the_frame_deadline() {
        // Given: a just-rendered frame and a deliberately slow restored frame policy.
        let policy = FramePolicy::new(1, 1_000);

        // When: in-app resume requests the one full redraw carrying handoff markers.
        let redraw_due = policy.redraw_due(Duration::ZERO, true, true);

        // Then: the handoff redraw is immediate rather than rate-limited.
        assert!(redraw_due);
    }
}
