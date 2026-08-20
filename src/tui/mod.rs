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

use crate::commands::CommandContext;
use crate::tui::app::App;
use crate::tui::terminal::{TerminalGuard, install_panic_hook};

/// Redraw at least this often while idle, so a clock or a slowly-growing cache
/// does not look frozen.
const IDLE_REDRAW: Duration = Duration::from_millis(500);

pub fn run(context: CommandContext) -> Result<()> {
    install_panic_hook();
    let mut app = App::new(context.config, context.theme);
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

        if app.dirty || last_draw.elapsed() >= IDLE_REDRAW {
            guard.terminal.draw(|frame| app.draw(frame))?;
            app.dirty = false;
            last_draw = Instant::now();
        }

        if app.should_quit {
            return Ok(());
        }

        // The frame budget doubles as the input poll timeout: the loop never
        // spins, and never redraws faster than the configured ceiling.
        let budget = Duration::from_millis((1000 / app.max_fps().max(1)) as u64);
        if !event::poll(budget)? {
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
