use std::io::{self, Stdout};
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow};
use chrono::Utc;
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyModifiers};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Block, BorderType, Borders, Clear, Gauge, List, ListItem, Padding, Paragraph, Wrap,
};

use crate::art::{self, ArtMapping, Bitmap};
use crate::digits::{DigitSource, DigitSourceSpec, FileDigitSource};
use crate::performance::{
    GeneratorBackendChoice, GpuMode, PerformanceOverrides, PerformanceProfile, PerformanceSettings,
    SearchBackendChoice, ThermalMode,
};
use crate::pi;
use crate::search::{
    FinishReason, MatchMode, SearchCommand, SearchOptions, SearchReporter, SearchSnapshot,
    run_search_controlled,
};
use crate::storage::{self, BestEventRecord, NewRun, RunRecord, RunStatus, Storage};

const HOME_MENU: [&str; 7] = [
    "New Search",
    "Resume Search",
    "Saved Runs",
    "Templates",
    "Import Pi Digits",
    "Settings",
    "Quit",
];

const FORM_FIELDS: [&str; 14] = [
    "Source",
    "Template / Art file",
    "Run name",
    "Pi source",
    "Pi digit file",
    "Size mode",
    "Width",
    "Height",
    "Threshold",
    "Performance",
    "Workers",
    "Limit",
    "Allow decimal prefix",
    "Start search",
];

const PI_SOURCE_MODES: [&str; 3] = ["infinite generated pi", "pi digit file", "demo sample"];
const PERFORMANCE_PROFILES: [PerformanceProfile; 4] = [
    PerformanceProfile::Eco,
    PerformanceProfile::Balanced,
    PerformanceProfile::Performance,
    PerformanceProfile::Max,
];
const DEFAULT_PROFILE_IDX: usize = 1;

const SIZE_MODES: [(&str, usize, usize); 4] = [
    ("8x8", 8, 8),
    ("12x12", 12, 12),
    ("16x16", 16, 16),
    ("custom", 16, 16),
];

const DETAIL_ACTIONS: [&str; 4] = ["Resume", "Export JSON", "Delete", "Back"];

fn interactive_performance(
    match_mode: MatchMode,
    workers: Option<usize>,
    profile: PerformanceProfile,
) -> PerformanceSettings {
    PerformanceSettings::from_profile(
        profile,
        SearchBackendChoice::Auto,
        GeneratorBackendChoice::Auto,
        GpuMode::Auto,
        None,
        ThermalMode::Normal,
        false,
        true,
        match_mode,
        PerformanceOverrides {
            cpu_workers: workers,
            checkpoint_every_secs: Some(5),
            ..PerformanceOverrides::default()
        },
    )
}

pub fn run(color: bool) -> Result<()> {
    let mut terminal = InteractiveTerminal::new()?;
    let mut app = App::new(color);

    loop {
        app.poll_worker();
        terminal.terminal.draw(|frame| app.draw(frame))?;

        if app.should_quit {
            break;
        }

        if event::poll(Duration::from_millis(100))? {
            if let Event::Key(key) = event::read()? {
                app.handle_key(key)?;
            }
        }
    }

    app.stop_worker_for_shutdown();
    Ok(())
}

struct InteractiveTerminal {
    terminal: Terminal<CrosstermBackend<Stdout>>,
}

impl InteractiveTerminal {
    fn new() -> Result<Self> {
        enable_raw_mode()?;
        let mut stdout = io::stdout();
        execute!(stdout, EnterAlternateScreen)?;
        let backend = CrosstermBackend::new(stdout);
        let mut terminal = Terminal::new(backend)?;
        terminal.clear()?;
        Ok(Self { terminal })
    }
}

impl Drop for InteractiveTerminal {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let _ = execute!(self.terminal.backend_mut(), LeaveAlternateScreen);
        let _ = self.terminal.show_cursor();
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Screen {
    Home,
    NewSearch,
    Dashboard,
    SavedRuns,
    RunDetails,
    Templates,
    Import,
    Settings,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum WizardSource {
    Template,
    CustomFile,
}

struct NewSearchForm {
    selected: usize,
    source: WizardSource,
    template_idx: usize,
    custom_file: String,
    name: String,
    pi_source_idx: usize,
    pi_file: String,
    size_mode_idx: usize,
    width: String,
    height: String,
    threshold: String,
    profile_idx: usize,
    workers: String,
    limit: String,
    allow_decimal_prefix: bool,
}

impl Default for NewSearchForm {
    fn default() -> Self {
        Self {
            selected: 0,
            source: WizardSource::Template,
            template_idx: 0,
            custom_file: String::new(),
            name: String::new(),
            pi_source_idx: 0,
            pi_file: String::new(),
            size_mode_idx: 1,
            width: "12".to_string(),
            height: "12".to_string(),
            threshold: "5".to_string(),
            profile_idx: DEFAULT_PROFILE_IDX,
            workers: String::new(),
            limit: String::new(),
            allow_decimal_prefix: false,
        }
    }
}

#[derive(Default)]
struct ImportForm {
    path: String,
    allow_decimal_prefix: bool,
    digits: Option<u64>,
    selected: usize,
}

struct App {
    color: bool,
    screen: Screen,
    home_idx: usize,
    saved_idx: usize,
    template_idx: usize,
    detail_action_idx: usize,
    runs: Vec<RunRecord>,
    selected_run: Option<RunRecord>,
    selected_history: Vec<BestEventRecord>,
    form: NewSearchForm,
    import_form: ImportForm,
    help: bool,
    confirm_delete: bool,
    message: Option<String>,
    worker: Option<SearchWorker>,
    should_quit: bool,
}

impl App {
    fn new(color: bool) -> Self {
        let mut app = Self {
            color,
            screen: Screen::Home,
            home_idx: 0,
            saved_idx: 0,
            template_idx: 0,
            detail_action_idx: 0,
            runs: Vec::new(),
            selected_run: None,
            selected_history: Vec::new(),
            form: NewSearchForm::default(),
            import_form: ImportForm::default(),
            help: false,
            confirm_delete: false,
            message: None,
            worker: None,
            should_quit: false,
        };
        app.refresh_runs();
        app
    }

    fn draw(&self, frame: &mut ratatui::Frame<'_>) {
        let area = frame.area();
        match self.screen {
            Screen::Home => self.draw_home(frame, area),
            Screen::NewSearch => self.draw_new_search(frame, area),
            Screen::Dashboard => self.draw_dashboard(frame, area),
            Screen::SavedRuns => self.draw_saved_runs(frame, area),
            Screen::RunDetails => self.draw_run_details(frame, area),
            Screen::Templates => self.draw_templates(frame, area),
            Screen::Import => self.draw_import(frame, area),
            Screen::Settings => self.draw_settings(frame, area),
        }
        if self.help {
            self.draw_help(frame, area);
        }
        if let Some(message) = &self.message {
            self.draw_message(frame, area, message);
        }
    }

    fn draw_home(&self, frame: &mut ratatui::Frame<'_>, area: Rect) {
        let rows = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(5),
                Constraint::Length(6),
                Constraint::Min(8),
                Constraint::Length(2),
            ])
            .split(area);
        let title = Paragraph::new(vec![
            Line::from(vec![
                Span::styled("pi", accent(self.color)),
                Span::styled("-casso", Style::default().add_modifier(Modifier::BOLD)),
                Span::styled("  /  ", dim(self.color)),
                Span::styled("visual emergence hunter for pi digits", dim(self.color)),
            ]),
            Line::from(vec![
                Span::styled("mode ", dim(self.color)),
                Span::styled("interactive", success(self.color)),
                Span::styled("   source ", dim(self.color)),
                Span::styled("cache + file + y-cruncher", accent(self.color)),
                Span::styled("   controls ", dim(self.color)),
                Span::styled("j/k enter q ?", warning(self.color)),
            ]),
        ])
        .alignment(Alignment::Center)
        .block(panel(" pi-casso ", self.color));
        frame.render_widget(title, rows[0]);

        let best = self
            .runs
            .iter()
            .map(|run| run.best_score)
            .fold(0.0_f64, f64::max);
        let active = self
            .runs
            .iter()
            .filter(|run| matches!(run.status, RunStatus::Running | RunStatus::Paused))
            .count();
        let stats = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([
                Constraint::Percentage(25),
                Constraint::Percentage(25),
                Constraint::Percentage(25),
                Constraint::Percentage(25),
            ])
            .split(rows[1]);
        frame.render_widget(
            metric_card(
                "runs",
                self.runs.len().to_string(),
                "saved hunts",
                self.color,
            ),
            stats[0],
        );
        frame.render_widget(
            metric_card("active", active.to_string(), "running/paused", self.color),
            stats[1],
        );
        frame.render_widget(
            metric_card(
                "best",
                format!("{:.2}%", best * 100.0),
                "top score",
                self.color,
            ),
            stats[2],
        );
        frame.render_widget(
            metric_card(
                "default",
                "12x12 emergence",
                "infinite pi cache",
                self.color,
            ),
            stats[3],
        );

        let columns = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Length(28), Constraint::Min(20)])
            .split(rows[2]);
        frame.render_widget(
            selectable_list("Menu", &HOME_MENU, self.home_idx, self.color),
            columns[0],
        );
        frame.render_widget(
            Paragraph::new(run_table_lines(&self.runs, 12, self.color))
                .block(panel("saved runs", self.color))
                .wrap(Wrap { trim: false }),
            columns[1],
        );
        frame.render_widget(
            footer_bar(
                "Up/Down or j/k navigate | Enter select | ? help | q quit",
                self.color,
            ),
            rows[3],
        );
    }

    fn draw_new_search(&self, frame: &mut ratatui::Frame<'_>, area: Rect) {
        let rows = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Min(12), Constraint::Length(3)])
            .split(area);
        let columns = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(52), Constraint::Percentage(48)])
            .split(rows[0]);

        let items: Vec<ListItem<'_>> = FORM_FIELDS
            .iter()
            .enumerate()
            .map(|(idx, label)| {
                let value = self.form_value(idx);
                let marker = if idx == self.form.selected { ">" } else { " " };
                let style = if idx == self.form.selected {
                    warning(self.color)
                } else {
                    Style::default().fg(fg(self.color, Color::Gray, Color::White))
                };
                ListItem::new(Line::from(vec![
                    Span::styled(format!("{marker} {label:<22}"), style),
                    Span::styled(value, accent(self.color)),
                ]))
            })
            .collect();
        frame.render_widget(
            List::new(items).block(panel("new search wizard", self.color)),
            columns[0],
        );

        let preview = self.preview_form_bitmap();
        let preview_lines = match preview {
            Ok(bitmap) => bitmap_to_lines(&bitmap, self.color, None),
            Err(err) => vec![Line::raw(err.to_string())],
        };
        frame.render_widget(
            Paragraph::new(preview_lines).block(panel("live preview", self.color)),
            columns[1],
        );
        frame.render_widget(
            footer_bar(
                "Up/Down move | type to edit | Left/Right adjust | Space toggle | Enter advances/starts | Esc back | ? help",
                self.color,
            ),
            rows[1],
        );
    }

    fn draw_dashboard(&self, frame: &mut ratatui::Frame<'_>, area: Rect) {
        let rows = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(4),
                Constraint::Length(5),
                Constraint::Min(10),
                Constraint::Length(8),
                Constraint::Length(2),
            ])
            .split(area);
        let Some(worker) = &self.worker else {
            frame.render_widget(Paragraph::new("no active search"), area);
            return;
        };
        let snapshot = worker.latest.as_ref();
        let header_lines = if let Some(snapshot) = snapshot {
            dashboard_header_lines(snapshot, self.color)
        } else {
            vec![
                Line::styled("starting search worker", accent(self.color)),
                Line::styled("waiting for first checkpoint", dim(self.color)),
            ]
        };
        frame.render_widget(
            Paragraph::new(header_lines)
                .block(panel("search dashboard", self.color))
                .wrap(Wrap { trim: true }),
            rows[0],
        );

        if let Some(snapshot) = snapshot {
            let cards = Layout::default()
                .direction(Direction::Horizontal)
                .constraints([
                    Constraint::Percentage(20),
                    Constraint::Percentage(20),
                    Constraint::Percentage(20),
                    Constraint::Percentage(20),
                    Constraint::Percentage(20),
                ])
                .split(rows[1]);
            frame.render_widget(
                metric_card(
                    "offset",
                    snapshot.run.current_offset.to_string(),
                    format!("{} scanned", snapshot.run.scanned_windows),
                    self.color,
                ),
                cards[0],
            );
            frame.render_widget(
                metric_card(
                    "speed",
                    format!("{}/s", fmt_rate(snapshot.speed_windows_per_sec)),
                    format!("{}/s digits", fmt_rate(snapshot.speed_digits_per_sec)),
                    self.color,
                ),
                cards[1],
            );
            frame.render_widget(
                Gauge::default()
                    .block(panel("best score", self.color))
                    .ratio(snapshot.run.best_score.clamp(0.0, 1.0))
                    .label(Span::styled(
                        format!("{:.2}%", snapshot.run.best_score * 100.0),
                        Style::default().add_modifier(Modifier::BOLD),
                    ))
                    .gauge_style(success(self.color)),
                cards[2],
            );
            frame.render_widget(
                metric_card(
                    "cache",
                    snapshot.source_len.to_string(),
                    if snapshot.source_is_growing {
                        format!("gap {}", snapshot.cache_gap_digits)
                    } else {
                        snapshot.source_kind.clone()
                    },
                    self.color,
                ),
                cards[3],
            );
            frame.render_widget(
                metric_card(
                    "engine",
                    snapshot.metrics.search_backend.clone(),
                    format!(
                        "{} workers / {}",
                        snapshot.metrics.cpu_workers,
                        snapshot.metrics.profile.as_str()
                    ),
                    self.color,
                ),
                cards[4],
            );
        }

        let columns = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
            .split(rows[2]);
        if let Some(snapshot) = snapshot {
            frame.render_widget(
                Paragraph::new(bitmap_to_lines(
                    &snapshot.run.target_bitmap,
                    self.color,
                    None,
                ))
                .block(panel("target mask", self.color)),
                columns[0],
            );
            let best = best_match_lines(&snapshot.run, self.color);
            frame.render_widget(
                Paragraph::new(best).block(panel("best pi manifestation", self.color)),
                columns[1],
            );
            frame.render_widget(
                Paragraph::new(history_lines(&snapshot.recent_events))
                    .block(panel("recent improvements", self.color)),
                rows[3],
            );
        }
        let footer = if worker.finished.is_some() {
            "Search finished | e export | q/backspace return home | h help"
        } else {
            "1 eco 2 balanced 3 performance 4 max | +/- workers | [] chunk | Space pause | q save quit"
        };
        frame.render_widget(footer_bar(footer, self.color), rows[4]);
    }

    fn draw_saved_runs(&self, frame: &mut ratatui::Frame<'_>, area: Rect) {
        let rows = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Min(10), Constraint::Length(2)])
            .split(area);
        let items: Vec<ListItem<'_>> = if self.runs.is_empty() {
            vec![ListItem::new("no saved runs")]
        } else {
            self.runs
                .iter()
                .enumerate()
                .map(|(idx, run)| {
                    let style = if idx == self.saved_idx && self.color {
                        Style::default()
                            .fg(Color::Yellow)
                            .add_modifier(Modifier::BOLD)
                    } else {
                        Style::default()
                    };
                    ListItem::new(Line::styled(
                        format!(
                            "{:<18} {:<8} {:>7.2}% offset={:<10} updated={}",
                            truncate(&run.name, 18),
                            run.status.as_str(),
                            run.best_score * 100.0,
                            run.current_offset,
                            run.updated_at
                        ),
                        style,
                    ))
                })
                .collect()
        };
        frame.render_widget(
            List::new(items).block(panel("saved runs", self.color)),
            rows[0],
        );
        frame.render_widget(
            footer_bar(
                "Enter details | r resume | d delete | e export | Backspace/q back",
                self.color,
            ),
            rows[1],
        );
    }

    fn draw_run_details(&self, frame: &mut ratatui::Frame<'_>, area: Rect) {
        let rows = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(10),
                Constraint::Min(10),
                Constraint::Length(7),
            ])
            .split(area);
        let Some(run) = &self.selected_run else {
            frame.render_widget(Paragraph::new("no run selected"), area);
            return;
        };
        frame.render_widget(
            Paragraph::new(run_metadata_lines(run)).block(panel("run details", self.color)),
            rows[0],
        );
        let columns = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
            .split(rows[1]);
        frame.render_widget(
            Paragraph::new(bitmap_to_lines(&run.target_bitmap, self.color, None))
                .block(panel("target", self.color)),
            columns[0],
        );
        let best = run
            .best_bitmap
            .as_ref()
            .map(|bitmap| {
                bitmap_to_lines(
                    bitmap,
                    self.color,
                    Some((&run.target_bitmap, run.best_inverted)),
                )
            })
            .unwrap_or_else(|| vec![Line::raw("no best match yet")]);
        frame.render_widget(
            Paragraph::new(best).block(panel("best", self.color)),
            columns[1],
        );

        let action_lines: Vec<Line<'_>> = DETAIL_ACTIONS
            .iter()
            .enumerate()
            .map(|(idx, action)| {
                let style = if idx == self.detail_action_idx && self.color {
                    Style::default()
                        .fg(Color::Yellow)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default()
                };
                Line::styled(
                    format!(
                        "{} {}",
                        if idx == self.detail_action_idx {
                            ">"
                        } else {
                            " "
                        },
                        action
                    ),
                    style,
                )
            })
            .chain(history_lines(&self.selected_history).into_iter().take(2))
            .collect();
        frame.render_widget(
            Paragraph::new(action_lines).block(panel("actions / history", self.color)),
            rows[2],
        );
    }

    fn draw_templates(&self, frame: &mut ratatui::Frame<'_>, area: Rect) {
        let rows = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Min(8), Constraint::Length(2)])
            .split(area);
        let columns = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Length(24), Constraint::Min(20)])
            .split(rows[0]);
        frame.render_widget(
            selectable_list(
                "Templates",
                art::template_names(),
                self.template_idx,
                self.color,
            ),
            columns[0],
        );
        let name = art::template_names()[self.template_idx];
        let preview = art::load_template(name, 16, 16)
            .map(|bitmap| bitmap_to_lines(&bitmap, self.color, None))
            .unwrap_or_else(|err| vec![Line::raw(err.to_string())]);
        frame.render_widget(
            Paragraph::new(preview).block(panel("preview", self.color)),
            columns[1],
        );
        frame.render_widget(
            footer_bar(
                "Enter starts a new run from selected template | Backspace/q back",
                self.color,
            ),
            rows[1],
        );
    }

    fn draw_import(&self, frame: &mut ratatui::Frame<'_>, area: Rect) {
        let lines = vec![
            Line::raw("Import a pi digit file into the app cache."),
            Line::raw(
                "ASCII whitespace is ignored. '.' is rejected unless decimal prefix is enabled.",
            ),
            Line::raw(""),
            selected_line(
                0,
                self.import_form.selected,
                format!("Path: {}", self.import_form.path),
                self.color,
            ),
            selected_line(
                1,
                self.import_form.selected,
                format!(
                    "Allow decimal prefix: {}",
                    yes_no(self.import_form.allow_decimal_prefix)
                ),
                self.color,
            ),
            Line::raw(format!(
                "Usable digits: {}",
                self.import_form
                    .digits
                    .map(|digits| digits.to_string())
                    .unwrap_or_else(|| "-".to_string())
            )),
            Line::raw(""),
            Line::raw("v validate | i import | Space toggle | Backspace edit/back | q back"),
        ];
        frame.render_widget(
            Paragraph::new(lines)
                .block(panel("import pi digits", self.color))
                .wrap(Wrap { trim: false }),
            area,
        );
    }

    fn draw_settings(&self, frame: &mut ratatui::Frame<'_>, area: Rect) {
        let db = storage::db_path()
            .map(|path| path.display().to_string())
            .unwrap_or_else(|err| err.to_string());
        let cache = pi::cache_path()
            .map(|path| path.display().to_string())
            .unwrap_or_else(|err| err.to_string());
        let lines = vec![
            Line::raw("Settings"),
            Line::raw(""),
            Line::raw(format!("Database: {db}")),
            Line::raw(format!("Pi cache: {cache}")),
            Line::raw(format!("Color: {}", yes_no(self.color))),
            Line::raw(""),
            Line::raw("Backspace/q returns home."),
        ];
        frame.render_widget(
            Paragraph::new(lines).block(panel("settings", self.color)),
            area,
        );
    }

    fn draw_help(&self, frame: &mut ratatui::Frame<'_>, area: Rect) {
        let popup = centered_rect(72, 60, area);
        frame.render_widget(Clear, popup);
        frame.render_widget(
            Paragraph::new(help_lines(self.screen))
                .block(panel("help", self.color))
                .wrap(Wrap { trim: true }),
            popup,
        );
    }

    fn draw_message(&self, frame: &mut ratatui::Frame<'_>, area: Rect, message: &str) {
        let popup = Rect {
            x: area.x + 2,
            y: area.y + area.height.saturating_sub(4),
            width: area.width.saturating_sub(4),
            height: 3,
        };
        frame.render_widget(Clear, popup);
        frame.render_widget(
            Paragraph::new(message.to_string())
                .block(panel("message", self.color))
                .wrap(Wrap { trim: true }),
            popup,
        );
    }

    fn handle_key(&mut self, key: KeyEvent) -> Result<()> {
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            self.handle_ctrl_c();
            return Ok(());
        }

        if (key.code == KeyCode::Char('?') || key.code == KeyCode::Char('h'))
            && (self.screen != Screen::Dashboard || key.code == KeyCode::Char('h'))
        {
            self.help = !self.help;
            self.message = None;
            return Ok(());
        }

        if self.help {
            self.help = false;
            return Ok(());
        }

        match self.screen {
            Screen::Home => self.handle_home_key(key)?,
            Screen::NewSearch => self.handle_new_search_key(key)?,
            Screen::Dashboard => self.handle_dashboard_key(key),
            Screen::SavedRuns => self.handle_saved_runs_key(key)?,
            Screen::RunDetails => self.handle_run_details_key(key)?,
            Screen::Templates => self.handle_templates_key(key),
            Screen::Import => self.handle_import_key(key)?,
            Screen::Settings => self.handle_settings_key(key),
        }
        Ok(())
    }

    fn handle_home_key(&mut self, key: KeyEvent) -> Result<()> {
        match key.code {
            KeyCode::Char('q') => self.should_quit = true,
            KeyCode::Up | KeyCode::Char('k') => self.home_idx = self.home_idx.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') => {
                self.home_idx = (self.home_idx + 1).min(HOME_MENU.len() - 1);
            }
            KeyCode::Enter => match self.home_idx {
                0 => self.screen = Screen::NewSearch,
                1 => self.resume_first_available()?,
                2 => {
                    self.refresh_runs();
                    self.screen = Screen::SavedRuns;
                }
                3 => self.screen = Screen::Templates,
                4 => self.screen = Screen::Import,
                5 => self.screen = Screen::Settings,
                6 => self.should_quit = true,
                _ => {}
            },
            _ => {}
        }
        Ok(())
    }

    fn handle_new_search_key(&mut self, key: KeyEvent) -> Result<()> {
        match key.code {
            KeyCode::Esc => self.screen = Screen::Home,
            KeyCode::Up | KeyCode::Char('k') => {
                self.form.selected = self.form.selected.saturating_sub(1);
            }
            KeyCode::Down | KeyCode::Char('j') | KeyCode::Tab => {
                self.form.selected = (self.form.selected + 1).min(FORM_FIELDS.len() - 1);
            }
            KeyCode::Left => self.adjust_form_field(-1),
            KeyCode::Right => self.adjust_form_field(1),
            KeyCode::Enter => {
                if self.form.selected == FORM_FIELDS.len() - 1 {
                    self.create_and_start_search()?;
                } else {
                    self.form.selected = (self.form.selected + 1).min(FORM_FIELDS.len() - 1);
                }
            }
            KeyCode::Char(' ') => self.toggle_form_field(),
            KeyCode::Backspace => self.backspace_form_field(),
            KeyCode::Char(ch) => self.push_form_char(ch),
            _ => {}
        }
        Ok(())
    }

    fn handle_dashboard_key(&mut self, key: KeyEvent) {
        if self
            .worker
            .as_ref()
            .and_then(|worker| worker.finished)
            .is_some()
        {
            match key.code {
                KeyCode::Char('q') | KeyCode::Backspace | KeyCode::Esc => {
                    self.worker = None;
                    self.refresh_runs();
                    self.screen = Screen::Home;
                    self.message = None;
                }
                KeyCode::Char('e') => {
                    if let Some(run) = self.active_run() {
                        self.message = Some(match export_run_json(&run) {
                            Ok(path) => format!("exported {}", path.display()),
                            Err(err) => format!("export failed: {err:#}"),
                        });
                    }
                }
                _ => {}
            }
            return;
        }

        match key.code {
            KeyCode::Char(' ') | KeyCode::Char('p') => {
                if let Some(worker) = &mut self.worker {
                    let command = if worker.paused {
                        SearchCommand::Resume
                    } else {
                        SearchCommand::Pause
                    };
                    let _ = worker.control_tx.send(command);
                    worker.paused = !worker.paused;
                    self.message = Some(if worker.paused {
                        "pause requested; checkpoint saved".to_string()
                    } else {
                        "resume requested".to_string()
                    });
                }
            }
            KeyCode::Char('1') => self.send_search_command(
                SearchCommand::SetProfile(PerformanceProfile::Eco),
                "profile: eco",
            ),
            KeyCode::Char('2') => self.send_search_command(
                SearchCommand::SetProfile(PerformanceProfile::Balanced),
                "profile: balanced",
            ),
            KeyCode::Char('3') => self.send_search_command(
                SearchCommand::SetProfile(PerformanceProfile::Performance),
                "profile: performance",
            ),
            KeyCode::Char('4') => self.send_search_command(
                SearchCommand::SetProfile(PerformanceProfile::Max),
                "profile: max",
            ),
            KeyCode::Char('+') => {
                self.send_search_command(SearchCommand::AdjustWorkers(1), "worker count increased");
            }
            KeyCode::Char('-') => {
                self.send_search_command(
                    SearchCommand::AdjustWorkers(-1),
                    "worker count decreased",
                );
            }
            KeyCode::Char(']') => {
                self.send_search_command(SearchCommand::AdjustChunkSize(1), "chunk size increased");
            }
            KeyCode::Char('[') => {
                self.send_search_command(
                    SearchCommand::AdjustChunkSize(-1),
                    "chunk size decreased",
                );
            }
            KeyCode::Char('t') => {
                self.send_search_command(SearchCommand::CycleThermalMode, "thermal mode cycled");
            }
            KeyCode::Char('m') => {
                self.send_search_command(SearchCommand::ToggleMetrics, "metrics panel toggled");
            }
            KeyCode::Char('g') => {
                self.send_search_command(SearchCommand::ToggleGpuMode, "gpu mode toggled");
            }
            KeyCode::Char('s') => {
                if let Some(worker) = &self.worker {
                    let _ = worker.control_tx.send(SearchCommand::SaveCheckpoint);
                    self.message = Some("checkpoint requested".to_string());
                }
            }
            KeyCode::Char('e') => {
                if let Some(run) = self.active_run() {
                    self.message = Some(match export_run_json(&run) {
                        Ok(path) => format!("exported {}", path.display()),
                        Err(err) => format!("export failed: {err:#}"),
                    });
                }
            }
            KeyCode::Char('q') => {
                if let Some(worker) = &mut self.worker {
                    let _ = worker.control_tx.send(SearchCommand::Stop);
                    worker.pending_quit = true;
                    self.message = Some("saving checkpoint and stopping search".to_string());
                } else {
                    self.should_quit = true;
                }
            }
            _ => {}
        }
    }

    fn send_search_command(&mut self, command: SearchCommand, message: &str) {
        if let Some(worker) = &self.worker {
            let _ = worker.control_tx.send(command);
            self.message = Some(message.to_string());
        }
    }

    fn handle_saved_runs_key(&mut self, key: KeyEvent) -> Result<()> {
        if self.confirm_delete {
            match key.code {
                KeyCode::Char('y') => {
                    self.delete_selected_run()?;
                    self.confirm_delete = false;
                }
                KeyCode::Char('n') | KeyCode::Esc => {
                    self.confirm_delete = false;
                    self.message = Some("delete cancelled".to_string());
                }
                _ => {}
            }
            return Ok(());
        }

        match key.code {
            KeyCode::Char('q') | KeyCode::Backspace | KeyCode::Esc => self.screen = Screen::Home,
            KeyCode::Up | KeyCode::Char('k') => self.saved_idx = self.saved_idx.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') => {
                self.saved_idx = (self.saved_idx + 1).min(self.runs.len().saturating_sub(1));
            }
            KeyCode::Enter => self.open_selected_details()?,
            KeyCode::Char('r') => self.resume_selected_run()?,
            KeyCode::Char('d') if self.selected_saved_run().is_some() => {
                self.confirm_delete = true;
                self.message = Some("delete run? y/n".to_string());
            }
            KeyCode::Char('e') => {
                if let Some(run) = self.selected_saved_run() {
                    self.message = Some(match export_run_json(run) {
                        Ok(path) => format!("exported {}", path.display()),
                        Err(err) => format!("export failed: {err:#}"),
                    });
                }
            }
            _ => {}
        }
        Ok(())
    }

    fn handle_run_details_key(&mut self, key: KeyEvent) -> Result<()> {
        if self.confirm_delete {
            match key.code {
                KeyCode::Char('y') => {
                    if let Some(run) = &self.selected_run {
                        let mut storage = Storage::open_default()?;
                        storage.delete_run(&run.id)?;
                        self.message = Some(format!("deleted {}", run.name));
                    }
                    self.confirm_delete = false;
                    self.selected_run = None;
                    self.refresh_runs();
                    self.screen = Screen::SavedRuns;
                }
                KeyCode::Char('n') | KeyCode::Esc => {
                    self.confirm_delete = false;
                    self.message = Some("delete cancelled".to_string());
                }
                _ => {}
            }
            return Ok(());
        }

        match key.code {
            KeyCode::Char('q') | KeyCode::Backspace | KeyCode::Esc => {
                self.screen = Screen::SavedRuns
            }
            KeyCode::Up | KeyCode::Char('k') => {
                self.detail_action_idx = self.detail_action_idx.saturating_sub(1);
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.detail_action_idx = (self.detail_action_idx + 1).min(DETAIL_ACTIONS.len() - 1);
            }
            KeyCode::Char('r') => self.resume_detail_run()?,
            KeyCode::Char('e') => self.export_detail_run(),
            KeyCode::Char('d') => {
                self.confirm_delete = true;
                self.message = Some("delete run? y/n".to_string());
            }
            KeyCode::Enter => match self.detail_action_idx {
                0 => self.resume_detail_run()?,
                1 => self.export_detail_run(),
                2 => {
                    self.confirm_delete = true;
                    self.message = Some("delete run? y/n".to_string());
                }
                3 => self.screen = Screen::SavedRuns,
                _ => {}
            },
            _ => {}
        }
        Ok(())
    }

    fn handle_templates_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Char('q') | KeyCode::Backspace | KeyCode::Esc => self.screen = Screen::Home,
            KeyCode::Up | KeyCode::Char('k') => {
                self.template_idx = self.template_idx.saturating_sub(1);
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.template_idx =
                    (self.template_idx + 1).min(art::template_names().len().saturating_sub(1));
            }
            KeyCode::Enter => {
                self.form = NewSearchForm::default();
                self.form.source = WizardSource::Template;
                self.form.template_idx = self.template_idx;
                self.form.name = format!("{}-hunt", art::template_names()[self.template_idx]);
                self.screen = Screen::NewSearch;
            }
            _ => {}
        }
    }

    fn handle_import_key(&mut self, key: KeyEvent) -> Result<()> {
        match key.code {
            KeyCode::Char('q') | KeyCode::Esc => self.screen = Screen::Home,
            KeyCode::Up | KeyCode::Char('k') => {
                self.import_form.selected = self.import_form.selected.saturating_sub(1);
            }
            KeyCode::Down | KeyCode::Char('j') | KeyCode::Tab => {
                self.import_form.selected = (self.import_form.selected + 1).min(1);
            }
            KeyCode::Char(' ') => {
                if self.import_form.selected == 1 {
                    self.import_form.allow_decimal_prefix = !self.import_form.allow_decimal_prefix;
                    self.import_form.digits = None;
                } else {
                    self.import_form.path.push(' ');
                }
            }
            KeyCode::Backspace => {
                if self.import_form.selected == 0 && !self.import_form.path.is_empty() {
                    self.import_form.path.pop();
                    self.import_form.digits = None;
                } else {
                    self.screen = Screen::Home;
                }
            }
            KeyCode::Char('v') => self.validate_import_path(),
            KeyCode::Char('i') => self.import_pi_digits()?,
            KeyCode::Char(ch) if self.import_form.selected == 0 => {
                self.import_form.path.push(ch);
                self.import_form.digits = None;
            }
            _ => {}
        }
        Ok(())
    }

    fn handle_settings_key(&mut self, key: KeyEvent) {
        if matches!(
            key.code,
            KeyCode::Char('q') | KeyCode::Backspace | KeyCode::Esc
        ) {
            self.screen = Screen::Home;
        }
    }

    fn handle_ctrl_c(&mut self) {
        if let Some(worker) = &mut self.worker {
            let _ = worker.control_tx.send(SearchCommand::Stop);
            worker.pending_quit = true;
            self.message = Some("Ctrl+C received; saving checkpoint".to_string());
        } else {
            self.should_quit = true;
        }
    }

    fn poll_worker(&mut self) {
        let mut finished = false;
        let mut pending_quit = false;
        if let Some(worker) = &mut self.worker {
            while let Ok(event) = worker.event_rx.try_recv() {
                match event {
                    SearchWorkerEvent::Snapshot(snapshot) => {
                        worker.paused = snapshot.run.status == RunStatus::Paused;
                        worker.latest = Some(snapshot);
                    }
                    SearchWorkerEvent::NewBest(event) => {
                        self.message = Some(format!(
                            "new best {:.2}% at offset {}",
                            event.score * 100.0,
                            event.offset
                        ));
                    }
                    SearchWorkerEvent::Finished(snapshot, reason) => {
                        worker.finished = Some(reason);
                        worker.latest = Some(snapshot);
                        pending_quit = worker.pending_quit;
                        self.message = Some(finish_message(reason).to_string());
                        finished = true;
                    }
                    SearchWorkerEvent::Error(error) => {
                        worker.error = Some(error.clone());
                        self.message = Some(error);
                        finished = true;
                    }
                }
            }
        }

        if finished {
            if pending_quit {
                self.should_quit = true;
            } else {
                self.refresh_runs();
            }
        }
    }

    fn stop_worker_for_shutdown(&mut self) {
        if let Some(worker) = &mut self.worker {
            let _ = worker.control_tx.send(SearchCommand::Stop);
            if let Some(handle) = worker.handle.take() {
                let _ = handle.join();
            }
        }
    }

    fn refresh_runs(&mut self) {
        match Storage::open_default().and_then(|storage| storage.list_runs()) {
            Ok(runs) => {
                self.runs = runs;
                if !self.runs.is_empty() {
                    self.saved_idx = self.saved_idx.min(self.runs.len() - 1);
                }
            }
            Err(err) => self.message = Some(format!("could not load runs: {err:#}")),
        }
    }

    fn selected_saved_run(&self) -> Option<&RunRecord> {
        self.runs.get(self.saved_idx)
    }

    fn open_selected_details(&mut self) -> Result<()> {
        let Some(run) = self.selected_saved_run().cloned() else {
            return Ok(());
        };
        let storage = Storage::open_default()?;
        self.selected_history = storage.history(&run.id, Some(10))?;
        self.selected_run = Some(run);
        self.detail_action_idx = 0;
        self.screen = Screen::RunDetails;
        Ok(())
    }

    fn delete_selected_run(&mut self) -> Result<()> {
        let Some(run) = self.selected_saved_run().cloned() else {
            return Ok(());
        };
        let mut storage = Storage::open_default()?;
        storage.delete_run(&run.id)?;
        self.message = Some(format!("deleted {}", run.name));
        self.refresh_runs();
        Ok(())
    }

    fn resume_first_available(&mut self) -> Result<()> {
        self.refresh_runs();
        let Some(run) = self
            .runs
            .iter()
            .find(|run| {
                !matches!(
                    run.status,
                    RunStatus::PerfectFound | RunStatus::SourceExhausted
                )
            })
            .cloned()
        else {
            self.message = Some("no resumable runs found".to_string());
            return Ok(());
        };
        self.start_existing_run(run);
        Ok(())
    }

    fn resume_selected_run(&mut self) -> Result<()> {
        let Some(run) = self.selected_saved_run().cloned() else {
            return Ok(());
        };
        self.start_existing_run(run);
        Ok(())
    }

    fn resume_detail_run(&mut self) -> Result<()> {
        let Some(run) = self.selected_run.clone() else {
            return Ok(());
        };
        self.start_existing_run(run);
        Ok(())
    }

    fn start_existing_run(&mut self, run: RunRecord) {
        if matches!(
            run.status,
            RunStatus::PerfectFound | RunStatus::SourceExhausted
        ) {
            self.message = Some(format!("{} is already {}", run.name, run.status.as_str()));
            return;
        }
        let top_n = run.top_matches.len().max(10);
        let performance =
            interactive_performance(run.match_mode, None, PerformanceProfile::Performance);
        let options = SearchOptions {
            max_offset: None,
            limit: None,
            match_mode: run.match_mode,
            canvas_width: run.canvas_width as usize,
            canvas_height: run.canvas_height as usize,
            threshold: run.threshold,
            invert: run.invert_enabled,
            workers: None,
            checkpoint_every: Duration::from_secs(performance.limits.checkpoint_every_secs),
            top_n,
            keep_going_after_perfect: false,
            chunk_windows: performance.limits.chunk_size,
            performance,
        };
        self.worker = Some(SearchWorker::start(run, options));
        self.screen = Screen::Dashboard;
        self.message = None;
    }

    fn create_and_start_search(&mut self) -> Result<()> {
        let width = parse_usize_field(&self.form.width, "width")?;
        let height = parse_usize_field(&self.form.height, "height")?;
        let threshold = parse_u8_field(&self.form.threshold, "threshold")?;
        if threshold > 9 {
            return Err(anyhow!("threshold must be between 0 and 9"));
        }
        let workers = parse_optional_usize(&self.form.workers, "workers")?;
        let limit = parse_optional_u64(&self.form.limit, "limit")?;
        let match_mode = MatchMode::Emergence;
        let canvas_width = 24usize.max(width);
        let canvas_height = 24usize.max(height);
        let name = if self.form.name.trim().is_empty() {
            format!("pi-casso-{}", Utc::now().timestamp())
        } else {
            self.form.name.trim().to_string()
        };
        let target = match self.form.source {
            WizardSource::Template => {
                let template = art::template_names()[self.form.template_idx];
                art::load_template(template, width, height)?
            }
            WizardSource::CustomFile => {
                let path = PathBuf::from(self.form.custom_file.trim());
                let contents = std::fs::read_to_string(&path)
                    .with_context(|| format!("failed to read {}", path.display()))?;
                Bitmap::from_ascii(&contents, width, height, &ArtMapping::default())?
            }
        };
        let cache = pi::PiCache::default()?;
        let (source, generated_digit_count) = match self.form.pi_source_idx {
            0 => {
                cache.ensure_parent()?;
                (
                    DigitSourceSpec::cache(cache.path().clone()),
                    cache.digit_count()?,
                )
            }
            1 => {
                if self.form.pi_file.trim().is_empty() {
                    return Err(anyhow!(
                        "enter a pi digit file path, or choose infinite generated pi"
                    ));
                }
                let path = PathBuf::from(self.form.pi_file.trim())
                    .canonicalize()
                    .with_context(|| format!("could not resolve {}", self.form.pi_file.trim()))?;
                FileDigitSource::new_with_options(path.clone(), self.form.allow_decimal_prefix)
                    .validate()?;
                (
                    DigitSourceSpec::file(path, self.form.allow_decimal_prefix),
                    0,
                )
            }
            2 => (DigitSourceSpec::demo(), 0),
            _ => unreachable!("invalid pi source mode"),
        };
        let source_impl = source.open()?;
        let _ = source_impl.len()?;
        let mut storage = Storage::open_default()?;
        let template_name = if self.form.source == WizardSource::Template {
            Some(art::template_names()[self.form.template_idx].to_string())
        } else {
            None
        };
        let profile = PERFORMANCE_PROFILES[self.form.profile_idx];
        let params = serde_json::json!({
            "limit": limit,
            "workers": workers,
            "allow_decimal_prefix": self.form.allow_decimal_prefix,
            "infinite": self.form.pi_source_idx == 0,
            "match_mode": match_mode.as_str(),
            "canvas_width": canvas_width,
            "canvas_height": canvas_height,
            "interactive": true,
            "profile": profile.as_str(),
        });
        let run = storage.create_run(NewRun {
            name,
            source,
            template_name,
            art_hash: target.sha256(),
            width: target.width as u32,
            height: target.height as u32,
            canvas_width: canvas_width as u32,
            canvas_height: canvas_height as u32,
            match_mode,
            threshold,
            invert_enabled: false,
            start_offset: Some(0),
            target_bitmap: target,
            generated_digit_count,
            params_json: params.to_string(),
        })?;
        let performance = interactive_performance(match_mode, workers, profile);
        let options = SearchOptions {
            max_offset: None,
            limit,
            match_mode,
            canvas_width,
            canvas_height,
            threshold,
            invert: false,
            workers,
            checkpoint_every: Duration::from_secs(performance.limits.checkpoint_every_secs),
            top_n: 10,
            keep_going_after_perfect: false,
            chunk_windows: performance.limits.chunk_size,
            performance,
        };
        self.worker = Some(SearchWorker::start(run, options));
        self.screen = Screen::Dashboard;
        self.message = None;
        Ok(())
    }

    fn active_run(&self) -> Option<RunRecord> {
        self.worker
            .as_ref()
            .and_then(|worker| worker.latest.as_ref().map(|snapshot| snapshot.run.clone()))
            .or_else(|| self.selected_run.clone())
    }

    fn export_detail_run(&mut self) {
        if let Some(run) = &self.selected_run {
            self.message = Some(match export_run_json(run) {
                Ok(path) => format!("exported {}", path.display()),
                Err(err) => format!("export failed: {err:#}"),
            });
        }
    }

    fn validate_import_path(&mut self) {
        let path = PathBuf::from(self.import_form.path.trim());
        let source = FileDigitSource::new_with_options(path, self.import_form.allow_decimal_prefix);
        match source.validate().and_then(|()| source.len()) {
            Ok(digits) => {
                self.import_form.digits = Some(digits);
                self.message = Some(format!("{digits} usable digits"));
            }
            Err(err) => {
                self.import_form.digits = None;
                self.message = Some(format!("{err:#}"));
            }
        }
    }

    fn import_pi_digits(&mut self) -> Result<()> {
        let path = PathBuf::from(self.import_form.path.trim());
        let source = FileDigitSource::new_with_options(path, self.import_form.allow_decimal_prefix);
        let cache = pi::PiCache::default()?;
        let dest = cache.path().clone();
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent)?;
        }
        match source.copy_digits_to(&dest) {
            Ok(digits) => {
                self.import_form.digits = Some(digits);
                self.message = Some(format!("imported {digits} digits into {}", dest.display()));
            }
            Err(err) => self.message = Some(format!("{err:#}")),
        }
        Ok(())
    }

    fn form_value(&self, idx: usize) -> String {
        match idx {
            0 => match self.form.source {
                WizardSource::Template => "built-in template".to_string(),
                WizardSource::CustomFile => "custom ASCII-art file".to_string(),
            },
            1 => match self.form.source {
                WizardSource::Template => art::template_names()[self.form.template_idx].to_string(),
                WizardSource::CustomFile => self.form.custom_file.clone(),
            },
            2 => self.form.name.clone(),
            3 => PI_SOURCE_MODES[self.form.pi_source_idx].to_string(),
            4 => match self.form.pi_source_idx {
                0 => "local cache, generated as needed".to_string(),
                1 if self.form.pi_file.is_empty() => "<enter pi digit file>".to_string(),
                1 => self.form.pi_file.clone(),
                2 => "tiny built-in demo, exhausts quickly".to_string(),
                _ => String::new(),
            },
            5 => SIZE_MODES[self.form.size_mode_idx].0.to_string(),
            6 => self.form.width.clone(),
            7 => self.form.height.clone(),
            8 => self.form.threshold.clone(),
            9 => PERFORMANCE_PROFILES[self.form.profile_idx]
                .as_str()
                .to_string(),
            10 => {
                if self.form.workers.is_empty() {
                    "auto".to_string()
                } else {
                    self.form.workers.clone()
                }
            }
            11 => {
                if self.form.limit.is_empty() {
                    "unbounded".to_string()
                } else {
                    self.form.limit.clone()
                }
            }
            12 => yes_no(self.form.allow_decimal_prefix).to_string(),
            13 => "press Enter".to_string(),
            _ => String::new(),
        }
    }

    fn preview_form_bitmap(&self) -> Result<Bitmap> {
        let width = parse_usize_field(&self.form.width, "width").unwrap_or(16);
        let height = parse_usize_field(&self.form.height, "height").unwrap_or(16);
        match self.form.source {
            WizardSource::Template => {
                art::load_template(art::template_names()[self.form.template_idx], width, height)
            }
            WizardSource::CustomFile => {
                if self.form.custom_file.trim().is_empty() {
                    return Err(anyhow!("enter an ASCII-art file path"));
                }
                let path = PathBuf::from(self.form.custom_file.trim());
                let contents = std::fs::read_to_string(&path)
                    .with_context(|| format!("failed to read {}", path.display()))?;
                Bitmap::from_ascii(&contents, width, height, &ArtMapping::default())
            }
        }
    }

    fn adjust_form_field(&mut self, delta: i32) {
        match self.form.selected {
            0 => self.toggle_form_field(),
            1 if self.form.source == WizardSource::Template => {
                self.form.template_idx =
                    wrap_index(self.form.template_idx, art::template_names().len(), delta);
            }
            3 => self.adjust_pi_source_mode(delta),
            5 => self.adjust_size_mode(delta),
            6 => {
                adjust_numeric_string(&mut self.form.width, delta, 1, 128);
                self.form.size_mode_idx = 3;
            }
            7 => {
                adjust_numeric_string(&mut self.form.height, delta, 1, 128);
                self.form.size_mode_idx = 3;
            }
            8 => adjust_numeric_string(&mut self.form.threshold, delta, 0, 9),
            9 => {
                self.form.profile_idx =
                    wrap_index(self.form.profile_idx, PERFORMANCE_PROFILES.len(), delta);
            }
            12 => self.toggle_form_field(),
            _ => {}
        }
    }

    fn adjust_pi_source_mode(&mut self, delta: i32) {
        self.form.pi_source_idx = wrap_index(self.form.pi_source_idx, PI_SOURCE_MODES.len(), delta);
        if self.form.pi_source_idx == 0 && self.form.size_mode_idx != 3 {
            self.form.size_mode_idx = 1;
            self.form.width = "12".to_string();
            self.form.height = "12".to_string();
        }
    }

    fn adjust_size_mode(&mut self, delta: i32) {
        self.form.size_mode_idx = wrap_index(self.form.size_mode_idx, SIZE_MODES.len(), delta);
        let (_, width, height) = SIZE_MODES[self.form.size_mode_idx];
        self.form.width = width.to_string();
        self.form.height = height.to_string();
    }

    fn toggle_form_field(&mut self) {
        match self.form.selected {
            0 => {
                self.form.source = match self.form.source {
                    WizardSource::Template => WizardSource::CustomFile,
                    WizardSource::CustomFile => WizardSource::Template,
                };
            }
            3 => self.adjust_pi_source_mode(1),
            5 => self.adjust_size_mode(1),
            9 => {
                self.form.profile_idx =
                    wrap_index(self.form.profile_idx, PERFORMANCE_PROFILES.len(), 1);
            }
            12 => self.form.allow_decimal_prefix = !self.form.allow_decimal_prefix,
            _ => {}
        }
    }

    fn push_form_char(&mut self, ch: char) {
        match self.form.selected {
            1 if self.form.source == WizardSource::CustomFile => self.form.custom_file.push(ch),
            2 => self.form.name.push(ch),
            4 if self.form.pi_source_idx == 1 => self.form.pi_file.push(ch),
            6 => {
                push_digit(&mut self.form.width, ch);
                self.form.size_mode_idx = 3;
            }
            7 => {
                push_digit(&mut self.form.height, ch);
                self.form.size_mode_idx = 3;
            }
            8 => push_digit(&mut self.form.threshold, ch),
            10 => push_digit(&mut self.form.workers, ch),
            11 => push_digit(&mut self.form.limit, ch),
            _ => {}
        }
    }

    fn backspace_form_field(&mut self) {
        match self.form.selected {
            1 if self.form.source == WizardSource::CustomFile => {
                self.form.custom_file.pop();
            }
            2 => {
                self.form.name.pop();
            }
            4 if self.form.pi_source_idx == 1 => {
                self.form.pi_file.pop();
            }
            6 => {
                self.form.width.pop();
                self.form.size_mode_idx = 3;
            }
            7 => {
                self.form.height.pop();
                self.form.size_mode_idx = 3;
            }
            8 => {
                self.form.threshold.pop();
            }
            10 => {
                self.form.workers.pop();
            }
            11 => {
                self.form.limit.pop();
            }
            _ => {}
        }
    }
}

struct SearchWorker {
    control_tx: Sender<SearchCommand>,
    event_rx: Receiver<SearchWorkerEvent>,
    handle: Option<JoinHandle<()>>,
    latest: Option<SearchSnapshot>,
    paused: bool,
    pending_quit: bool,
    finished: Option<FinishReason>,
    error: Option<String>,
}

impl SearchWorker {
    fn start(run: RunRecord, options: SearchOptions) -> Self {
        let (control_tx, control_rx) = mpsc::channel();
        let (event_tx, event_rx) = mpsc::channel();
        let handle = thread::spawn(move || {
            let result = (|| -> Result<()> {
                let mut storage = Storage::open_default()?;
                let source = run.source.open()?;
                let mut reporter = ChannelReporter::new(event_tx.clone());
                let search_result = run_search_controlled(
                    &mut storage,
                    run,
                    source.as_ref(),
                    options,
                    &mut reporter,
                    control_rx,
                );
                search_result.map(|_| ())
            })();
            if let Err(err) = result {
                let _ = event_tx.send(SearchWorkerEvent::Error(format!("{err:#}")));
            }
        });

        Self {
            control_tx,
            event_rx,
            handle: Some(handle),
            latest: None,
            paused: false,
            pending_quit: false,
            finished: None,
            error: None,
        }
    }
}

enum SearchWorkerEvent {
    Snapshot(SearchSnapshot),
    NewBest(BestEventRecord),
    Finished(SearchSnapshot, FinishReason),
    Error(String),
}

struct ChannelReporter {
    tx: Sender<SearchWorkerEvent>,
    last_update: Instant,
}

impl ChannelReporter {
    fn new(tx: Sender<SearchWorkerEvent>) -> Self {
        Self {
            tx,
            last_update: Instant::now() - Duration::from_secs(1),
        }
    }
}

impl SearchReporter for ChannelReporter {
    fn on_update(&mut self, snapshot: &SearchSnapshot) -> Result<()> {
        if self.last_update.elapsed() >= Duration::from_millis(snapshot.metrics.tui_refresh_ms)
            || snapshot.run.status == RunStatus::Paused
        {
            let _ = self.tx.send(SearchWorkerEvent::Snapshot(snapshot.clone()));
            self.last_update = Instant::now();
        }
        Ok(())
    }

    fn on_new_best(&mut self, snapshot: &SearchSnapshot, event: &BestEventRecord) -> Result<()> {
        let _ = self.tx.send(SearchWorkerEvent::NewBest(event.clone()));
        let _ = self.tx.send(SearchWorkerEvent::Snapshot(snapshot.clone()));
        self.last_update = Instant::now();
        Ok(())
    }

    fn on_finish(&mut self, snapshot: &SearchSnapshot, reason: FinishReason) -> Result<()> {
        let _ = self
            .tx
            .send(SearchWorkerEvent::Finished(snapshot.clone(), reason));
        Ok(())
    }
}

fn fg(color: bool, enabled: Color, fallback: Color) -> Color {
    if color { enabled } else { fallback }
}

fn accent(color: bool) -> Style {
    Style::default()
        .fg(fg(color, Color::Cyan, Color::White))
        .add_modifier(Modifier::BOLD)
}

fn dim(color: bool) -> Style {
    Style::default().fg(fg(color, Color::DarkGray, Color::White))
}

fn success(color: bool) -> Style {
    Style::default()
        .fg(fg(color, Color::Green, Color::White))
        .add_modifier(Modifier::BOLD)
}

fn warning(color: bool) -> Style {
    Style::default()
        .fg(fg(color, Color::Yellow, Color::White))
        .add_modifier(Modifier::BOLD)
}

fn danger(color: bool) -> Style {
    Style::default()
        .fg(fg(color, Color::Red, Color::White))
        .add_modifier(Modifier::BOLD)
}

fn panel(title: impl Into<String>, color: bool) -> Block<'static> {
    Block::default()
        .title(Line::from(vec![Span::styled(
            format!(" {} ", title.into()),
            accent(color),
        )]))
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(fg(color, Color::DarkGray, Color::White)))
        .padding(Padding::horizontal(1))
}

fn footer_bar(text: impl Into<String>, color: bool) -> Paragraph<'static> {
    Paragraph::new(Line::from(vec![
        Span::styled(" keys ", dim(color)),
        Span::styled(
            text.into(),
            Style::default().fg(fg(color, Color::Gray, Color::White)),
        ),
    ]))
    .block(
        Block::default()
            .borders(Borders::TOP)
            .border_style(Style::default().fg(fg(color, Color::DarkGray, Color::White))),
    )
}

fn status_style(status: RunStatus, waiting: bool, color: bool) -> Style {
    match status {
        RunStatus::Running if waiting => warning(color),
        RunStatus::Running => success(color),
        RunStatus::Paused => warning(color),
        RunStatus::PerfectFound => success(color),
        RunStatus::SourceExhausted => danger(color),
    }
}

fn metric_card(
    label: &str,
    value: impl Into<String>,
    hint: impl Into<String>,
    color: bool,
) -> Paragraph<'static> {
    Paragraph::new(vec![
        Line::styled(label.to_ascii_uppercase(), dim(color)),
        Line::styled(value.into(), accent(color)),
        Line::styled(hint.into(), dim(color)),
    ])
    .block(panel("", color))
}

fn selectable_list<'a>(
    title: &'a str,
    items: &'a [&'a str],
    selected: usize,
    color: bool,
) -> List<'a> {
    let list_items: Vec<ListItem<'a>> = items
        .iter()
        .enumerate()
        .map(|(idx, item)| {
            let style = if idx == selected && color {
                warning(color)
            } else {
                Style::default().fg(fg(color, Color::Gray, Color::White))
            };
            ListItem::new(Line::styled(
                format!("{} {}", if idx == selected { ">" } else { " " }, item),
                style,
            ))
        })
        .collect();
    List::new(list_items).block(panel(title, color))
}

fn run_table_lines(runs: &[RunRecord], limit: usize, color: bool) -> Vec<Line<'static>> {
    if runs.is_empty() {
        return vec![Line::styled("No saved runs yet.", dim(color))];
    }
    let mut lines = vec![Line::styled(
        format!(
            "{:<18} {:>8} {:>12} {:<14}",
            "name", "best", "offset", "status"
        ),
        dim(color),
    )];
    lines.extend(runs.iter().take(limit).map(|run| {
        Line::from(vec![
            Span::styled(format!("{:<18}", truncate(&run.name, 18)), accent(color)),
            Span::raw(format!(" {:>7.2}% ", run.best_score * 100.0)),
            Span::styled(format!("{:>12} ", run.current_offset), dim(color)),
            Span::styled(
                format!("{:<14}", run.status.as_str()),
                status_style(run.status, false, color),
            ),
        ])
    }));
    lines
}

fn dashboard_header_lines(snapshot: &SearchSnapshot, color: bool) -> Vec<Line<'static>> {
    let run = &snapshot.run;
    let metrics = &snapshot.metrics;
    let mut lines = vec![
        Line::from(vec![
            Span::styled("run ", dim(color)),
            Span::styled(truncate(&run.name, 32), accent(color)),
            Span::styled("  status ", dim(color)),
            Span::styled(
                live_status_label(snapshot),
                status_style(run.status, snapshot.waiting_for_digits, color),
            ),
            Span::styled("  best ", dim(color)),
            Span::styled(format!("{:.2}%", run.best_score * 100.0), success(color)),
            Span::styled(" at ", dim(color)),
            Span::styled(
                run.best_offset
                    .map(|offset| offset.to_string())
                    .unwrap_or_else(|| "-".to_string()),
                warning(color),
            ),
        ]),
        Line::from(vec![
            Span::styled("mode ", dim(color)),
            Span::styled(run.match_mode.as_str(), accent(color)),
            Span::styled("  target ", dim(color)),
            Span::styled(format!("{}x{}", run.width, run.height), Style::default()),
            Span::styled("  canvas ", dim(color)),
            Span::styled(
                format!("{}x{}", run.canvas_width, run.canvas_height),
                Style::default(),
            ),
            Span::styled("  runtime ", dim(color)),
            Span::styled(
                fmt_duration(Duration::from_secs_f64(run.total_runtime_secs)),
                Style::default(),
            ),
            Span::styled("  session ", dim(color)),
            Span::styled(fmt_duration(snapshot.session_elapsed), Style::default()),
        ]),
        dashboard_source_line(snapshot, color),
    ];
    if snapshot.run.params_json.contains("\"show_metrics\":true")
        || snapshot.metrics.profile != PerformanceProfile::Balanced
    {
        lines.push(Line::from(vec![
            Span::styled("profile ", dim(color)),
            Span::styled(metrics.profile.as_str(), warning(color)),
            Span::styled("  backend ", dim(color)),
            Span::styled(metrics.search_backend.clone(), accent(color)),
            Span::styled("  gpu ", dim(color)),
            Span::styled(metrics.gpu_status.clone(), Style::default()),
            Span::styled("  chunk ", dim(color)),
            Span::styled(metrics.chunk_size.to_string(), Style::default()),
            Span::styled("  queue ", dim(color)),
            Span::styled(metrics.queue_depth.to_string(), Style::default()),
            Span::styled("  memory ", dim(color)),
            Span::styled(
                format!(
                    "~{}MB/{}MB",
                    metrics.memory_estimate_mb, metrics.memory_limit_mb
                ),
                Style::default(),
            ),
            Span::styled("  chunk_time ", dim(color)),
            Span::styled(
                format!("{:.2}ms", metrics.chunk_processing_ms),
                Style::default(),
            ),
        ]));
    }
    lines
}

fn dashboard_source_line(snapshot: &SearchSnapshot, color: bool) -> Line<'static> {
    let status = live_status_label(snapshot);
    if snapshot.source_is_growing {
        Line::from(vec![
            Span::styled("pi cache ", dim(color)),
            Span::raw(snapshot.source_len.to_string()),
            Span::styled("  gap ", dim(color)),
            Span::raw(snapshot.cache_gap_digits.to_string()),
            Span::styled("  pipeline ", dim(color)),
            Span::raw(status.to_string()),
        ])
    } else {
        Line::from(vec![
            Span::styled("source ", dim(color)),
            Span::raw(snapshot.source_kind.clone()),
            Span::styled("  digits ", dim(color)),
            Span::raw(snapshot.source_len.to_string()),
            Span::styled("  pipeline ", dim(color)),
            Span::raw(status.to_string()),
        ])
    }
}

fn best_match_lines(run: &RunRecord, color: bool) -> Vec<Line<'static>> {
    if let Some(details) = &run.best_match {
        if let Some(raw) = &details.raw_canvas_digits {
            return digit_canvas_to_lines(raw, details, &run.target_bitmap, color);
        }
    }
    run.best_bitmap
        .as_ref()
        .map(|bitmap| bitmap_to_lines(bitmap, color, Some((&run.target_bitmap, run.best_inverted))))
        .unwrap_or_else(|| vec![Line::raw("no best match yet")])
}

fn digit_canvas_to_lines(
    raw: &str,
    details: &crate::search::BestMatchDetails,
    target: &Bitmap,
    color: bool,
) -> Vec<Line<'static>> {
    let width = details.canvas_width as usize;
    let height = details.canvas_height as usize;
    let matched_digit = details.digit.map(|digit| char::from(b'0' + digit));
    let x_offset = details.x.unwrap_or_default() as usize;
    let y_offset = details.y.unwrap_or_default() as usize;
    let chars: Vec<char> = raw.chars().collect();
    let mut lines = Vec::with_capacity(height + 1);
    if let Some(digit) = details.digit {
        lines.push(Line::raw(format!(
            "digit={} pos=({}, {}) coverage={:.2}% leakage={:.2}%",
            digit,
            x_offset,
            y_offset,
            details.coverage.unwrap_or_default() * 100.0,
            details.leakage.unwrap_or_default() * 100.0
        )));
    }
    for y in 0..height {
        let mut spans = Vec::with_capacity(width);
        for x in 0..width {
            let index = y * width + x;
            let ch = chars.get(index).copied().unwrap_or(' ');
            let in_target = x >= x_offset
                && y >= y_offset
                && x < x_offset + target.width
                && y < y_offset + target.height;
            let shape_pixel = in_target && target.get(x - x_offset, y - y_offset) == 1;
            let near_shape = is_adjacent_to_shape(x, y, target, x_offset, y_offset);
            let is_matched_digit = Some(ch) == matched_digit;
            let style = if !color {
                Style::default()
            } else if is_matched_digit && shape_pixel {
                Style::default()
                    .fg(Color::Green)
                    .add_modifier(Modifier::BOLD)
            } else if shape_pixel {
                Style::default().fg(Color::Gray)
            } else if is_matched_digit && near_shape {
                Style::default()
                    .fg(Color::LightYellow)
                    .add_modifier(Modifier::BOLD)
            } else if in_target {
                Style::default().fg(Color::Rgb(75, 75, 80))
            } else {
                Style::default().fg(Color::Rgb(55, 55, 60))
            };
            spans.push(Span::styled(ch.to_string(), style));
        }
        lines.push(Line::from(spans));
    }
    lines
}

fn is_adjacent_to_shape(
    x: usize,
    y: usize,
    target: &Bitmap,
    x_offset: usize,
    y_offset: usize,
) -> bool {
    let x = x as isize;
    let y = y as isize;
    let x_offset = x_offset as isize;
    let y_offset = y_offset as isize;
    for dy in -1..=1 {
        for dx in -1..=1 {
            if dx == 0 && dy == 0 {
                continue;
            }
            let target_x = x + dx - x_offset;
            let target_y = y + dy - y_offset;
            if target_x >= 0
                && target_y >= 0
                && (target_x as usize) < target.width
                && (target_y as usize) < target.height
                && target.get(target_x as usize, target_y as usize) == 1
            {
                return true;
            }
        }
    }
    false
}

fn live_status_label(snapshot: &SearchSnapshot) -> &'static str {
    match snapshot.run.status {
        RunStatus::Running if snapshot.waiting_for_digits => "waiting for more pi",
        RunStatus::Running => "searching",
        RunStatus::Paused => "paused",
        RunStatus::PerfectFound => "perfect found",
        RunStatus::SourceExhausted => "source exhausted",
    }
}

fn finish_message(reason: FinishReason) -> &'static str {
    match reason {
        FinishReason::PerfectFound => "perfect match found; search stopped",
        FinishReason::SourceExhausted => {
            "digit source exhausted; use infinite pi or a larger pi file"
        }
        FinishReason::Interrupted => "search stopped; checkpoint saved",
        FinishReason::LimitReached => "search limit reached; checkpoint saved",
        FinishReason::MaxOffsetReached => "max offset reached; checkpoint saved",
    }
}

fn run_metadata_lines(run: &RunRecord) -> Vec<Line<'static>> {
    vec![
        Line::raw(format!("Name: {}  ID: {}", run.name, run.id)),
        Line::raw(format!(
            "Status: {}  Source: {}  Template: {}",
            run.status.as_str(),
            run.source.source_type,
            run.template_name
                .clone()
                .unwrap_or_else(|| "custom".to_string())
        )),
        Line::raw(format!(
            "Mode: {}  Target: {}x{}  Canvas: {}x{}",
            run.match_mode.as_str(),
            run.width,
            run.height,
            run.canvas_width,
            run.canvas_height
        )),
        Line::raw(format!(
            "Threshold: {}  Invert: {}",
            run.threshold,
            yes_no(run.invert_enabled)
        )),
        Line::raw(format!(
            "Offset: {}  Scanned: {}  Best: {:.2}% at {}",
            run.current_offset,
            run.scanned_windows,
            run.best_score * 100.0,
            run.best_offset
                .map(|offset| offset.to_string())
                .unwrap_or_else(|| "-".to_string())
        )),
        Line::raw(format!(
            "Created: {}  Updated: {}",
            run.created_at, run.updated_at
        )),
    ]
}

fn bitmap_to_lines(
    bitmap: &Bitmap,
    color: bool,
    compare: Option<(&Bitmap, bool)>,
) -> Vec<Line<'static>> {
    let mut lines = Vec::with_capacity(bitmap.height);
    for y in 0..bitmap.height {
        let mut spans = Vec::with_capacity(bitmap.width);
        for x in 0..bitmap.width {
            let pixel = bitmap.get(x, y);
            let symbol = if color {
                if pixel == 1 { "█" } else { "·" }
            } else if pixel == 1 {
                "#"
            } else {
                "."
            };
            let style = if !color {
                Style::default()
            } else if let Some((target, inverted)) = compare {
                let expected = if inverted {
                    1 - target.get(x, y)
                } else {
                    target.get(x, y)
                };
                if pixel == expected {
                    Style::default()
                        .fg(Color::Green)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(Color::Red)
                }
            } else if pixel == 1 {
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(Color::DarkGray)
            };
            spans.push(Span::styled(symbol.to_string(), style));
        }
        lines.push(Line::from(spans));
    }
    lines
}

fn history_lines(events: &[BestEventRecord]) -> Vec<Line<'static>> {
    if events.is_empty() {
        return vec![Line::raw("no best-score improvements yet")];
    }
    events
        .iter()
        .rev()
        .take(6)
        .map(|event| {
            Line::raw(format!(
                "{}  offset={}  score={:.2}%  scanned={}",
                event.timestamp,
                event.offset,
                event.score * 100.0,
                event.scanned_windows
            ))
        })
        .collect()
}

fn selected_line(index: usize, selected: usize, text: String, color: bool) -> Line<'static> {
    let style = if index == selected {
        warning(color)
    } else {
        Style::default().fg(fg(color, Color::Gray, Color::White))
    };
    Line::styled(
        format!("{} {text}", if index == selected { ">" } else { " " }),
        style,
    )
}

fn help_lines(screen: Screen) -> Vec<Line<'static>> {
    let mut lines = vec![
        Line::raw("Global"),
        Line::raw("  ? toggles this help overlay"),
        Line::raw("  Ctrl+C saves active search progress and exits"),
        Line::raw(""),
    ];
    let screen_lines: &[&str] = match screen {
        Screen::Home => &[
            "Home",
            "  Up/Down or j/k navigate",
            "  Enter selects",
            "  q quits",
        ],
        Screen::NewSearch => &[
            "New Search",
            "  Up/Down moves between fields",
            "  Type edits the selected field",
            "  Left/Right adjusts toggles and numbers",
            "  Enter advances or starts search",
            "  Esc returns home",
        ],
        Screen::Dashboard => &[
            "Search Dashboard",
            "  1/2/3/4 switches eco/balanced/performance/max",
            "  +/- changes CPU workers",
            "  [/] changes chunk size",
            "  Space pauses/resumes",
            "  s saves a checkpoint",
            "  e exports JSON",
            "  q saves progress and quits",
        ],
        Screen::SavedRuns => &[
            "Saved Runs",
            "  Enter opens details",
            "  r resumes",
            "  d deletes after confirmation",
            "  e exports JSON",
            "  Backspace/q returns",
        ],
        Screen::RunDetails => &[
            "Run Details",
            "  Up/Down selects actions",
            "  Enter runs selected action",
            "  r/e/d are direct Resume/Export/Delete shortcuts",
        ],
        Screen::Templates => &[
            "Templates",
            "  Up/Down selects a template",
            "  Enter opens the new-search wizard with that template",
        ],
        Screen::Import => &[
            "Import Pi Digits",
            "  Type a file path",
            "  v validates and counts usable digits",
            "  i imports normalized digits into the app cache",
            "  Space toggles decimal-prefix mode",
        ],
        Screen::Settings => &["Settings", "  Backspace/q returns home"],
    };
    lines.extend(screen_lines.iter().map(|line| Line::raw(*line)));
    lines
}

fn centered_rect(percent_x: u16, percent_y: u16, area: Rect) -> Rect {
    let vertical = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - percent_y) / 2),
            Constraint::Percentage(percent_y),
            Constraint::Percentage((100 - percent_y) / 2),
        ])
        .split(area);
    let horizontal = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - percent_x) / 2),
            Constraint::Percentage(percent_x),
            Constraint::Percentage((100 - percent_x) / 2),
        ])
        .split(vertical[1]);
    horizontal[1]
}

fn export_run_json(run: &RunRecord) -> Result<PathBuf> {
    let storage = Storage::open_default()?;
    let history = storage.history(&run.id, None)?;
    let filename = format!("pi-casso-{}-export.json", sanitize_filename(&run.name));
    let path = std::env::current_dir()?.join(filename);
    let body = serde_json::to_string_pretty(&serde_json::json!({
        "run": run,
        "history": history,
    }))?;
    std::fs::write(&path, body)?;
    Ok(path)
}

fn parse_usize_field(value: &str, name: &str) -> Result<usize> {
    let parsed = value
        .trim()
        .parse::<usize>()
        .with_context(|| format!("{name} must be a positive integer"))?;
    if parsed == 0 {
        return Err(anyhow!("{name} must be greater than zero"));
    }
    Ok(parsed)
}

fn parse_u8_field(value: &str, name: &str) -> Result<u8> {
    value
        .trim()
        .parse::<u8>()
        .with_context(|| format!("{name} must be an integer"))
}

fn parse_optional_usize(value: &str, name: &str) -> Result<Option<usize>> {
    if value.trim().is_empty() {
        Ok(None)
    } else {
        value
            .trim()
            .parse::<usize>()
            .map(Some)
            .with_context(|| format!("{name} must be an integer"))
    }
}

fn parse_optional_u64(value: &str, name: &str) -> Result<Option<u64>> {
    if value.trim().is_empty() {
        Ok(None)
    } else {
        value
            .trim()
            .parse::<u64>()
            .map(Some)
            .with_context(|| format!("{name} must be an integer"))
    }
}

fn adjust_numeric_string(value: &mut String, delta: i32, min: i32, max: i32) {
    let current = value.parse::<i32>().unwrap_or(min);
    let next = (current + delta).clamp(min, max);
    *value = next.to_string();
}

fn push_digit(value: &mut String, ch: char) {
    if ch.is_ascii_digit() {
        value.push(ch);
    }
}

fn wrap_index(current: usize, len: usize, delta: i32) -> usize {
    if len == 0 {
        return 0;
    }
    if delta < 0 {
        if current == 0 { len - 1 } else { current - 1 }
    } else {
        (current + 1) % len
    }
}

fn truncate(value: &str, max: usize) -> String {
    if value.chars().count() <= max {
        value.to_string()
    } else {
        value
            .chars()
            .take(max.saturating_sub(1))
            .collect::<String>()
            + "~"
    }
}

fn yes_no(value: bool) -> &'static str {
    if value { "yes" } else { "no" }
}

fn fmt_rate(value: f64) -> String {
    if value >= 1_000_000.0 {
        format!("{:.1}M", value / 1_000_000.0)
    } else if value >= 1_000.0 {
        format!("{:.1}K", value / 1_000.0)
    } else {
        format!("{value:.0}")
    }
}

fn fmt_duration(duration: Duration) -> String {
    let total = duration.as_secs();
    let hours = total / 3600;
    let minutes = (total % 3600) / 60;
    let seconds = total % 60;
    format!("{hours:02}:{minutes:02}:{seconds:02}")
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
    sanitized.trim_matches('-').to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_tui_profile_is_balanced() {
        assert_eq!(
            PERFORMANCE_PROFILES[DEFAULT_PROFILE_IDX],
            PerformanceProfile::Balanced
        );

        let form = NewSearchForm::default();
        assert_eq!(
            PERFORMANCE_PROFILES[form.profile_idx],
            PerformanceProfile::Balanced
        );
    }
}
