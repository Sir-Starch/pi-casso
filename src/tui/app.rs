//! Application state and the routing of input to it.
//!
//! `App` used to hold every screen's fields directly and dispatch through two
//! enormous `match` blocks. It now owns one state object per tab and a single
//! action table, and — critically — no input handler here returns `Result`:
//! a failure becomes a toast, never an exit.

use std::collections::VecDeque;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::Modifier;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Clear, Paragraph, Wrap};

use crate::config::Config;
use crate::render::{Theme, finish_message, fmt_count};
use crate::search::{FinishReason, SearchCommand};
use crate::storage::{RunRecord, Storage};
use crate::tui::actions::{Action, ActionContext, Scope, available, resolve};
use crate::tui::form::FormOutcome;
use crate::tui::palette::Palette;
use crate::tui::tabs::{
    Tab, data::DataTab, gallery::GalleryTab, hunt, hunt::HuntTab, runs::RunsTab,
    settings::SettingsTab,
};
use crate::tui::toast::Toasts;
use crate::tui::widgets::{
    Hint, MIN_HEIGHT, MIN_WIDTH, RowRegion, dim_line, modal_area, move_selection, panel,
    render_status_bar, render_tab_bar, render_toasts, render_too_small,
};
use crate::tui::worker::{SearchWorker, WorkerEvent};

/// How many speed samples the sparkline keeps. At the default refresh this is
/// roughly the last minute of the hunt.
const SPEED_HISTORY: usize = 120;

/// Hit areas from the last frame, keyed by what they contain.
#[derive(Default)]
struct Regions {
    tabs: Vec<Rect>,
    /// The scrollable list on the current tab, if it has one.
    list: Option<RowRegion>,
    /// The form on the current tab, if it has one.
    form: Option<RowRegion>,
    /// The command palette's result rows, while it is open.
    palette: Option<RowRegion>,
    /// Clickable buttons in the bottom bar.
    buttons: Vec<(Rect, Action)>,
    /// Confirm-dialog choices, while one is up. `true` is the affirmative.
    confirm: Vec<(Rect, bool)>,
}

pub struct App {
    pub config: Config,
    pub theme: Theme,
    color_enabled: bool,
    pub tab: Tab,
    pub toasts: Toasts,
    pub worker: Option<SearchWorker>,
    pub hunt: HuntTab,
    pub runs: RunsTab,
    pub gallery: GalleryTab,
    pub data: DataTab,
    pub settings: SettingsTab,
    palette: Option<Palette>,
    help: bool,
    speed_history: VecDeque<u64>,
    /// Where each interactive surface was drawn last frame. Mouse handling is
    /// only possible because drawing records this.
    regions: Regions,
    pub should_quit: bool,
    /// Set whenever something changed that the screen has not shown yet.
    pub dirty: bool,
}

impl App {
    pub fn new(config: Config, theme: Theme) -> Self {
        let mut app = Self {
            hunt: HuntTab::new(&config),
            settings: SettingsTab::new(&config),
            color_enabled: theme.color,
            config,
            theme,
            tab: Tab::Hunt,
            toasts: Toasts::default(),
            worker: None,
            runs: RunsTab::default(),
            gallery: GalleryTab::default(),
            data: DataTab::default(),
            palette: None,
            help: false,
            speed_history: VecDeque::with_capacity(SPEED_HISTORY),
            regions: Regions::default(),
            should_quit: false,
            dirty: true,
        };
        if let Err(err) = app.runs.reload() {
            app.toasts.error(format!("could not load runs: {err:#}"));
        }
        app
    }

    fn action_context(&self) -> ActionContext {
        ActionContext {
            tab: self.tab,
            search_active: self.worker.is_some(),
        }
    }

    pub fn max_fps(&self) -> u32 {
        self.config.appearance.max_fps.clamp(1, 120)
    }

    // ---------------------------------------------------------------- input

    pub fn handle_key(&mut self, key: KeyEvent) {
        self.dirty = true;

        // Ctrl+C is the one binding that must work from inside every modal.
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            self.request_quit();
            return;
        }

        if self.help {
            // A modal closes on an explicit key rather than swallowing whatever
            // the user pressed next, which is what the old overlay did.
            if matches!(
                key.code,
                KeyCode::Esc | KeyCode::Enter | KeyCode::Char('?') | KeyCode::Char(' ')
            ) {
                self.help = false;
            }
            return;
        }

        if self.palette.is_some() {
            self.handle_palette_key(key);
            return;
        }

        if self.runs.pending_delete.is_some() {
            self.handle_delete_confirmation(key);
            return;
        }

        // A focused field claims the key before any single-letter shortcut can.
        if self.handle_focused_input(key) {
            return;
        }

        if let Some(action) = resolve(key, self.action_context()) {
            self.perform(action);
            return;
        }

        self.handle_navigation(key);
    }

    /// Gives the active tab's form first refusal on the key.
    /// Returns true when it was consumed.
    fn handle_focused_input(&mut self, key: KeyEvent) -> bool {
        match self.tab {
            Tab::Hunt if self.worker.is_none() => match self.hunt.handle_key(key) {
                FormOutcome::Consumed => true,
                FormOutcome::Submit => {
                    self.start_search();
                    true
                }
                FormOutcome::Ignored => false,
            },
            Tab::Data => self.data.handle_key(key) == FormOutcome::Consumed,
            Tab::Settings => {
                let consumed = self.settings.handle_key(key) == FormOutcome::Consumed;
                if consumed {
                    // Theme and glyph changes are visible immediately; saving is
                    // a separate, explicit step.
                    self.apply_settings_preview();
                }
                consumed
            }
            _ => false,
        }
    }

    fn handle_navigation(&mut self, key: KeyEvent) {
        let (len, state) = match self.tab {
            Tab::Runs => (self.runs.runs.len(), &mut self.runs.state),
            Tab::Gallery => (self.gallery.len(), &mut self.gallery.state),
            _ => return,
        };
        let page = 10;
        match key.code {
            KeyCode::Up | KeyCode::Char('k') => move_selection(state, len, -1),
            KeyCode::Down | KeyCode::Char('j') => move_selection(state, len, 1),
            KeyCode::PageUp => move_selection(state, len, -page),
            KeyCode::PageDown => move_selection(state, len, page),
            KeyCode::Home => move_selection(state, len, -(len as i32)),
            KeyCode::End => move_selection(state, len, len as i32),
            _ => return,
        }
        if self.tab == Tab::Runs {
            if let Err(err) = self.runs.sync_history() {
                self.toasts.error(format!("{err:#}"));
            }
        }
    }

    fn handle_palette_key(&mut self, key: KeyEvent) {
        let context = self.action_context();
        let Some(palette) = self.palette.as_mut() else {
            return;
        };
        match key.code {
            KeyCode::Esc => {
                self.palette = None;
            }
            KeyCode::Up => palette.move_selection(-1),
            KeyCode::Down => palette.move_selection(1),
            KeyCode::Enter => {
                let action = palette.chosen();
                self.palette = None;
                if let Some(action) = action {
                    self.perform(action);
                } else {
                    self.toasts.warn("no matching command");
                }
            }
            _ => {
                if palette.query.handle_key(key) {
                    palette.selected = 0;
                    palette.refresh(context);
                }
            }
        }
    }

    fn handle_delete_confirmation(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Char('y') | KeyCode::Char('Y') | KeyCode::Enter => self.resolve_delete(true),
            KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => self.resolve_delete(false),
            _ => {}
        }
    }

    fn resolve_delete(&mut self, confirmed: bool) {
        self.runs.pending_delete = None;
        self.dirty = true;
        if !confirmed {
            self.toasts.info("delete cancelled");
            return;
        }
        match self.runs.delete_selected() {
            Ok(message) => self.toasts.success(message),
            Err(err) => self.toasts.error(format!("{err:#}")),
        }
    }

    pub fn handle_mouse(&mut self, event: MouseEvent) {
        let (column, row) = (event.column, event.row);
        match event.kind {
            MouseEventKind::Down(MouseButton::Left) => self.handle_click(column, row),
            MouseEventKind::ScrollUp => self.handle_scroll(column, row, -1),
            MouseEventKind::ScrollDown => self.handle_scroll(column, row, 1),
            _ => {}
        }
    }

    fn handle_click(&mut self, column: u16, row: u16) {
        self.dirty = true;

        // A modal owns every click while it is up, so a stray click cannot
        // operate the screen hidden behind it.
        if self.palette.is_some() {
            self.click_palette(column, row);
            return;
        }
        if self.help {
            self.help = false;
            return;
        }
        if self.runs.pending_delete.is_some() {
            for (area, confirmed) in self.regions.confirm.clone() {
                if hits(area, column, row) {
                    self.resolve_delete(confirmed);
                    return;
                }
            }
            return;
        }

        for (area, action) in self.regions.buttons.clone() {
            if hits(area, column, row) {
                self.perform(action);
                return;
            }
        }

        for (index, area) in self.regions.tabs.clone().into_iter().enumerate() {
            if hits(area, column, row) {
                self.goto(Tab::ALL[index]);
                return;
            }
        }

        if let Some(region) = self.regions.list {
            if region.contains(column, row) {
                if let Some(index) = region.index_at(row) {
                    self.click_list(index);
                }
                return;
            }
        }
        if let Some(region) = self.regions.form {
            if region.contains(column, row) {
                if let Some(index) = region.index_at(row) {
                    self.click_form(index);
                }
            }
        }
    }

    fn click_palette(&mut self, column: u16, row: u16) {
        let Some(region) = self.regions.palette else {
            return;
        };
        if !region.contains(column, row) {
            // Clicking outside dismisses, which is what every palette does.
            self.palette = None;
            return;
        }
        let Some(index) = region.index_at(row) else {
            return;
        };
        let action = self.palette.as_mut().and_then(|palette| {
            if index >= palette.matches().len() {
                return None;
            }
            palette.selected = index;
            palette.chosen()
        });
        if let Some(action) = action {
            self.palette = None;
            self.perform(action);
        }
    }

    fn click_list(&mut self, index: usize) {
        let (len, state) = match self.tab {
            Tab::Runs => (self.runs.runs.len(), &mut self.runs.state),
            Tab::Gallery => (self.gallery.len(), &mut self.gallery.state),
            _ => return,
        };
        if index >= len {
            return;
        }
        state.select(Some(index));
        if self.tab == Tab::Runs {
            if let Err(err) = self.runs.sync_history() {
                self.toasts.error(format!("{err:#}"));
            }
        }
    }

    /// A click focuses the field; on a choice or toggle it also advances it, and
    /// on "Start search" it starts. Clicking a value should change the value.
    fn click_form(&mut self, index: usize) {
        let outcome = match self.tab {
            Tab::Hunt if self.worker.is_none() => {
                if self.hunt.form.focus_index(index) {
                    let outcome = self.hunt.form.activate_selected();
                    self.hunt.sync_enabled();
                    outcome
                } else {
                    FormOutcome::Ignored
                }
            }
            Tab::Data => {
                if self.data.form.focus_index(index) {
                    self.data.validated_digits = None;
                    self.data.form.activate_selected()
                } else {
                    FormOutcome::Ignored
                }
            }
            Tab::Settings => {
                if self.settings.form.focus_index(index) {
                    let outcome = self.settings.form.activate_selected();
                    self.settings.dirty = true;
                    self.apply_settings_preview();
                    outcome
                } else {
                    FormOutcome::Ignored
                }
            }
            _ => FormOutcome::Ignored,
        };
        if outcome == FormOutcome::Submit {
            self.start_search();
        }
    }

    fn handle_scroll(&mut self, column: u16, row: u16, delta: i32) {
        self.dirty = true;

        if self.palette.is_some() {
            if let Some(palette) = self.palette.as_mut() {
                palette.move_selection(delta);
            }
            return;
        }

        if let Some(region) = self.regions.list {
            if region.contains(column, row) {
                self.scroll_active_list(delta);
                return;
            }
        }
        if let Some(region) = self.regions.form {
            if region.contains(column, row) {
                // Over a form the wheel moves focus, which is the only kind of
                // scrolling a form has.
                let key = KeyEvent::new(
                    if delta < 0 {
                        KeyCode::Up
                    } else {
                        KeyCode::Down
                    },
                    KeyModifiers::NONE,
                );
                self.handle_focused_input(key);
                return;
            }
        }
        // Anywhere else on a tab that has a list still scrolls it, which is what
        // a user expects after clicking into the details pane.
        self.scroll_active_list(delta);
    }

    fn scroll_active_list(&mut self, delta: i32) {
        let (len, state) = match self.tab {
            Tab::Runs => (self.runs.runs.len(), &mut self.runs.state),
            Tab::Gallery => (self.gallery.len(), &mut self.gallery.state),
            _ => return,
        };
        move_selection(state, len, delta);
        self.dirty = true;
        if self.tab == Tab::Runs {
            if let Err(err) = self.runs.sync_history() {
                self.toasts.error(format!("{err:#}"));
            }
        }
    }

    // -------------------------------------------------------------- actions

    pub fn perform(&mut self, action: Action) {
        self.dirty = true;
        match action {
            Action::Goto(tab) => self.goto(tab),
            Action::NextTab => self.goto(self.tab.step(1)),
            Action::PrevTab => self.goto(self.tab.step(-1)),
            Action::OpenPalette => self.palette = Some(Palette::new(self.action_context())),
            Action::ToggleHelp => self.help = !self.help,
            Action::Quit => self.request_quit(),

            Action::StartSearch => self.start_search(),
            Action::ResetWizard => {
                self.hunt = HuntTab::new(&self.config);
                self.toasts.info("wizard reset to defaults");
            }

            Action::TogglePause => self.toggle_pause(),
            Action::SaveCheckpoint => {
                self.send_to_worker(SearchCommand::SaveCheckpoint, "checkpoint requested");
            }
            Action::SetProfile(profile) => self.send_to_worker(
                SearchCommand::SetProfile(profile),
                format!("profile: {}", profile.as_str()),
            ),
            Action::CycleProfile => self.cycle_profile(),
            Action::AdjustWorkers(delta) => self.send_to_worker(
                SearchCommand::AdjustWorkers(delta),
                if delta > 0 {
                    "more CPU workers"
                } else {
                    "fewer CPU workers"
                },
            ),
            Action::AdjustChunk(delta) => self.send_to_worker(
                SearchCommand::AdjustChunkSize(delta),
                if delta > 0 {
                    "larger chunk"
                } else {
                    "smaller chunk"
                },
            ),
            Action::CycleThermal => {
                self.send_to_worker(SearchCommand::CycleThermalMode, "thermal mode cycled");
            }
            Action::ToggleGpu => {
                self.send_to_worker(SearchCommand::ToggleGpuMode, "gpu mode cycled")
            }
            Action::ToggleMetrics => {
                self.send_to_worker(SearchCommand::ToggleMetrics, "metrics toggled");
            }
            Action::StopSearch => self.stop_search(),
            Action::DismissFinished => self.dismiss_finished(),
            Action::ExportActiveRun => {
                let run = self
                    .worker
                    .as_ref()
                    .and_then(|worker| worker.latest.as_ref())
                    .map(|snapshot| snapshot.run.clone());
                self.export(run);
            }

            Action::ResumeSelectedRun => self.resume_selected(),
            Action::DeleteSelectedRun => match self.runs.selected() {
                Some(run) => {
                    self.runs.pending_delete = Some(run.name.clone());
                }
                None => self.toasts.warn("no run selected"),
            },
            Action::ExportSelectedRun => {
                let run = self.runs.selected().cloned();
                self.export(run);
            }
            Action::RefreshRuns => match self.runs.reload() {
                Ok(()) => self.toasts.info(format!("{} runs", self.runs.runs.len())),
                Err(err) => self.toasts.error(format!("{err:#}")),
            },

            Action::UseSelectedTemplate => match self.gallery.selected_template() {
                Some(template) => {
                    self.hunt.preload_template(template);
                    self.tab = Tab::Hunt;
                    self.toasts.info(format!("wizard loaded with {template}"));
                }
                None => self.toasts.warn("no template selected"),
            },

            Action::ValidateDigitFile => match self.data.validate() {
                Ok(digits) => self.toasts.success(format!(
                    "{} usable digits",
                    fmt_count(digits, self.theme.unicode)
                )),
                Err(err) => self.toasts.error(format!("{err:#}")),
            },
            Action::ImportDigitFile => match self.data.import() {
                Ok((digits, path)) => self.toasts.success(format!(
                    "imported {} digits into {}",
                    fmt_count(digits, self.theme.unicode),
                    path.display()
                )),
                Err(err) => self.toasts.error(format!("{err:#}")),
            },

            Action::CycleTheme => {
                self.settings.cycle_theme();
                self.apply_settings_preview();
            }
            Action::ToggleUnicode => {
                self.settings.toggle_unicode();
                self.apply_settings_preview();
            }
            Action::SaveSettings => match self.settings.save(&self.config) {
                Ok((config, path)) => {
                    self.config = config;
                    self.theme = self.config.theme(self.color_enabled);
                    self.toasts.success(format!("saved {}", path.display()));
                }
                Err(err) => self.toasts.error(format!("{err:#}")),
            },
        }
    }

    fn goto(&mut self, tab: Tab) {
        if self.tab == tab {
            return;
        }
        self.tab = tab;
        // Runs can change under us — another process, or a hunt that just ended.
        if tab == Tab::Runs {
            if let Err(err) = self.runs.reload().and_then(|()| self.runs.sync_history()) {
                self.toasts.error(format!("{err:#}"));
            }
        }
    }

    /// Applies theme changes to the running UI without writing them to disk.
    fn apply_settings_preview(&mut self) {
        let preview = self.settings.apply(&self.config);
        self.theme = preview.theme(self.color_enabled);
    }

    fn send_to_worker(&mut self, command: SearchCommand, message: impl Into<String>) {
        match &self.worker {
            Some(worker) => {
                worker.send(command);
                self.toasts.info(message);
            }
            None => self.toasts.warn("no search is running"),
        }
    }

    /// Steps the live profile one notch, starting from whatever the engine last
    /// reported rather than from a value the UI guessed at.
    fn cycle_profile(&mut self) {
        let Some(current) = self
            .worker
            .as_ref()
            .and_then(|worker| worker.latest.as_ref())
            .map(|snapshot| snapshot.metrics.profile)
        else {
            self.toasts.warn("no search is running");
            return;
        };
        let next = hunt::next_profile(current);
        self.send_to_worker(
            SearchCommand::SetProfile(next),
            format!("profile: {}", next.as_str()),
        );
    }

    fn toggle_pause(&mut self) {
        let Some(worker) = self.worker.as_mut() else {
            self.toasts.warn("no search is running");
            return;
        };
        let command = if worker.paused {
            SearchCommand::Resume
        } else {
            SearchCommand::Pause
        };
        worker.send(command);
        worker.paused = !worker.paused;
        let message = if worker.paused {
            "paused; checkpoint saved"
        } else {
            "resumed"
        };
        self.toasts.info(message);
    }

    fn start_search(&mut self) {
        if self.worker.is_some() {
            self.toasts.warn("a search is already running");
            return;
        }
        match self.hunt.build() {
            Ok((run, options)) => {
                self.speed_history.clear();
                self.toasts.success(format!("hunting: {}", run.name));
                self.worker = Some(SearchWorker::start(run, options));
                self.tab = Tab::Hunt;
            }
            // The exact case that used to terminate the application.
            Err(err) => self.toasts.error(format!("{err:#}")),
        }
    }

    fn resume_selected(&mut self) {
        let Some(run) = self.runs.selected().cloned() else {
            self.toasts.warn("no run selected");
            return;
        };
        self.resume(run);
    }

    fn resume(&mut self, run: RunRecord) {
        if self.worker.is_some() {
            self.toasts.warn("stop the running search first");
            return;
        }
        if let Some(reason) = hunt::resume_blocked_reason(&run) {
            self.toasts.warn(reason);
            return;
        }
        let options = hunt::resume_options(&run);
        self.speed_history.clear();
        self.toasts.success(format!("resuming {}", run.name));
        self.worker = Some(SearchWorker::start(run, options));
        self.tab = Tab::Hunt;
    }

    fn stop_search(&mut self) {
        match self.worker.as_ref() {
            Some(worker) if worker.is_running() => {
                worker.send(SearchCommand::Stop);
                self.toasts.info("stopping; checkpoint will be saved");
            }
            Some(_) => self.dismiss_finished(),
            None => self.toasts.warn("no search is running"),
        }
    }

    fn dismiss_finished(&mut self) {
        let finished = self
            .worker
            .as_ref()
            .map(|worker| !worker.is_running())
            .unwrap_or(false);
        if !finished {
            return;
        }
        self.worker = None;
        self.speed_history.clear();
        if let Err(err) = self.runs.reload() {
            self.toasts.error(format!("{err:#}"));
        }
    }

    fn export(&mut self, run: Option<RunRecord>) {
        let Some(run) = run else {
            self.toasts.warn("no run to export");
            return;
        };
        match export_run(&run, &self.config) {
            Ok(path) => self.toasts.success(format!("exported {}", path.display())),
            Err(err) => self.toasts.error(format!("export failed: {err:#}")),
        }
    }

    fn request_quit(&mut self) {
        match self.worker.as_mut() {
            Some(worker) if worker.is_running() => {
                worker.send(SearchCommand::Stop);
                worker.quit_after_stop = true;
                self.toasts.info("saving checkpoint before exit");
            }
            _ => self.should_quit = true,
        }
    }

    // --------------------------------------------------------------- events

    /// Drains the worker channel. Returns true when anything changed.
    pub fn poll_worker(&mut self) -> bool {
        let Some(worker) = self.worker.as_mut() else {
            return false;
        };
        let events = worker.drain();
        if events.is_empty() {
            return false;
        }

        let mut finished_reason = None;
        let mut new_bests = Vec::new();
        let mut error = None;
        let mut samples = Vec::new();

        for event in events {
            match event {
                WorkerEvent::Snapshot(snapshot) => {
                    worker.paused = snapshot.run.status == crate::storage::RunStatus::Paused;
                    samples.push(snapshot.speed_windows_per_sec.max(0.0) as u64);
                    worker.latest = Some(*snapshot);
                }
                WorkerEvent::NewBest(event) => new_bests.push(*event),
                WorkerEvent::Finished(snapshot, reason) => {
                    worker.latest = Some(*snapshot);
                    worker.finished = Some(reason);
                    finished_reason = Some(reason);
                }
                WorkerEvent::Error(message) => {
                    worker.error = Some(message.clone());
                    error = Some(message);
                }
            }
        }

        let quit_after_stop = worker.quit_after_stop;
        for sample in samples {
            if self.speed_history.len() == SPEED_HISTORY {
                self.speed_history.pop_front();
            }
            self.speed_history.push_back(sample);
        }
        for event in new_bests {
            self.toasts.success(format!(
                "new best {:.2}% at offset {}",
                event.score * 100.0,
                event.offset
            ));
        }
        if let Some(message) = error {
            self.toasts.error(message);
        }
        if let Some(reason) = finished_reason {
            self.on_finished(reason, quit_after_stop);
        }
        true
    }

    fn on_finished(&mut self, reason: FinishReason, quit_after_stop: bool) {
        if quit_after_stop {
            self.should_quit = true;
            return;
        }
        match reason {
            FinishReason::PerfectFound => self.toasts.success(finish_message(reason)),
            FinishReason::SourceExhausted => self.toasts.warn(finish_message(reason)),
            _ => self.toasts.info(finish_message(reason)),
        }
        if let Err(err) = self.runs.reload() {
            self.toasts.error(format!("{err:#}"));
        }
    }

    /// Called on the way out so the worker's final checkpoint lands on disk.
    pub fn shutdown(&mut self) {
        if let Some(worker) = self.worker.as_mut() {
            worker.stop_and_join();
        }
    }

    // ----------------------------------------------------------------- draw

    pub fn draw(&mut self, frame: &mut Frame<'_>) {
        let area = frame.area();
        if area.width < MIN_WIDTH || area.height < MIN_HEIGHT {
            render_too_small(frame, area, &self.theme);
            // Nothing interactive is on screen, so no click should resolve
            // against hit areas left over from a larger frame.
            self.regions = Regions::default();
            return;
        }

        let rows = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(1),
                Constraint::Min(6),
                Constraint::Length(2),
            ])
            .split(area);

        let theme = self.theme;
        let (status, state) = hunt::status_label(
            self.worker
                .as_ref()
                .and_then(|worker| worker.latest.as_ref()),
        );
        // The bullet carries the state at a glance: green while work is
        // happening, red when it genuinely is not, amber otherwise.
        let status_style = if state.is_problem() {
            theme.danger_style()
        } else if state.is_busy() {
            theme.success_style()
        } else {
            theme.warning_style()
        };
        let mut regions = Regions {
            tabs: render_tab_bar(frame, rows[0], self.tab, &status, status_style, &theme),
            ..Regions::default()
        };

        match self.tab {
            Tab::Hunt => match &self.worker {
                Some(worker) => {
                    let history: Vec<u64> = self.speed_history.iter().copied().collect();
                    hunt::draw_dashboard(frame, rows[1], worker.latest.as_ref(), &history, &theme);
                }
                None => regions.form = Some(self.hunt.draw_wizard(frame, rows[1], &theme)),
            },
            Tab::Runs => regions.list = Some(self.runs.draw(frame, rows[1], &theme)),
            Tab::Gallery => regions.list = Some(self.gallery.draw(frame, rows[1], &theme)),
            Tab::Data => regions.form = Some(self.data.draw(frame, rows[1], &theme)),
            Tab::Settings => regions.form = Some(self.settings.draw(frame, rows[1], &theme)),
        }

        regions.buttons = render_status_bar(frame, rows[2], &self.status_hints(), &theme);

        if let Some(name) = self.runs.pending_delete.clone() {
            regions.confirm = self.draw_confirm(frame, area, &name);
        }
        if self.help {
            self.draw_help(frame, area);
        }
        regions.palette = self.draw_palette(frame, area);
        render_toasts(frame, rows[1], &self.toasts, &theme);
        self.regions = regions;
    }

    /// The handful of actions most worth advertising for the current view.
    /// Anything with an action here is a real button: it can be clicked as well
    /// as typed. Everything else is a `ctrl+p` away.
    fn status_hints(&self) -> Vec<Hint<Action>> {
        let mut hints: Vec<Hint<Action>> = Vec::new();
        match self.tab {
            Tab::Hunt if self.worker.is_some() => {
                let finished = self
                    .worker
                    .as_ref()
                    .map(|worker| !worker.is_running())
                    .unwrap_or(false);
                if finished {
                    hints.push(Hint::button("enter", "close", Action::DismissFinished));
                } else {
                    hints.push(Hint::button("space", "pause", Action::TogglePause));
                    hints.push(Hint::button("p", "profile", Action::CycleProfile));
                    hints.push(Hint::note("+/-", "workers"));
                    hints.push(Hint::button("esc", "stop", Action::StopSearch));
                }
                hints.push(Hint::button("e", "export", Action::ExportActiveRun));
            }
            Tab::Hunt => {
                // Start leads, because it is what the screen is for.
                hints.push(Hint::button("F9", "start search", Action::StartSearch));
                hints.push(Hint::note("↑↓", "field"));
                hints.push(Hint::note("←→", "adjust"));
                hints.push(Hint::note("enter", "next"));
            }
            Tab::Runs => {
                hints.push(Hint::button("r", "resume", Action::ResumeSelectedRun));
                hints.push(Hint::button("e", "export", Action::ExportSelectedRun));
                hints.push(Hint::button("d", "delete", Action::DeleteSelectedRun));
                hints.push(Hint::note("↑↓", "select"));
            }
            Tab::Gallery => {
                hints.push(Hint::button(
                    "enter",
                    "hunt this",
                    Action::UseSelectedTemplate,
                ));
                hints.push(Hint::note("↑↓", "select"));
            }
            Tab::Data => {
                hints.push(Hint::button("v", "validate", Action::ValidateDigitFile));
                hints.push(Hint::button("i", "import", Action::ImportDigitFile));
            }
            Tab::Settings => {
                hints.push(Hint::button("ctrl+s", "save", Action::SaveSettings));
                hints.push(Hint::note("←→", "change"));
            }
        }
        hints.push(Hint::button("^p", "commands", Action::OpenPalette));
        // Advertise the key that actually works here: a focused text field
        // consumes '?' as a character.
        hints.push(Hint::button(self.help_key(), "help", Action::ToggleHelp));
        hints
    }

    fn help_key(&self) -> &'static str {
        let text_focused = match self.tab {
            Tab::Hunt => self.worker.is_none() && self.hunt.form.focus_is_text(),
            Tab::Data => self.data.form.focus_is_text(),
            Tab::Settings => self.settings.form.focus_is_text(),
            _ => false,
        };
        if text_focused { "F1" } else { "?" }
    }

    fn draw_confirm(&self, frame: &mut Frame<'_>, area: Rect, name: &str) -> Vec<(Rect, bool)> {
        let popup = modal_area(area, 52, 26);
        frame.render_widget(Clear, popup);
        frame.render_widget(
            Paragraph::new(vec![
                Line::styled(format!("Delete run \"{name}\"?"), self.theme.danger_style()),
                Line::raw(""),
                dim_line("This removes its checkpoint and history.", &self.theme),
            ])
            .wrap(Wrap { trim: true })
            .block(panel("confirm", &self.theme)),
            popup,
        );

        // Clickable choices: a confirmation that can only be answered by
        // keyboard is a dead end for anyone driving with the mouse.
        let labels = [(" Delete ", true), (" Cancel ", false)];
        let row = popup.y + popup.height.saturating_sub(2);
        let mut cursor = popup.x + 2;
        let mut spans = Vec::new();
        let mut buttons = Vec::new();
        for (label, confirmed) in labels {
            let width = label.chars().count() as u16;
            if cursor + width >= popup.x + popup.width {
                break;
            }
            buttons.push((
                Rect {
                    x: cursor,
                    y: row,
                    width,
                    height: 1,
                },
                confirmed,
            ));
            cursor += width + 2;
            spans.push(Span::styled(
                label.to_string(),
                if confirmed {
                    self.theme.danger_style().add_modifier(Modifier::REVERSED)
                } else {
                    self.theme.button_style()
                },
            ));
            spans.push(Span::raw("  "));
        }
        spans.push(Span::styled("  y / n", self.theme.dim_style()));
        frame.render_widget(
            Paragraph::new(Line::from(spans)),
            Rect {
                x: popup.x + 2,
                y: row,
                width: popup.width.saturating_sub(4),
                height: 1,
            },
        );
        buttons
    }

    fn draw_help(&self, frame: &mut Frame<'_>, area: Rect) {
        let context = self.action_context();
        // Generated from the same registry the keys resolve against, so help can
        // never describe a binding that no longer exists.
        let mut lines = Vec::new();
        for scope_label in ["this view", "everywhere"] {
            let global = scope_label == "everywhere";
            let entries: Vec<_> = available(context)
                .filter(|command| (command.scope == Scope::Global) == global)
                .filter(|command| !command.keys.is_empty())
                .collect();
            if entries.is_empty() {
                continue;
            }
            lines.push(Line::styled(scope_label, self.theme.accent_style()));
            for command in entries {
                lines.push(Line::from(vec![
                    Span::styled(
                        format!("  {:<10}", command.keys),
                        self.theme.warning_style(),
                    ),
                    Span::styled(command.title.to_string(), self.theme.text_style()),
                    Span::styled(
                        if command.hint.is_empty() {
                            String::new()
                        } else {
                            format!("  — {}", command.hint)
                        },
                        self.theme.dim_style(),
                    ),
                ]));
            }
            lines.push(Line::raw(""));
        }
        lines.push(dim_line(
            "ctrl+p lists every command, including those without a key.",
            &self.theme,
        ));

        let popup = modal_area(area, 78, 76);
        frame.render_widget(Clear, popup);
        frame.render_widget(
            Paragraph::new(lines)
                .wrap(Wrap { trim: true })
                .block(panel("help", &self.theme)),
            popup,
        );
    }

    fn draw_palette(&self, frame: &mut Frame<'_>, area: Rect) -> Option<RowRegion> {
        let palette = self.palette.as_ref()?;
        let popup = modal_area(area, 66, 60);
        frame.render_widget(Clear, popup);
        frame.render_widget(panel("command palette", &self.theme), popup);

        let inner = popup.inner(ratatui::layout::Margin {
            horizontal: 2,
            vertical: 1,
        });
        let rows = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(2), Constraint::Min(1)])
            .split(inner);

        frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled("> ", self.theme.accent_style()),
                Span::styled(
                    palette.query.display_with_cursor(true),
                    self.theme.text_style(),
                ),
            ])),
            rows[0],
        );

        let visible = rows[1].height as usize;
        // Keep the selection on screen without a full stateful list.
        let start = palette.selected.saturating_sub(visible.saturating_sub(1));
        let lines: Vec<Line<'static>> = palette
            .matches()
            .iter()
            .enumerate()
            .skip(start)
            .take(visible)
            .map(|(index, entry)| {
                let selected = index == palette.selected;
                Line::from(vec![
                    Span::styled(
                        format!(
                            "{} {:<28}",
                            if selected {
                                self.theme.glyphs.arrow
                            } else {
                                " "
                            },
                            entry.command.title
                        ),
                        if selected {
                            self.theme.selected_style()
                        } else {
                            self.theme.text_style()
                        },
                    ),
                    Span::styled(
                        format!("{:<10}", entry.command.keys),
                        self.theme.warning_style(),
                    ),
                    Span::styled(entry.command.hint.to_string(), self.theme.dim_style()),
                ])
            })
            .collect();
        let empty = lines.is_empty();
        let lines = if empty {
            vec![dim_line("no matching command", &self.theme)]
        } else {
            lines
        };
        frame.render_widget(Paragraph::new(lines), rows[1]);
        // The whole modal is the click target so a click outside it can dismiss,
        // while row hits still resolve against the result list.
        Some(RowRegion {
            area: popup,
            first_index: if empty { usize::MAX } else { start },
            top_inset: rows[1].y - popup.y,
        })
    }
}

/// Exports go to a directory the app owns, not to whatever the working
/// directory happened to be when the TUI was launched.
fn export_run(run: &RunRecord, config: &Config) -> anyhow::Result<std::path::PathBuf> {
    let storage = Storage::open_default()?;
    let history = storage.history(&run.id, None)?;
    let directory = config.export_dir()?;
    std::fs::create_dir_all(&directory)?;
    let path = directory.join(format!("{}.json", sanitize_filename(&run.name)));
    let body = serde_json::to_string_pretty(&serde_json::json!({
        "run": run,
        "history": history,
    }))?;
    std::fs::write(&path, body)?;
    Ok(path)
}

fn hits(area: Rect, column: u16, row: u16) -> bool {
    column >= area.x && column < area.x + area.width && row >= area.y && row < area.y + area.height
}

fn sanitize_filename(value: &str) -> String {
    let sanitized: String = value
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_') {
                ch
            } else {
                '-'
            }
        })
        .collect();
    let trimmed = sanitized.trim_matches('-').to_string();
    if trimmed.is_empty() {
        "run".to_string()
    } else {
        trimmed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filenames_lose_path_separators_and_spaces() {
        assert_eq!(sanitize_filename("arch eternal"), "arch-eternal");
        assert_eq!(sanitize_filename("../../etc/passwd"), "etc-passwd");
        assert_eq!(sanitize_filename("keep_this-1"), "keep_this-1");
    }

    #[test]
    fn a_name_of_only_punctuation_still_yields_a_filename() {
        assert_eq!(sanitize_filename("///"), "run");
        assert_eq!(sanitize_filename(""), "run");
    }
}
