//! The one-shot full-screen view used by `start`, `hunt` and `resume` when they
//! are not running under the interactive app. It shows a search and nothing
//! else: no tabs, no navigation, no state to manage.

use anyhow::Result;
use ratatui::Terminal;
use ratatui::layout::{Constraint, Direction, Layout};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Wrap};
use std::time::{Duration, Instant};

use crate::render::{
    BitmapView, Theme, bitmap_lines_fit, digit_canvas_lines, fmt_count, fmt_duration, fmt_rate,
    opt_u64, snapshot_state,
};
use crate::search::{FinishReason, SearchReporter, SearchSnapshot};
use crate::storage::{BestEventRecord, RunRecord};
use crate::tui::terminal::{Backend, TerminalGuard, install_panic_hook};
use crate::tui::widgets::{MIN_HEIGHT, MIN_WIDTH, panel, render_too_small};

pub struct LiveReporter {
    guard: TerminalGuard,
    theme: Theme,
    last_draw: Instant,
}

impl LiveReporter {
    pub fn new(theme: Theme) -> Result<Self> {
        install_panic_hook();
        Ok(Self {
            // No mouse: this view has nothing to click.
            guard: TerminalGuard::new(false)?,
            theme,
            last_draw: Instant::now() - Duration::from_secs(60),
        })
    }

    fn draw(&mut self, snapshot: &SearchSnapshot) -> Result<()> {
        let theme = self.theme;
        draw_live(&mut self.guard.terminal, snapshot, &theme)
    }
}

impl SearchReporter for LiveReporter {
    fn on_update(&mut self, snapshot: &SearchSnapshot) -> Result<()> {
        if self.last_draw.elapsed() >= Duration::from_millis(150) {
            self.draw(snapshot)?;
            self.last_draw = Instant::now();
        }
        Ok(())
    }

    fn on_new_best(&mut self, snapshot: &SearchSnapshot, _event: &BestEventRecord) -> Result<()> {
        self.draw(snapshot)?;
        self.last_draw = Instant::now();
        Ok(())
    }

    fn on_finish(&mut self, snapshot: &SearchSnapshot, _reason: FinishReason) -> Result<()> {
        self.draw(snapshot)
    }
}

fn draw_live(
    terminal: &mut Terminal<Backend>,
    snapshot: &SearchSnapshot,
    theme: &Theme,
) -> Result<()> {
    terminal.draw(|frame| {
        let area = frame.area();
        if area.width < MIN_WIDTH || area.height < MIN_HEIGHT {
            render_too_small(frame, area, theme);
            return;
        }
        let rows = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(8),
                Constraint::Min(8),
                Constraint::Length(8),
            ])
            .split(area);

        frame.render_widget(
            Paragraph::new(header_lines(snapshot, theme))
                .block(panel("pi-casso", theme))
                .wrap(Wrap { trim: true }),
            rows[0],
        );

        let columns = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(45), Constraint::Percentage(55)])
            .split(rows[1]);
        let inner_height = rows[1].height.saturating_sub(2) as usize;
        frame.render_widget(
            Paragraph::new(bitmap_lines_fit(
                &snapshot.run.target_bitmap,
                theme,
                BitmapView::Plain,
                inner_height,
            ))
            .block(panel("target", theme)),
            columns[0],
        );
        frame.render_widget(
            Paragraph::new(best_lines(&snapshot.run, theme, inner_height))
                .block(panel("best manifestation", theme)),
            columns[1],
        );
        frame.render_widget(
            Paragraph::new(history_lines(&snapshot.recent_events, theme))
                .block(panel("recent improvements", theme)),
            rows[2],
        );
    })?;
    Ok(())
}

fn header_lines(snapshot: &SearchSnapshot, theme: &Theme) -> Vec<Line<'static>> {
    let run = &snapshot.run;
    let unicode = theme.unicode;
    let mut lines = vec![
        Line::raw(format!(
            "run {}  art {}  source {}  mode {}",
            run.name,
            run.template_name.clone().unwrap_or_else(|| "custom".into()),
            snapshot.source_kind,
            run.match_mode.as_str()
        )),
        Line::raw(format!(
            "offset={} scanned={} source_digits={} canvas={}x{}",
            fmt_count(run.current_offset, unicode),
            fmt_count(run.scanned_windows, unicode),
            fmt_count(snapshot.source_len, unicode),
            run.canvas_width,
            run.canvas_height
        )),
        source_line(snapshot, unicode),
        Line::raw(format!(
            "speed={}/s  avg={}/s  best={:.2}% at {}",
            fmt_rate(snapshot.speed_windows_per_sec),
            fmt_rate(snapshot.average_windows_per_sec),
            run.best_score * 100.0,
            opt_u64(run.best_offset)
        )),
        Line::raw(format!(
            "elapsed={} total_runtime={}",
            fmt_duration(snapshot.session_elapsed),
            fmt_duration(Duration::from_secs_f64(run.total_runtime_secs))
        )),
    ];
    lines.push(match snapshot.progress {
        Some(progress) => Line::raw(format!("bounded progress={:.2}%", progress * 100.0)),
        None => Line::raw("bounded progress=unbounded search"),
    });
    if snapshot.source_kind == "demo" {
        lines.push(Line::styled(
            "demo pi source only; use --pi-file for a meaningful local scan",
            theme.warning_style(),
        ));
    }
    lines
}

fn source_line(snapshot: &SearchSnapshot, unicode: bool) -> Line<'static> {
    let status = snapshot_state(snapshot).label();
    if !snapshot.source_is_growing {
        return Line::raw(format!(
            "source={} source_digits={} status={status}",
            snapshot.source_kind,
            fmt_count(snapshot.source_len, unicode)
        ));
    }
    // A generated cache is best described by how fast it is growing, not by how
    // far behind it happens to be at this instant.
    let generation = snapshot
        .generation
        .filter(|progress| progress.active || progress.digits_per_sec > 0.0)
        .map(|progress| format!(" generating={}/s", fmt_rate(progress.digits_per_sec)))
        .unwrap_or_default();
    Line::raw(format!(
        "generated={} cache_gap={}{generation} status={status}",
        fmt_count(snapshot.source_len, unicode),
        fmt_count(snapshot.cache_gap_digits, unicode)
    ))
}

pub(crate) fn best_lines(run: &RunRecord, theme: &Theme, max_height: usize) -> Vec<Line<'static>> {
    // The raw digit canvas is the most informative view when it exists: it shows
    // which digit emerged and where, not just the resulting silhouette.
    if let Some(details) = &run.best_match {
        if let Some(raw) = &details.raw_canvas_digits {
            return digit_canvas_lines(raw, details, &run.target_bitmap, theme);
        }
    }
    match &run.best_bitmap {
        Some(bitmap) => bitmap_lines_fit(
            bitmap,
            theme,
            BitmapView::Compare {
                target: &run.target_bitmap,
                inverted: run.best_inverted,
            },
            max_height,
        ),
        None => vec![Line::styled(
            "no match has improved the starting score yet",
            theme.dim_style(),
        )],
    }
}

pub(crate) fn history_lines(events: &[BestEventRecord], theme: &Theme) -> Vec<Line<'static>> {
    if events.is_empty() {
        return vec![Line::styled(
            "no best-score improvements yet",
            theme.dim_style(),
        )];
    }
    events
        .iter()
        .rev()
        .take(6)
        .map(|event| {
            Line::from(vec![
                Span::styled(format!("{} ", theme.glyphs.rising), theme.success_style()),
                Span::styled(format!("{:.2}%", event.score * 100.0), theme.text_style()),
                Span::styled(
                    format!(
                        "  offset {}  ·  {} scanned",
                        fmt_count(event.offset, theme.unicode),
                        fmt_count(event.scanned_windows, theme.unicode)
                    ),
                    theme.dim_style(),
                ),
            ])
        })
        .collect()
}
