//! The GALLERY tab: the built-in templates and what they look like.

use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::text::Line;
use ratatui::widgets::{ListItem, ListState, Paragraph, Wrap};

use crate::art;
use crate::render::{BitmapView, Theme, bitmap_lines_fit};
use crate::tui::widgets::{RowRegion, dim_line, panel, render_scroll_list};

pub struct GalleryTab {
    pub state: ListState,
}

impl Default for GalleryTab {
    fn default() -> Self {
        let mut state = ListState::default();
        state.select(Some(0));
        Self { state }
    }
}

impl GalleryTab {
    pub fn selected_template(&self) -> Option<&'static str> {
        let names = art::template_names();
        self.state
            .selected()
            .and_then(|index| names.get(index))
            .copied()
    }

    pub fn len(&self) -> usize {
        art::template_names().len()
    }

    pub fn draw(&mut self, frame: &mut Frame<'_>, area: Rect, theme: &Theme) -> RowRegion {
        let columns = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Length(24), Constraint::Min(20)])
            .split(area);

        let items: Vec<ListItem<'static>> = art::template_names()
            .iter()
            .map(|name| ListItem::new(Line::styled((*name).to_string(), theme.text_style())))
            .collect();
        let region = render_scroll_list(
            frame,
            columns[0],
            "templates",
            items,
            &mut self.state,
            theme,
        );

        let rows = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Min(6), Constraint::Length(4)])
            .split(columns[1]);

        let Some(name) = self.selected_template() else {
            frame.render_widget(
                Paragraph::new(dim_line("no templates available", theme))
                    .block(panel("preview", theme)),
                columns[1],
            );
            return region;
        };

        let height = rows[0].height.saturating_sub(2) as usize;
        // Previewed at 16x16 because that is the largest preset; smaller targets
        // are scaled from the same source art.
        let lines = match art::load_template(name, 16, 16) {
            Ok(bitmap) => bitmap_lines_fit(&bitmap, theme, BitmapView::Plain, height),
            Err(err) => vec![Line::styled(err.to_string(), theme.danger_style())],
        };
        frame.render_widget(Paragraph::new(lines).block(panel(name, theme)), rows[0]);
        frame.render_widget(
            Paragraph::new(vec![dim_line(
                "Enter opens the Hunt wizard preloaded with this template.",
                theme,
            )])
            .wrap(Wrap { trim: true })
            .block(panel("", theme)),
            rows[1],
        );
        region
    }
}
