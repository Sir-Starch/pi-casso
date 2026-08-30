//! The HUNT tab: the new-search wizard when nothing is running, the live
//! dashboard when something is.

use std::path::PathBuf;
#[cfg(test)]
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;

use anyhow::{Context, Result, anyhow};
use chrono::Utc;
use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Gauge, Paragraph, Wrap};

use crate::art::{self, ArtMapping, Bitmap};
use crate::benchmark_contract::{
    AUTO_MIN_WORK_WINDOWS, BackendPreflightRequest, BackendResolution, CudaPreflight,
    WgpuPreflight, cuda_preflight, resolve_backend_preflight,
};
use crate::capability::GpuCapability;
use crate::config::Config;
use crate::digits::DigitSourceSpec;
use crate::performance::{
    GeneratorBackendChoice, GpuMode, PerformanceOverrides, PerformanceProfile, PerformanceSettings,
    SearchBackendChoice, ThermalMode,
};
use crate::pi;
use crate::render::{
    BitmapView, PipelineState, Theme, bitmap_lines_fit, fmt_count, fmt_duration, fmt_percent,
    fmt_rate, opt_u64, snapshot_state, truncate,
};
use crate::search::{BackendSelectionError, MatchMode, SearchOptions, SearchSnapshot};
use crate::storage::{NewRun, RunRecord, RunStatus, Storage};
use crate::tui::form::{Field, Form, FormOutcome};
use crate::tui::live::{best_lines, history_lines};
use crate::tui::widgets::{
    RowRegion, clip, dim_line, fit_segments, focused_panel, panel, render_metric,
    render_metric_lines,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WizardField {
    ArtSource,
    Template,
    ArtFile,
    Name,
    PiSource,
    PiFile,
    AllowDecimalPrefix,
    MatchMode,
    SizeMode,
    Width,
    Height,
    CanvasWidth,
    CanvasHeight,
    Threshold,
    Invert,
    KeepGoing,
    Profile,
    Gpu,
    Thermal,
    Workers,
    Limit,
    Start,
    AdvancedHeading,
}

const ART_SOURCES: [&str; 2] = ["built-in template", "custom ASCII file"];
const PI_SOURCES: [&str; 3] = ["infinite generated pi", "pi digit file", "demo sample"];
const MATCH_MODES: [&str; 3] = ["emergence", "threshold", "exact"];
const SIZE_MODES: [(&str, usize, usize); 4] = [
    ("8x8", 8, 8),
    ("12x12", 12, 12),
    ("16x16", 16, 16),
    ("custom", 0, 0),
];
const CUSTOM_SIZE_INDEX: usize = 3;
const PROFILES: [PerformanceProfile; 4] = [
    PerformanceProfile::Eco,
    PerformanceProfile::Balanced,
    PerformanceProfile::Performance,
    PerformanceProfile::Max,
];
const GPU_MODES: [GpuMode; 3] = [GpuMode::Off, GpuMode::Auto, GpuMode::On];
const THERMAL_MODES: [ThermalMode; 3] = [
    ThermalMode::Quiet,
    ThermalMode::Normal,
    ThermalMode::Aggressive,
];

pub struct HuntTab {
    pub form: Form<WizardField>,
    scroll: u16,
    /// Set when focus moved, so the next draw can re-anchor the scroll window.
    dirty_focus: bool,
}

pub struct StartSpec {
    target: StartTarget,
    name: Option<String>,
    source: StartDigitSource,
    template_name: Option<String>,
    width: usize,
    height: usize,
    canvas_width: usize,
    canvas_height: usize,
    match_mode: MatchMode,
    threshold: u8,
    invert: bool,
    params_json: String,
    options: SearchOptions,
}

pub struct PreparedStart {
    pub run: RunRecord,
    pub options: SearchOptions,
    pub capability: BackendResolution,
}

enum StartTarget {
    Template(String),
    File(PathBuf),
}

enum StartDigitSource {
    Cache(PathBuf),
    File {
        path: PathBuf,
        allow_decimal_prefix: bool,
    },
    Demo,
}

#[cfg(test)]
static TEST_FORCE_START_WGPU_UNAVAILABLE: AtomicBool = AtomicBool::new(false);
#[cfg(test)]
static TEST_FAIL_IF_SOURCE_OPEN: AtomicBool = AtomicBool::new(false);
#[cfg(test)]
static TEST_SOURCE_OPEN_COUNT: AtomicUsize = AtomicUsize::new(0);

impl HuntTab {
    pub fn new(config: &Config) -> Self {
        let defaults = &config.search;
        let templates: Vec<&str> = art::template_names().to_vec();
        let template_index = templates
            .iter()
            .position(|name| *name == defaults.template)
            .unwrap_or(0);
        let size_index = SIZE_MODES
            .iter()
            .position(|(_, width, height)| *width == defaults.width && *height == defaults.height)
            .unwrap_or(CUSTOM_SIZE_INDEX);
        let match_index = MATCH_MODES
            .iter()
            .position(|mode| *mode == defaults.match_mode.as_str())
            .unwrap_or(0);

        // Ordered so the common path is short: pick art, name it, press Enter.
        // "Start search" sits directly after the essentials, and everything that
        // has a sensible default lives below it under a heading.
        let form = Form::new(vec![
            Field::choice(
                WizardField::ArtSource,
                "Art source",
                "built-in template or your own ASCII file",
                &ART_SOURCES,
                0,
            ),
            Field::choice(
                WizardField::Template,
                "Template",
                "left/right to browse the gallery",
                &templates,
                template_index,
            ),
            Field::text(
                WizardField::ArtFile,
                "Art file",
                "path to a .txt of ASCII art",
                "",
            ),
            Field::text(
                WizardField::Name,
                "Run name",
                "left blank, a timestamped name is used",
                "",
            ),
            Field::choice(
                WizardField::PiSource,
                "Pi source",
                "generated cache never runs out; a file eventually does",
                &PI_SOURCES,
                0,
            ),
            Field::text(
                WizardField::PiFile,
                "Pi digit file",
                "path to a file of pi digits",
                "",
            ),
            Field::toggle(
                WizardField::AllowDecimalPrefix,
                "Allow '3.'",
                "accept a leading '3.' in the digit file",
                false,
            ),
            Field::choice(
                WizardField::SizeMode,
                "Target size",
                "preset sizes, or custom width and height",
                &SIZE_MODES.map(|(label, _, _)| label),
                size_index,
            ),
            Field::submit(WizardField::Start, "START SEARCH", "click, Enter, or F9"),
            Field::separator(
                WizardField::AdvancedHeading,
                "advanced (all have sensible defaults)",
            ),
            Field::choice(
                WizardField::MatchMode,
                "Match mode",
                "emergence looks for a digit forming the shape; threshold compares pixels",
                &MATCH_MODES,
                match_index,
            ),
            Field::number(
                WizardField::Width,
                "Width",
                "target width in pixels",
                defaults.width,
                1,
                128,
            ),
            Field::number(
                WizardField::Height,
                "Height",
                "target height in pixels",
                defaults.height,
                1,
                128,
            ),
            Field::number(
                WizardField::CanvasWidth,
                "Canvas width",
                "the window pi is read into; must be at least the target",
                defaults.canvas_width,
                1,
                256,
            ),
            Field::number(
                WizardField::CanvasHeight,
                "Canvas height",
                "the window pi is read into; must be at least the target",
                defaults.canvas_height,
                1,
                256,
            ),
            Field::number(
                WizardField::Threshold,
                "Threshold",
                "digits at or above this count as ink",
                defaults.threshold,
                0,
                9,
            ),
            Field::toggle(
                WizardField::Invert,
                "Allow inverted",
                "also score the negative of the target",
                false,
            ),
            Field::toggle(
                WizardField::KeepGoing,
                "Keep going at 100%",
                "do not stop on a perfect match",
                false,
            ),
            Field::choice(
                WizardField::Profile,
                "Performance",
                "how much of the machine to use",
                &PROFILES.map(|profile| profile.as_str()),
                profile_index(defaults.profile),
            ),
            Field::choice(
                WizardField::Gpu,
                "GPU",
                "off, automatic, or forced",
                &GPU_MODES.map(gpu_label),
                1,
            ),
            Field::choice(
                WizardField::Thermal,
                "Thermal",
                "how hard to push between batches",
                &THERMAL_MODES.map(thermal_label),
                1,
            ),
            Field::number(
                WizardField::Workers,
                "CPU workers",
                "0 means one per core",
                0,
                0,
                1024,
            ),
            Field::number(
                WizardField::Limit,
                "Window limit",
                "0 means no limit",
                0,
                0,
                i64::MAX,
            ),
        ]);
        let mut tab = Self {
            form,
            scroll: 0,
            dirty_focus: false,
        };
        tab.sync_enabled();
        tab
    }

    pub fn preload_template(&mut self, template: &str) {
        if let Some(index) = art::template_names()
            .iter()
            .position(|name| *name == template)
        {
            self.form.set_choice(WizardField::Template, index);
        }
        self.form.set_choice(WizardField::ArtSource, 0);
        if self.form.text(WizardField::Name).is_empty() {
            self.form
                .set_text(WizardField::Name, format!("{template}-hunt"));
        }
        self.sync_enabled();
        self.form.focus(WizardField::Name);
    }

    /// Fields that do not apply to the current choices are disabled rather than
    /// hidden, so the form never reflows under the cursor.
    pub fn sync_enabled(&mut self) {
        let template_source = self.form.choice(WizardField::ArtSource) == 0;
        let pi_file_source = self.form.choice(WizardField::PiSource) == 1;
        let emergence = self.form.choice(WizardField::MatchMode) == 0;
        let custom_size = self.form.choice(WizardField::SizeMode) == CUSTOM_SIZE_INDEX;

        self.form
            .set_enabled(WizardField::Template, template_source);
        self.form
            .set_enabled(WizardField::ArtFile, !template_source);
        self.form.set_enabled(WizardField::PiFile, pi_file_source);
        self.form
            .set_enabled(WizardField::AllowDecimalPrefix, pi_file_source);
        self.form.set_enabled(WizardField::Width, custom_size);
        self.form.set_enabled(WizardField::Height, custom_size);
        self.form.set_enabled(WizardField::CanvasWidth, emergence);
        self.form.set_enabled(WizardField::CanvasHeight, emergence);
        // Threshold and inversion are meaningless when a single digit is being
        // asked to form the shape.
        self.form.set_enabled(WizardField::Threshold, !emergence);
        self.form.set_enabled(WizardField::Invert, !emergence);

        if !custom_size {
            let (_, width, height) = SIZE_MODES[self.form.choice(WizardField::SizeMode)];
            self.form.set_text(WizardField::Width, width);
            self.form.set_text(WizardField::Height, height);
        }
        self.form.ensure_enabled(1);
    }

    pub fn handle_key(&mut self, key: crossterm::event::KeyEvent) -> FormOutcome {
        let before = self.form.selected_id();
        let outcome = self.form.handle_key(key);
        if outcome == FormOutcome::Consumed {
            self.sync_enabled();
            if self.form.selected_id() != before {
                self.dirty_focus = true;
            }
        }
        outcome
    }

    pub fn start_spec(&self) -> Result<StartSpec> {
        let width = parse_positive(self.form.text(WizardField::Width), "width")?;
        let height = parse_positive(self.form.text(WizardField::Height), "height")?;
        let match_mode = match self.form.choice(WizardField::MatchMode) {
            0 => MatchMode::Emergence,
            1 => MatchMode::Threshold,
            _ => MatchMode::Exact,
        };
        let threshold =
            parse_optional(self.form.text(WizardField::Threshold), "threshold")?.unwrap_or(5) as u8;
        if threshold > 9 {
            return Err(anyhow!("threshold must be between 0 and 9"));
        }

        let (canvas_width, canvas_height) = if match_mode == MatchMode::Emergence {
            let canvas_width =
                parse_positive(self.form.text(WizardField::CanvasWidth), "canvas width")?;
            let canvas_height =
                parse_positive(self.form.text(WizardField::CanvasHeight), "canvas height")?;
            if canvas_width < width || canvas_height < height {
                return Err(anyhow!(
                    "canvas {canvas_width}x{canvas_height} must be at least the target {width}x{height}"
                ));
            }
            (canvas_width, canvas_height)
        } else {
            (width, height)
        };

        let profile = PROFILES[self.form.choice(WizardField::Profile)];
        let gpu = GPU_MODES[self.form.choice(WizardField::Gpu)];
        let thermal = THERMAL_MODES[self.form.choice(WizardField::Thermal)];
        let workers = parse_optional(self.form.text(WizardField::Workers), "workers")?
            .filter(|value| *value > 0)
            .map(|value| value as usize);
        let limit =
            parse_optional(self.form.text(WizardField::Limit), "limit")?.filter(|value| *value > 0);
        let invert = match_mode != MatchMode::Emergence && self.form.toggled(WizardField::Invert);
        let keep_going = self.form.toggled(WizardField::KeepGoing);

        let performance = performance_settings(profile, gpu, thermal, workers, match_mode);
        let params = serde_json::json!({
            "limit": limit,
            "workers": workers,
            "allow_decimal_prefix": self.form.toggled(WizardField::AllowDecimalPrefix),
            "infinite": self.form.choice(WizardField::PiSource) == 0,
            "match_mode": match_mode.as_str(),
            "canvas_width": canvas_width,
            "canvas_height": canvas_height,
            "interactive": true,
            "profile": profile.as_str(),
            "gpu": gpu.as_str(),
            "thermal_mode": thermal.as_str(),
            "keep_going_after_perfect": keep_going,
        });

        let options = SearchOptions {
            max_offset: None,
            work_windows: None,
            limit,
            match_mode,
            canvas_width,
            canvas_height,
            threshold,
            invert,
            workers,
            checkpoint_every: Duration::from_secs(performance.limits.checkpoint_every_secs),
            top_n: 10,
            keep_going_after_perfect: keep_going,
            chunk_windows: performance.limits.chunk_size,
            performance,
        };
        let target = if self.form.choice(WizardField::ArtSource) == 0 {
            StartTarget::Template(
                art::template_names()[self.form.choice(WizardField::Template)].to_string(),
            )
        } else {
            let path = self.form.text(WizardField::ArtFile).trim();
            if path.is_empty() {
                return Err(anyhow!("enter a path to an ASCII-art file"));
            }
            StartTarget::File(PathBuf::from(path))
        };
        let name = (!self.form.text(WizardField::Name).trim().is_empty())
            .then(|| self.form.text(WizardField::Name).trim().to_string());
        let template_name = match &target {
            StartTarget::Template(name) => Some(name.clone()),
            StartTarget::File(_) => None,
        };

        Ok(StartSpec {
            target,
            name,
            source: self.build_source_spec()?,
            template_name,
            width,
            height,
            canvas_width,
            canvas_height,
            match_mode,
            threshold,
            invert,
            params_json: params.to_string(),
            options,
        })
    }

    pub fn build(
        &self,
        mut start: StartSpec,
        capability: BackendResolution,
    ) -> Result<PreparedStart> {
        apply_resolved_backend(&mut start.options, &capability)?;
        let target = match &start.target {
            StartTarget::Template(template) => {
                art::load_template(template, start.width, start.height)?
            }
            StartTarget::File(path) => {
                let contents = std::fs::read_to_string(path)
                    .with_context(|| format!("failed to read {}", path.display()))?;
                Bitmap::from_ascii(&contents, start.width, start.height, &ArtMapping::default())?
            }
        };
        let (source, generated_digit_count) = materialize_source(start.source)?;
        let mut storage = Storage::open_default()?;
        let run = storage.create_run(NewRun {
            name: start
                .name
                .unwrap_or_else(|| format!("pi-casso-{}", Utc::now().timestamp())),
            source,
            template_name: start.template_name,
            art_hash: target.sha256(),
            width: target.width as u32,
            height: target.height as u32,
            canvas_width: start.canvas_width as u32,
            canvas_height: start.canvas_height as u32,
            match_mode: start.match_mode,
            threshold: start.threshold,
            invert_enabled: start.invert,
            start_offset: Some(0),
            target_bitmap: target,
            generated_digit_count,
            params_json: start.params_json,
        })?;
        Ok(PreparedStart {
            run,
            options: start.options,
            capability,
        })
    }

    fn build_source_spec(&self) -> Result<StartDigitSource> {
        match self.form.choice(WizardField::PiSource) {
            0 => {
                let cache = pi::PiCache::default()?;
                Ok(StartDigitSource::Cache(cache.path().clone()))
            }
            1 => {
                let raw = self.form.text(WizardField::PiFile).trim();
                if raw.is_empty() {
                    return Err(anyhow!(
                        "enter a pi digit file, or switch to infinite generated pi"
                    ));
                }
                Ok(StartDigitSource::File {
                    path: PathBuf::from(raw),
                    allow_decimal_prefix: self.form.toggled(WizardField::AllowDecimalPrefix),
                })
            }
            _ => Ok(StartDigitSource::Demo),
        }
    }

    fn load_target(&self, width: usize, height: usize) -> Result<Bitmap> {
        if self.form.choice(WizardField::ArtSource) == 0 {
            let template = art::template_names()[self.form.choice(WizardField::Template)];
            return art::load_template(template, width, height);
        }
        let path = self.form.text(WizardField::ArtFile).trim();
        if path.is_empty() {
            return Err(anyhow!("enter a path to an ASCII-art file"));
        }
        let contents =
            std::fs::read_to_string(path).with_context(|| format!("failed to read {path}"))?;
        Bitmap::from_ascii(&contents, width, height, &ArtMapping::default())
    }

    /// Live preview of whatever the art fields currently describe.
    fn preview(&self) -> Result<Bitmap> {
        let width = parse_positive(self.form.text(WizardField::Width), "width").unwrap_or(12);
        let height = parse_positive(self.form.text(WizardField::Height), "height").unwrap_or(12);
        self.load_target(width, height)
    }

    pub fn draw_wizard(&mut self, frame: &mut Frame<'_>, area: Rect, theme: &Theme) -> RowRegion {
        let columns = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(58), Constraint::Percentage(42)])
            .split(area);

        let visible = columns[0].height.saturating_sub(2);
        self.dirty_focus = false;
        self.scroll = scroll_for(self.form.selected_index() as u16, visible, self.scroll);
        frame.render_widget(
            Paragraph::new(self.form.lines(theme, 20))
                .scroll((self.scroll, 0))
                .block(focused_panel("new search", theme)),
            columns[0],
        );

        let rows = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Min(6), Constraint::Length(5)])
            .split(columns[1]);
        let preview_height = rows[0].height.saturating_sub(2) as usize;
        let preview_lines = match self.preview() {
            Ok(bitmap) => bitmap_lines_fit(&bitmap, theme, BitmapView::Plain, preview_height),
            Err(err) => vec![Line::styled(format!("{err}"), theme.dim_style())],
        };
        frame.render_widget(
            Paragraph::new(preview_lines)
                .alignment(Alignment::Center)
                .block(panel("preview", theme)),
            rows[0],
        );
        frame.render_widget(
            Paragraph::new(vec![dim_line(self.form.hint(), theme)])
                .wrap(Wrap { trim: true })
                .block(panel("about this field", theme)),
            rows[1],
        );
        RowRegion::panel(columns[0], self.scroll as usize)
    }
}

/// Draws the live dashboard for an in-flight or just-finished search.
pub fn draw_dashboard(
    frame: &mut Frame<'_>,
    area: Rect,
    snapshot: Option<&SearchSnapshot>,
    theme: &Theme,
) {
    let Some(snapshot) = snapshot else {
        frame.render_widget(
            Paragraph::new(vec![
                Line::styled("starting search worker", theme.accent_style()),
                dim_line("waiting for the first snapshot", theme),
            ])
            .block(panel("hunt", theme)),
            area,
        );
        return;
    };

    // The history panel is the first thing to go on a short terminal: the live
    // numbers and the two canvases are what the search is actually about.
    let show_history = area.height >= 20;
    let mut constraints = vec![
        Constraint::Length(1),
        Constraint::Length(3),
        Constraint::Min(8),
    ];
    if show_history {
        constraints.push(Constraint::Length(6));
    }
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints(constraints)
        .split(area);

    frame.render_widget(
        Paragraph::new(context_line(snapshot, rows[0], theme)),
        rows[0],
    );
    draw_metrics(frame, rows[1], snapshot, theme);

    let columns = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(36), Constraint::Percentage(64)])
        .split(rows[2]);
    let inner_height = rows[2].height.saturating_sub(2) as usize;
    frame.render_widget(
        Paragraph::new(bitmap_lines_fit(
            &snapshot.run.target_bitmap,
            theme,
            BitmapView::Plain,
            inner_height,
        ))
        .alignment(Alignment::Center)
        .block(panel("target canvas", theme).style(theme.canvas_bg_style())),
        columns[0],
    );
    frame.render_widget(
        Paragraph::new(best_lines(&snapshot.run, theme, inner_height))
            .block(panel("best match / pi stream", theme).style(theme.canvas_bg_style())),
        columns[1],
    );
    if show_history {
        frame.render_widget(
            Paragraph::new(history_lines(&snapshot.recent_events, theme))
                .block(panel("recent improvements", theme)),
            rows[3],
        );
    }
}

impl StartSpec {
    pub fn resolve_preflight(
        &self,
    ) -> std::result::Result<BackendResolution, BackendSelectionError> {
        let effective_work_windows =
            SearchOptions::intersect_count_bounds(self.options.work_windows, self.options.limit)
                .unwrap_or_default();
        let (backend, gpu) = match self.options.performance.gpu {
            GpuMode::Off => (SearchBackendChoice::Cpu, GpuMode::Off),
            GpuMode::Auto => (SearchBackendChoice::Auto, GpuMode::Auto),
            GpuMode::On => (SearchBackendChoice::Gpu, GpuMode::On),
        };
        let cuda_capability = (gpu == GpuMode::Auto
            && effective_work_windows >= AUTO_MIN_WORK_WINDOWS)
            .then(start_cuda_capability);
        let cuda = cuda_capability
            .as_ref()
            .map_or(CudaPreflight::NotProbed, cuda_preflight);
        let probes_wgpu = !matches!(cuda, CudaPreflight::Eligible)
            && (gpu == GpuMode::On
                || (gpu == GpuMode::Auto && effective_work_windows >= AUTO_MIN_WORK_WINDOWS));
        let wgpu = if probes_wgpu {
            let capability = start_wgpu_capability(
                self.options
                    .performance
                    .gpu_device
                    .as_deref()
                    .filter(|device| *device != "auto"),
            );
            if capability.capability_state == "preflight_ok" {
                WgpuPreflight::Eligible
            } else {
                WgpuPreflight::Unavailable(match capability.reason.as_str() {
                    "adapter_unavailable" => "adapter_unavailable",
                    _ => "pipeline_preflight_unavailable",
                })
            }
        } else {
            WgpuPreflight::NotProbed
        };
        let resolution = resolve_backend_preflight(BackendPreflightRequest {
            backend: Some(backend),
            gpu: Some(gpu),
            effective_work_windows,
            cuda,
            wgpu,
        });
        match resolution.status {
            "ok" => Ok(resolution),
            "unsupported" | "selection_error" => Err(BackendSelectionError {
                status: resolution.status,
                reason: resolution.reason,
                requested_backend: resolution.requested.to_string(),
            }),
            status => Err(BackendSelectionError {
                status,
                reason: resolution.reason,
                requested_backend: resolution.requested.to_string(),
            }),
        }
    }
}

fn start_cuda_capability() -> GpuCapability {
    #[cfg(feature = "cuda-native")]
    {
        crate::cuda::detect_capability()
    }
    #[cfg(not(feature = "cuda-native"))]
    {
        GpuCapability::cuda_unavailable("cuda_not_compiled", "not_attempted")
    }
}

fn start_wgpu_capability(device_filter: Option<&str>) -> GpuCapability {
    #[cfg(test)]
    if test_start_wgpu_unavailable() {
        return GpuCapability::unavailable("adapter_unavailable");
    }
    GpuCapability::detect_with_filter(device_filter)
}

fn apply_resolved_backend(
    options: &mut SearchOptions,
    capability: &BackendResolution,
) -> Result<()> {
    match capability.resolved {
        Some("cpu") => {
            options.performance.backend = SearchBackendChoice::Cpu;
            options.performance.gpu = GpuMode::Off;
        }
        Some("wgpu") => {
            options.performance.backend = SearchBackendChoice::Gpu;
            options.performance.gpu = GpuMode::On;
        }
        Some("cuda") => {
            options.performance.backend = SearchBackendChoice::Cuda;
            options.performance.gpu = GpuMode::On;
        }
        Some(backend) => return Err(anyhow!("unknown resolved backend {backend}")),
        None => return Err(anyhow!("backend preflight did not select a backend")),
    }
    Ok(())
}

fn materialize_source(source: StartDigitSource) -> Result<(DigitSourceSpec, u64)> {
    #[cfg(test)]
    test_source_open_boundary()?;
    let (source, generated_digit_count) = match source {
        StartDigitSource::Cache(path) => {
            let cache = pi::PiCache::new(path.clone());
            cache.ensure_parent()?;
            let generated_digit_count = cache.published_digit_count()?;
            (DigitSourceSpec::cache(path), generated_digit_count)
        }
        StartDigitSource::File {
            path,
            allow_decimal_prefix,
        } => {
            let path = path
                .canonicalize()
                .with_context(|| format!("could not resolve {}", path.display()))?;
            (DigitSourceSpec::file(path, allow_decimal_prefix), 0)
        }
        StartDigitSource::Demo => (DigitSourceSpec::demo(), 0),
    };
    Ok((source, generated_digit_count))
}

#[cfg(test)]
fn test_start_wgpu_unavailable() -> bool {
    TEST_FORCE_START_WGPU_UNAVAILABLE.load(Ordering::SeqCst)
        || (crate::gpu_ring::test_mode_enabled()
            && std::env::var("PI_CASSO_TEST_FORCE_CAPABILITY")
                .is_ok_and(|value| value == "wgpu-unavailable"))
}

#[cfg(test)]
fn test_source_open_boundary() -> Result<()> {
    TEST_SOURCE_OPEN_COUNT.fetch_add(1, Ordering::SeqCst);
    let fails = TEST_FAIL_IF_SOURCE_OPEN.load(Ordering::SeqCst)
        || (crate::gpu_ring::test_mode_enabled()
            && std::env::var("PI_CASSO_TEST_FAIL_IF_SOURCE_OPEN").is_ok_and(|value| value == "1"));
    if fails {
        return Err(anyhow!(
            "PI_CASSO_TEST_FAIL_IF_SOURCE_OPEN reached the TUI source-open boundary"
        ));
    }
    Ok(())
}

#[cfg(test)]
pub(crate) fn test_force_start_wgpu_unavailable(enabled: bool) {
    TEST_FORCE_START_WGPU_UNAVAILABLE.store(enabled, Ordering::SeqCst);
}

#[cfg(test)]
pub(crate) fn test_fail_if_source_open(enabled: bool) {
    TEST_FAIL_IF_SOURCE_OPEN.store(enabled, Ordering::SeqCst);
    if enabled {
        TEST_SOURCE_OPEN_COUNT.store(0, Ordering::SeqCst);
    }
}

#[cfg(test)]
pub(crate) fn test_source_open_count() -> usize {
    TEST_SOURCE_OPEN_COUNT.load(Ordering::SeqCst)
}

/// One line naming the run and its shape, so the dashboard says *what* is being
/// hunted and not only how fast. Segments are ordered by importance and the
/// trailing ones are dropped on a narrow terminal.
fn context_line(snapshot: &SearchSnapshot, area: Rect, theme: &Theme) -> Line<'static> {
    let run = &snapshot.run;
    let mut segments = vec![(
        format!(" {}", truncate(&run.name, 28)),
        theme.accent_style(),
    )];

    // A finished search must say so in the body, not only in a toast that
    // expires and a status bullet in the corner.
    match run.status {
        RunStatus::Paused => segments.push(("  PAUSED".to_string(), theme.warning_style())),
        RunStatus::PerfectFound => {
            segments.push(("  PERFECT MATCH".to_string(), theme.success_style()));
        }
        RunStatus::SourceExhausted => {
            segments.push(("  SOURCE EXHAUSTED".to_string(), theme.danger_style()));
        }
        RunStatus::Running => {}
    }

    segments.extend([
        (
            format!(
                "  {}",
                run.template_name.clone().unwrap_or_else(|| "custom".into())
            ),
            theme.text_style(),
        ),
        (
            format!(
                "  {}x{} on {}x{}",
                run.width, run.height, run.canvas_width, run.canvas_height
            ),
            theme.dim_style(),
        ),
        (format!("  {}", run.match_mode.as_str()), theme.text_style()),
        (
            format!(
                "  {} / {} / {}w / {}",
                snapshot.metrics.search_backend,
                snapshot.metrics.profile.as_str(),
                snapshot.metrics.cpu_workers,
                snapshot.metrics.thermal_mode.as_str()
            ),
            theme.dim_style(),
        ),
        (
            format!(
                "  runtime {}",
                fmt_duration(Duration::from_secs_f64(run.total_runtime_secs))
            ),
            theme.dim_style(),
        ),
    ]);
    Line::from(fit_segments(segments, area.width))
}

fn draw_metrics(frame: &mut Frame<'_>, area: Rect, snapshot: &SearchSnapshot, theme: &Theme) {
    let run = &snapshot.run;
    frame.render_widget(Block::default().style(theme.canvas_bg_style()), area);
    let cells = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Ratio(1, 4); 4])
        .split(area);

    render_metric(
        frame,
        cells[0],
        "progress",
        fmt_count(run.current_offset, theme.unicode),
        format!(
            "{} windows scanned",
            fmt_count(run.scanned_windows, theme.unicode)
        ),
        theme,
    );
    render_metric(
        frame,
        cells[1],
        "throughput",
        format!("{}/s", fmt_rate(snapshot.speed_windows_per_sec)),
        format!("avg {}/s", fmt_rate(snapshot.average_windows_per_sec)),
        theme,
    );

    let gauge_rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Min(1),
        ])
        .split(cells[2]);
    frame.render_widget(
        Paragraph::new(Line::styled("BEST SCORE", theme.dim_style())),
        gauge_rows[0],
    );
    frame.render_widget(
        Gauge::default()
            .ratio(run.best_score.clamp(0.0, 1.0))
            .label(Span::styled(
                fmt_percent(run.best_score),
                theme.text_style(),
            ))
            .gauge_style(theme.success_style()),
        gauge_rows[1],
    );
    frame.render_widget(
        Paragraph::new(Line::styled(
            clip(format!("at {}", opt_u64(run.best_offset)), cells[2].width),
            theme.dim_style(),
        )),
        gauge_rows[2],
    );

    draw_cache_metric(frame, cells[3], snapshot, theme);
}

/// The cache metric carries the answer to "why is nothing happening": when the
/// search outruns the generator, this is where the generation rate shows up.
fn draw_cache_metric(frame: &mut Frame<'_>, area: Rect, snapshot: &SearchSnapshot, theme: &Theme) {
    if !snapshot.source_is_growing {
        render_metric(
            frame,
            area,
            "source",
            fmt_count(snapshot.source_len, theme.unicode),
            snapshot.source_kind.clone(),
            theme,
        );
        return;
    }

    let generating = snapshot
        .generation
        .map(|progress| progress.active || progress.digits_per_sec > 0.0)
        .unwrap_or(false);
    let rate = snapshot
        .generation
        .map(|progress| progress.digits_per_sec)
        .unwrap_or(0.0);
    // Only a backlog worth waiting for is worth reporting; the generator
    // normally sits within a few digits of the request.
    const MEANINGFUL_BACKLOG: u64 = 10_000;
    let backlog = snapshot
        .generation
        .map(|progress| progress.target_digits.saturating_sub(snapshot.source_len))
        .unwrap_or(0);
    // The rate comes first: on a narrow terminal the backlog is what gets
    // clipped, and the rate is the part that answers "is it working".
    let hint = if generating && backlog >= MEANINGFUL_BACKLOG {
        format!("+{}/s  {} left", fmt_rate(rate), fmt_rate(backlog as f64))
    } else if generating {
        format!("+{}/s generating", fmt_rate(rate))
    } else {
        format!(
            "{} ahead",
            fmt_count(snapshot.cache_gap_digits, theme.unicode)
        )
    };

    render_metric_lines(
        frame,
        area,
        "pi cache",
        Line::from(vec![
            Span::styled(
                clip(
                    fmt_count(snapshot.source_len, theme.unicode),
                    area.width.saturating_sub(2),
                ),
                theme.accent_style(),
            ),
            Span::styled(
                if generating {
                    format!(" {}", theme.glyphs.rising)
                } else {
                    String::new()
                },
                theme.success_style(),
            ),
        ]),
        Line::styled(
            clip(hint, area.width),
            if generating {
                theme.success_style()
            } else {
                theme.dim_style()
            },
        ),
        theme,
    );
}

/// The pipeline state shown next to the tabs, plus how it should be coloured.
pub fn status_label(snapshot: Option<&SearchSnapshot>) -> (String, PipelineState) {
    let Some(snapshot) = snapshot else {
        return ("idle".to_string(), PipelineState::Paused);
    };
    let state = snapshot_state(snapshot);
    let text = match state {
        // Generation is the one state worth annotating in the header: it tells
        // the user the pause is productive and roughly how long it will last.
        PipelineState::GeneratingPi => {
            let rate = snapshot
                .generation
                .map(|progress| progress.digits_per_sec)
                .unwrap_or(0.0);
            if rate > 0.0 {
                format!("generating pi {}/s", fmt_rate(rate))
            } else {
                state.label().to_string()
            }
        }
        _ => state.label().to_string(),
    };
    (text, state)
}

/// Options for resuming an existing run.
///
/// The profile is read back from the run's own parameters instead of being
/// hard-coded, so resuming an eco hunt does not silently promote it.
#[cfg(test)]
pub fn resume_options(run: &RunRecord) -> SearchOptions {
    let params: serde_json::Value =
        serde_json::from_str(&run.params_json).unwrap_or(serde_json::Value::Null);
    let profile = params
        .get("profile")
        .and_then(|value| value.as_str())
        .and_then(profile_from_str)
        .unwrap_or(PerformanceProfile::Balanced);
    let gpu = params
        .get("gpu")
        .and_then(|value| value.as_str())
        .and_then(gpu_from_str)
        .unwrap_or(GpuMode::Auto);
    let keep_going = params
        .get("keep_going_after_perfect")
        .and_then(|value| value.as_bool())
        .unwrap_or(false);
    let thermal = params
        .get("thermal_mode")
        .and_then(|value| value.as_str())
        .and_then(thermal_from_str)
        .unwrap_or(ThermalMode::Normal);
    let workers = params
        .get("workers")
        .and_then(|value| value.as_u64())
        .map(|value| value as usize);

    let performance = performance_settings(profile, gpu, thermal, workers, run.match_mode);
    SearchOptions {
        max_offset: None,
        work_windows: None,
        limit: None,
        match_mode: run.match_mode,
        canvas_width: run.canvas_width as usize,
        canvas_height: run.canvas_height as usize,
        threshold: run.threshold,
        invert: run.invert_enabled,
        workers,
        checkpoint_every: Duration::from_secs(performance.limits.checkpoint_every_secs),
        top_n: run.top_matches.len().max(10),
        keep_going_after_perfect: keep_going,
        chunk_windows: performance.limits.chunk_size,
        performance,
    }
}

fn performance_settings(
    profile: PerformanceProfile,
    gpu: GpuMode,
    thermal: ThermalMode,
    workers: Option<usize>,
    match_mode: MatchMode,
) -> PerformanceSettings {
    PerformanceSettings::from_profile(
        profile,
        SearchBackendChoice::Auto,
        GeneratorBackendChoice::Auto,
        gpu,
        None,
        thermal,
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

/// The next profile in the eco to max ordering, wrapping around. A profile the
/// UI does not offer (`custom`, set from the command line) steps to balanced.
pub fn next_profile(current: PerformanceProfile) -> PerformanceProfile {
    match PROFILES.iter().position(|item| *item == current) {
        Some(index) => PROFILES[(index + 1) % PROFILES.len()],
        None => PerformanceProfile::Balanced,
    }
}

fn profile_index(profile: PerformanceProfile) -> usize {
    PROFILES
        .iter()
        .position(|candidate| *candidate == profile)
        .unwrap_or(1)
}

#[cfg(test)]
fn profile_from_str(value: &str) -> Option<PerformanceProfile> {
    PROFILES
        .iter()
        .copied()
        .find(|profile| profile.as_str() == value)
}

#[cfg(test)]
fn gpu_from_str(value: &str) -> Option<GpuMode> {
    GPU_MODES
        .iter()
        .copied()
        .find(|mode| mode.as_str() == value)
}

fn gpu_label(mode: GpuMode) -> &'static str {
    mode.as_str()
}

#[cfg(test)]
fn thermal_from_str(value: &str) -> Option<ThermalMode> {
    THERMAL_MODES
        .iter()
        .copied()
        .find(|mode| mode.as_str() == value)
}

fn thermal_label(mode: ThermalMode) -> &'static str {
    mode.as_str()
}

/// Keeps the focused row inside the visible window of a scrolling panel.
fn scroll_for(selected: u16, visible: u16, current: u16) -> u16 {
    if visible == 0 {
        return 0;
    }
    if selected < current {
        return selected;
    }
    if selected >= current + visible {
        return selected + 1 - visible;
    }
    current
}

fn parse_positive(value: &str, name: &str) -> Result<usize> {
    let parsed = value
        .trim()
        .parse::<usize>()
        .map_err(|_| anyhow!("{name} must be a whole number"))?;
    if parsed == 0 {
        return Err(anyhow!("{name} must be greater than zero"));
    }
    Ok(parsed)
}

fn parse_optional(value: &str, name: &str) -> Result<Option<u64>> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }
    trimmed
        .parse::<u64>()
        .map(Some)
        .map_err(|_| anyhow!("{name} must be a whole number"))
}

/// Runs that already ended cannot be resumed; saying so is friendlier than
/// starting a worker that immediately stops.
pub fn resume_blocked_reason(run: &RunRecord) -> Option<String> {
    match run.status {
        RunStatus::PerfectFound => Some(format!("{} already found a perfect match", run.name)),
        RunStatus::SourceExhausted => Some(format!("{} exhausted its digit source", run.name)),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_profile_cycle_covers_every_offered_profile_and_wraps() {
        let mut seen = vec![PerformanceProfile::Eco];
        let mut current = PerformanceProfile::Eco;
        for _ in 0..3 {
            current = next_profile(current);
            seen.push(current);
        }
        assert_eq!(seen, PROFILES.to_vec());
        assert_eq!(
            next_profile(PerformanceProfile::Max),
            PerformanceProfile::Eco
        );
    }

    #[test]
    fn a_profile_the_wizard_does_not_offer_steps_to_balanced() {
        assert_eq!(
            next_profile(PerformanceProfile::Custom),
            PerformanceProfile::Balanced
        );
    }

    #[test]
    fn start_search_comes_before_the_advanced_fields() {
        // The whole point of the reorder: reaching Start must not require
        // walking past a dozen defaults.
        let tab = HuntTab::new(&Config::default());
        let position = |id: WizardField| {
            tab.form
                .fields
                .iter()
                .position(|field| field.id == id)
                .unwrap()
        };
        assert!(position(WizardField::Start) < position(WizardField::MatchMode));
        assert!(position(WizardField::Start) < position(WizardField::Profile));
        assert!(position(WizardField::SizeMode) < position(WizardField::Start));
    }

    #[test]
    fn the_default_path_to_start_is_short() {
        let mut tab = HuntTab::new(&Config::default());
        let mut presses = 0;
        while tab.form.selected_id() != Some(WizardField::Start) {
            tab.handle_key(crossterm::event::KeyEvent::new(
                crossterm::event::KeyCode::Enter,
                crossterm::event::KeyModifiers::NONE,
            ));
            presses += 1;
            assert!(presses < 30, "never reached Start");
        }
        // It used to take fourteen. Anything in single digits is a wizard.
        assert!(presses <= 6, "took {presses} presses to reach Start");
    }

    #[test]
    fn scrolling_follows_the_selection_in_both_directions() {
        assert_eq!(scroll_for(0, 10, 0), 0);
        // Selection below the window pulls it down by just enough.
        assert_eq!(scroll_for(12, 10, 0), 3);
        // Selection above the window pulls it back up.
        assert_eq!(scroll_for(2, 10, 5), 2);
        // Already visible: no movement.
        assert_eq!(scroll_for(7, 10, 3), 3);
    }

    #[test]
    fn a_zero_height_panel_does_not_divide_by_visible_rows() {
        assert_eq!(scroll_for(9, 0, 4), 0);
    }

    #[test]
    fn emergence_disables_threshold_and_inversion() {
        let mut tab = HuntTab::new(&Config::default());
        tab.form.set_choice(WizardField::MatchMode, 0);
        tab.sync_enabled();
        assert!(!tab.form.get(WizardField::Threshold).unwrap().enabled);
        assert!(tab.form.get(WizardField::CanvasWidth).unwrap().enabled);

        tab.form.set_choice(WizardField::MatchMode, 1);
        tab.sync_enabled();
        assert!(tab.form.get(WizardField::Threshold).unwrap().enabled);
        assert!(!tab.form.get(WizardField::CanvasWidth).unwrap().enabled);
    }

    #[test]
    fn a_size_preset_overwrites_width_and_height() {
        let mut tab = HuntTab::new(&Config::default());
        tab.form.set_choice(WizardField::SizeMode, 0);
        tab.sync_enabled();
        assert_eq!(tab.form.text(WizardField::Width), "8");
        assert_eq!(tab.form.text(WizardField::Height), "8");
        assert!(!tab.form.get(WizardField::Width).unwrap().enabled);
    }

    #[test]
    fn an_empty_size_is_an_error_rather_than_a_crash() {
        // This exact sequence used to terminate the whole application.
        let mut tab = HuntTab::new(&Config::default());
        tab.form
            .set_choice(WizardField::SizeMode, CUSTOM_SIZE_INDEX);
        tab.sync_enabled();
        tab.form.set_text(WizardField::Width, "");
        let error = tab.start_spec().err().unwrap().to_string();
        assert!(error.contains("width"), "unexpected error: {error}");
    }

    #[test]
    fn a_canvas_smaller_than_the_target_is_rejected() {
        let mut tab = HuntTab::new(&Config::default());
        tab.form.set_choice(WizardField::MatchMode, 0);
        tab.form.set_choice(WizardField::SizeMode, 2);
        tab.sync_enabled();
        tab.form.set_text(WizardField::CanvasWidth, "8");
        tab.form.set_text(WizardField::CanvasHeight, "8");
        let error = tab.start_spec().err().unwrap().to_string();
        assert!(error.contains("canvas"), "unexpected error: {error}");
    }

    #[test]
    fn a_missing_art_file_is_reported_not_fatal() {
        let mut tab = HuntTab::new(&Config::default());
        tab.form.set_choice(WizardField::ArtSource, 1);
        tab.sync_enabled();
        assert!(tab.start_spec().is_err());
    }

    #[test]
    fn resume_keeps_the_profile_the_run_was_created_with() {
        let mut run = sample_run();
        run.params_json = serde_json::json!({
            "profile": "eco",
            "gpu": "off",
            "thermal_mode": "quiet",
            "keep_going_after_perfect": true,
        })
        .to_string();
        let options = resume_options(&run);
        assert_eq!(options.performance.profile, PerformanceProfile::Eco);
        assert_eq!(options.performance.gpu, GpuMode::Off);
        assert_eq!(options.performance.thermal_mode, ThermalMode::Quiet);
        assert!(options.keep_going_after_perfect);
    }

    #[test]
    fn resume_falls_back_to_balanced_when_parameters_are_unreadable() {
        let mut run = sample_run();
        run.params_json = "{}".to_string();
        assert_eq!(
            resume_options(&run).performance.profile,
            PerformanceProfile::Balanced
        );
        run.params_json = "not json".to_string();
        assert_eq!(
            resume_options(&run).performance.profile,
            PerformanceProfile::Balanced
        );
    }

    #[test]
    fn finished_runs_report_why_they_cannot_resume() {
        let mut run = sample_run();
        run.status = RunStatus::PerfectFound;
        assert!(resume_blocked_reason(&run).is_some());
        run.status = RunStatus::Paused;
        assert!(resume_blocked_reason(&run).is_none());
    }

    fn sample_run() -> RunRecord {
        RunRecord {
            id: "id".into(),
            name: "sample".into(),
            created_at: String::new(),
            updated_at: String::new(),
            source: DigitSourceSpec::demo(),
            template_name: None,
            art_hash: String::new(),
            width: 12,
            height: 12,
            canvas_width: 24,
            canvas_height: 24,
            match_mode: MatchMode::Emergence,
            threshold: 5,
            invert_enabled: false,
            current_offset: 0,
            scanned_windows: 0,
            best_score: 0.0,
            best_offset: None,
            best_bitmap: None,
            best_inverted: false,
            best_match: None,
            target_bitmap: Bitmap::blank(12, 12),
            status: RunStatus::Paused,
            total_runtime_secs: 0.0,
            generated_digit_count: 0,
            params_json: "{}".into(),
            top_matches: Vec::new(),
        }
    }
}
