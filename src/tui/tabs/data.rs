//! The DATA tab: what the local pi cache holds, and importing more digits into
//! it. Previously the import screen showed nothing about the cache it was
//! importing into.

use anyhow::Result;
use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::text::Line;
use ratatui::widgets::{Paragraph, Wrap};

use crate::digits::{DigitSource, FileDigitSource};
use crate::pi;
use crate::render::{Theme, fmt_bytes, fmt_count};
use crate::storage;
use crate::tui::form::{Field, Form, FormOutcome};
use crate::tui::widgets::{RowRegion, dim_line, field_line, focused_panel, panel};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ImportField {
    Path,
    AllowDecimalPrefix,
}

pub struct DataTab {
    pub form: Form<ImportField>,
    /// Result of the last validation, shown until the path changes.
    pub validated_digits: Option<u64>,
}

impl Default for DataTab {
    fn default() -> Self {
        Self {
            form: Form::new(vec![
                Field::text(
                    ImportField::Path,
                    "Digit file",
                    "path to a text file of pi digits",
                    "",
                ),
                Field::toggle(
                    ImportField::AllowDecimalPrefix,
                    "Allow '3.'",
                    "accept a leading '3.' instead of rejecting the file",
                    false,
                ),
            ]),
            validated_digits: None,
        }
    }
}

impl DataTab {
    pub fn handle_key(&mut self, key: crossterm::event::KeyEvent) -> FormOutcome {
        let outcome = self.form.handle_key(key);
        if outcome == FormOutcome::Consumed {
            // Any edit invalidates the previous count.
            self.validated_digits = None;
        }
        outcome
    }

    fn source(&self) -> Result<FileDigitSource> {
        let path = self.form.text(ImportField::Path).trim();
        if path.is_empty() {
            return Err(anyhow::anyhow!("enter a path to a pi digit file"));
        }
        Ok(FileDigitSource::new_with_options(
            path.into(),
            self.form.toggled(ImportField::AllowDecimalPrefix),
        ))
    }

    pub fn validate(&mut self) -> Result<u64> {
        let source = self.source()?;
        source.validate()?;
        let digits = source.len()?;
        self.validated_digits = Some(digits);
        Ok(digits)
    }

    pub fn import(&mut self) -> Result<(u64, std::path::PathBuf)> {
        let source = self.source()?;
        let cache = pi::PiCache::default()?;
        let destination = cache.path().clone();
        if let Some(parent) = destination.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let digits = source.copy_digits_to(&destination)?;
        self.validated_digits = Some(digits);
        Ok((digits, destination))
    }

    pub fn draw(&mut self, frame: &mut Frame<'_>, area: Rect, theme: &Theme) -> RowRegion {
        let rows = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(8),
                Constraint::Length(8),
                Constraint::Min(3),
            ])
            .split(area);

        frame.render_widget(
            Paragraph::new(cache_lines(theme)).block(panel("pi cache", theme)),
            rows[0],
        );

        let mut import_lines = self.form.lines(theme, 16);
        import_lines.push(Line::raw(""));
        import_lines.push(field_line(
            "usable digits",
            self.validated_digits
                .map(|digits| fmt_count(digits, theme.unicode))
                .unwrap_or_else(|| "not checked".to_string()),
            theme,
        ));
        frame.render_widget(
            Paragraph::new(import_lines).block(focused_panel("import digits", theme)),
            rows[1],
        );

        frame.render_widget(
            Paragraph::new(vec![
                dim_line(
                    "Whitespace is ignored. A leading '3.' is rejected unless allowed above.",
                    theme,
                ),
                dim_line(
                    "Imported digits are appended to the cache and become available to every run.",
                    theme,
                ),
                dim_line(self.form.hint(), theme),
            ])
            .wrap(Wrap { trim: true })
            .block(panel("notes", theme)),
            rows[2],
        );
        RowRegion::panel(rows[1], 0)
    }
}

fn cache_lines(theme: &Theme) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    match pi::PiCache::default().and_then(|cache| cache.info()) {
        Ok(info) => {
            lines.push(field_line(
                "digits",
                fmt_count(info.digits, theme.unicode),
                theme,
            ));
            lines.push(field_line("on disk", fmt_bytes(info.bytes), theme));
            lines.push(field_line("path", info.path.display().to_string(), theme));
        }
        Err(err) => lines.push(Line::styled(
            format!("cache unavailable: {err:#}"),
            theme.danger_style(),
        )),
    }
    match storage::db_path() {
        Ok(path) => lines.push(field_line("database", path.display().to_string(), theme)),
        Err(err) => lines.push(Line::styled(err.to_string(), theme.danger_style())),
    }
    lines
}
