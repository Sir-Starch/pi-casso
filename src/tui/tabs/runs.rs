//! The RUNS tab: every saved hunt, with its details beside the list.
//!
//! This replaces the old two-screen split (a list screen you had to leave to see
//! anything about a run) with one view that shows both at once.

use anyhow::Result;
use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::text::{Line, Span};
use ratatui::widgets::{ListItem, ListState, Paragraph};

use crate::render::{
    BitmapView, Theme, bitmap_lines_fit, fmt_count, fmt_duration, fmt_percent, opt_u64, truncate,
    yes_no,
};
use crate::storage::{BestEventRecord, RunRecord, RunStatus, Storage};
use crate::tui::live::{best_lines, history_lines};
use crate::tui::widgets::{
    RowRegion, clamp_selection, dim_line, field_line, panel, render_scroll_list,
};

#[derive(Default)]
pub struct RunsTab {
    pub runs: Vec<RunRecord>,
    pub state: ListState,
    pub history: Vec<BestEventRecord>,
    /// Set while a delete is awaiting y/n.
    pub pending_delete: Option<String>,
    loaded_history_for: Option<String>,
}

impl RunsTab {
    pub fn reload(&mut self) -> Result<()> {
        let storage = Storage::open_default()?;
        self.runs = storage.list_runs()?;
        clamp_selection(&mut self.state, self.runs.len());
        self.loaded_history_for = None;
        Ok(())
    }

    pub fn selected(&self) -> Option<&RunRecord> {
        self.state.selected().and_then(|index| self.runs.get(index))
    }

    /// History is fetched lazily and cached per run, so moving through a long
    /// list does not hit the database on every keypress.
    pub fn sync_history(&mut self) -> Result<()> {
        let Some(id) = self.selected().map(|run| run.id.clone()) else {
            self.history.clear();
            self.loaded_history_for = None;
            return Ok(());
        };
        if self.loaded_history_for.as_deref() == Some(id.as_str()) {
            return Ok(());
        }
        let storage = Storage::open_default()?;
        self.history = storage.history(&id, Some(20))?;
        self.loaded_history_for = Some(id);
        Ok(())
    }

    pub fn delete_selected(&mut self) -> Result<String> {
        let Some(run) = self.selected().cloned() else {
            return Ok("nothing selected".to_string());
        };
        let mut storage = Storage::open_default()?;
        storage.delete_run(&run.id)?;
        self.reload()?;
        Ok(format!("deleted {}", run.name))
    }

    pub fn draw(&mut self, frame: &mut Frame<'_>, area: Rect, theme: &Theme) -> RowRegion {
        let columns = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(46), Constraint::Percentage(54)])
            .split(area);

        let items: Vec<ListItem<'static>> = if self.runs.is_empty() {
            vec![ListItem::new(dim_line(
                "no saved runs yet — start one from the Hunt tab",
                theme,
            ))]
        } else {
            self.runs
                .iter()
                .map(|run| ListItem::new(run_row(run, theme)))
                .collect()
        };
        let region = render_scroll_list(
            frame,
            columns[0],
            "saved runs",
            items,
            &mut self.state,
            theme,
        );

        let rows = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(9),
                Constraint::Min(6),
                Constraint::Length(6),
            ])
            .split(columns[1]);

        let Some(run) = self.selected().cloned() else {
            frame.render_widget(
                Paragraph::new(dim_line("select a run to see its details", theme))
                    .block(panel("details", theme)),
                columns[1],
            );
            return region;
        };

        frame.render_widget(
            Paragraph::new(metadata_lines(&run, theme)).block(panel("details", theme)),
            rows[0],
        );

        let art = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(40), Constraint::Percentage(60)])
            .split(rows[1]);
        let inner_height = rows[1].height.saturating_sub(2) as usize;
        frame.render_widget(
            Paragraph::new(bitmap_lines_fit(
                &run.target_bitmap,
                theme,
                BitmapView::Plain,
                inner_height,
            ))
            .block(panel("target", theme)),
            art[0],
        );
        frame.render_widget(
            Paragraph::new(best_lines(&run, theme, inner_height)).block(panel("best", theme)),
            art[1],
        );
        frame.render_widget(
            Paragraph::new(history_lines(&self.history, theme)).block(panel("history", theme)),
            rows[2],
        );
        region
    }
}

fn run_row(run: &RunRecord, theme: &Theme) -> Line<'static> {
    Line::from(vec![
        Span::styled(
            format!("{:<18}", truncate(&run.name, 18)),
            theme.text_style(),
        ),
        Span::styled(
            format!("{:>7} ", fmt_percent(run.best_score)),
            theme.accent_style(),
        ),
        Span::styled(
            format!("{:<15}", status_text(run.status)),
            status_style(run.status, theme),
        ),
    ])
}

fn status_text(status: RunStatus) -> &'static str {
    match status {
        RunStatus::Running => "running",
        RunStatus::Paused => "paused",
        RunStatus::PerfectFound => "perfect",
        RunStatus::SourceExhausted => "exhausted",
    }
}

fn status_style(status: RunStatus, theme: &Theme) -> ratatui::style::Style {
    match status {
        RunStatus::Running => theme.success_style(),
        RunStatus::Paused => theme.warning_style(),
        RunStatus::PerfectFound => theme.success_style(),
        RunStatus::SourceExhausted => theme.danger_style(),
    }
}

fn metadata_lines(run: &RunRecord, theme: &Theme) -> Vec<Line<'static>> {
    let unicode = theme.unicode;
    let mut lines = vec![
        field_line("name", run.name.clone(), theme),
        field_line(
            "art",
            format!(
                "{}  {}x{} on {}x{}",
                run.template_name.clone().unwrap_or_else(|| "custom".into()),
                run.width,
                run.height,
                run.canvas_width,
                run.canvas_height
            ),
            theme,
        ),
        field_line(
            "mode",
            format!(
                "{}  threshold {}  invert {}",
                run.match_mode.as_str(),
                run.threshold,
                yes_no(run.invert_enabled)
            ),
            theme,
        ),
        field_line(
            "progress",
            format!(
                "offset {}  scanned {}",
                fmt_count(run.current_offset, unicode),
                fmt_count(run.scanned_windows, unicode)
            ),
            theme,
        ),
        field_line(
            "best",
            format!(
                "{} at {}",
                fmt_percent(run.best_score),
                opt_u64(run.best_offset)
            ),
            theme,
        ),
        field_line(
            "runtime",
            fmt_duration(std::time::Duration::from_secs_f64(run.total_runtime_secs)),
            theme,
        ),
        field_line("source", run.source.source_type.clone(), theme),
    ];
    if let Some(path) = &run.source.source_path {
        lines.push(field_line("path", truncate(path, 48), theme));
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_status_gets_a_distinct_label() {
        let labels = [
            RunStatus::Running,
            RunStatus::Paused,
            RunStatus::PerfectFound,
            RunStatus::SourceExhausted,
        ]
        .map(status_text);
        let mut sorted = labels.to_vec();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), labels.len());
    }

    #[test]
    fn an_empty_tab_has_no_selection_and_no_run() {
        let tab = RunsTab::default();
        assert!(tab.selected().is_none());
    }
}
