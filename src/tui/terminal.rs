//! Terminal setup and — more importantly — teardown.
//!
//! Raw mode and the alternate screen must be undone on every exit path,
//! including a panic. A panic that escapes with raw mode still on leaves the
//! user with an unusable shell and no visible backtrace.

use std::io::{self, Stdout};

use anyhow::Result;
use crossterm::event::{DisableMouseCapture, EnableMouseCapture};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;

pub type Backend = CrosstermBackend<Stdout>;

pub struct TerminalGuard {
    pub terminal: Terminal<Backend>,
}

impl TerminalGuard {
    pub fn new(mouse: bool) -> Result<Self> {
        enable_raw_mode()?;
        let mut stdout = io::stdout();
        execute!(stdout, EnterAlternateScreen)?;
        if mouse {
            execute!(stdout, EnableMouseCapture)?;
        }
        let mut terminal = Terminal::new(CrosstermBackend::new(stdout))?;
        terminal.hide_cursor()?;
        terminal.clear()?;
        Ok(Self { terminal })
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        restore();
    }
}

/// Idempotent and infallible: every teardown path calls it, including the panic
/// hook, where returning an error would be useless.
pub fn restore() {
    let _ = disable_raw_mode();
    let mut stdout = io::stdout();
    let _ = execute!(stdout, DisableMouseCapture);
    let _ = execute!(stdout, LeaveAlternateScreen);
    let _ = crossterm::execute!(stdout, crossterm::cursor::Show);
}

/// Restores the terminal before the default hook prints, so the panic message
/// lands on a usable screen instead of the alternate one that is about to
/// disappear.
pub fn install_panic_hook() {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        restore();
        previous(info);
    }));
}
