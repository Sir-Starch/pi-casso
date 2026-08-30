//! Shared chrome: panels, metric cards, the tab bar, the status bar, list
//! scrolling and modal overlays. Every tab is built from these, so the app looks
//! like one thing rather than five.

use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Block, BorderType, Borders, Clear, List, ListItem, ListState, Padding, Paragraph, Scrollbar,
    ScrollbarOrientation, ScrollbarState, Wrap,
};

use crate::render::Theme;
use crate::tui::tabs::Tab;
use crate::tui::toast::Toasts;

/// Where an interactive surface was drawn, so a click can be mapped back to the
/// row it landed on. Recorded during drawing because that is the only place the
/// layout is actually known.
#[derive(Clone, Copy, Debug)]
pub struct RowRegion {
    /// The bordered panel, inclusive of its frame.
    pub area: Rect,
    /// Index of the item drawn on the panel's first content row.
    pub first_index: usize,
    /// Rows of border above the content. Panels have one; bare areas have none.
    pub top_inset: u16,
}

impl RowRegion {
    pub fn panel(area: Rect, first_index: usize) -> Self {
        Self {
            area,
            first_index,
            top_inset: 1,
        }
    }

    pub fn contains(&self, column: u16, row: u16) -> bool {
        column >= self.area.x
            && column < self.area.x + self.area.width
            && row >= self.area.y
            && row < self.area.y + self.area.height
    }

    /// The item index at a screen row, or `None` when the click landed on the
    /// border or below the last row.
    pub fn index_at(&self, row: u16) -> Option<usize> {
        let top = self.area.y + self.top_inset;
        let bottom = self.area.y + self.area.height.saturating_sub(self.top_inset);
        if row < top || row >= bottom {
            return None;
        }
        Some(self.first_index + (row - top) as usize)
    }
}

/// The minimum terminal this layout can honestly render. Below it the app says
/// so rather than drawing overlapping garbage.
pub const MIN_WIDTH: u16 = 64;
pub const MIN_HEIGHT: u16 = 18;

pub fn panel(title: &str, theme: &Theme) -> Block<'static> {
    styled_panel(title, theme, false)
}

/// The panel the keyboard is currently acting on, marked by a brighter border.
pub fn focused_panel(title: &str, theme: &Theme) -> Block<'static> {
    styled_panel(title, theme, true)
}

fn styled_panel(title: &str, theme: &Theme, focused: bool) -> Block<'static> {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(if theme.unicode {
            BorderType::Rounded
        } else {
            BorderType::Plain
        })
        .border_style(if focused {
            theme.border_focus_style()
        } else {
            theme.border_style()
        })
        .padding(Padding::horizontal(1));
    if title.is_empty() {
        // An empty title used to render as a stray highlighted space in the border.
        block
    } else {
        block.title(Line::from(Span::styled(
            format!(" {title} "),
            theme.accent_style(),
        )))
    }
}

/// One headline number with a label above and a qualifier below.
///
/// Every line is clipped to the cell, with a column to spare, so a long value
/// cannot run into the neighbouring metric.
pub fn render_metric(
    frame: &mut Frame<'_>,
    area: Rect,
    label: &str,
    value: impl Into<String>,
    hint: impl Into<String>,
    theme: &Theme,
) {
    render_metric_lines(
        frame,
        area,
        label,
        Line::styled(clip(value, area.width), theme.accent_style()),
        Line::styled(clip(hint, area.width), theme.dim_style()),
        theme,
    );
}

/// The same three-row shape, for metrics that need their own styling.
pub fn render_metric_lines(
    frame: &mut Frame<'_>,
    area: Rect,
    label: &str,
    value: Line<'static>,
    hint: Line<'static>,
    theme: &Theme,
) {
    frame.render_widget(
        Paragraph::new(vec![
            Line::styled(
                clip(label.to_ascii_uppercase(), area.width),
                theme.dim_style(),
            ),
            value,
            hint,
        ])
        .style(theme.canvas_bg_style()),
        area,
    );
}

/// Leaves one column of breathing room between adjacent cells.
pub fn clip(text: impl Into<String>, width: u16) -> String {
    crate::render::truncate(&text.into(), width.saturating_sub(1) as usize)
}

pub fn render_tab_bar(
    frame: &mut Frame<'_>,
    area: Rect,
    active: Tab,
    status: &str,
    status_style: Style,
    theme: &Theme,
) -> Vec<Rect> {
    let brand = " π-casso ";
    let mut spans = vec![Span::styled(
        brand,
        theme.text_style().add_modifier(Modifier::BOLD),
    )];
    let mut hit_areas = Vec::with_capacity(Tab::ALL.len());
    let mut cursor = area.x + brand.chars().count() as u16;

    for tab in Tab::ALL {
        let selected = tab == active;
        let label = if selected {
            format!(
                " {} {} {} ",
                theme.glyphs.tab_marker,
                tab.title(),
                theme.glyphs.tab_marker
            )
        } else {
            format!("  {}  ", tab.title())
        };
        let width = label.chars().count() as u16;
        hit_areas.push(Rect {
            x: cursor,
            y: area.y,
            width,
            height: area.height.max(1),
        });
        cursor = cursor.saturating_add(width);
        spans.push(Span::styled(
            label,
            if selected {
                theme.button_style()
            } else {
                theme.dim_style()
            },
        ));
    }

    frame.render_widget(Paragraph::new(Line::from(spans)), area);
    // The pipeline status sits flush right so it never collides with the tabs.
    let status_width = status.chars().count() as u16 + 2;
    if area.width > cursor.saturating_sub(area.x) + status_width {
        let status_area = Rect {
            x: area.x + area.width - status_width,
            y: area.y,
            width: status_width,
            height: area.height.max(1),
        };
        frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled(format!("{} ", theme.glyphs.bullet), status_style),
                Span::styled(status.to_string(), theme.dim_style()),
            ])),
            status_area,
        );
    }
    hit_areas
}

/// One entry in the bottom bar. An entry that carries an action is drawn as a
/// filled key cap and can be clicked; one without is a plain description of how
/// the keyboard works there.
#[derive(Clone, Copy)]
pub struct Hint<A> {
    pub key: &'static str,
    pub label: &'static str,
    pub action: Option<A>,
}

impl<A> Hint<A> {
    pub fn button(key: &'static str, label: &'static str, action: A) -> Self {
        Self {
            key,
            label,
            action: Some(action),
        }
    }

    /// A binding worth describing that has nothing sensible to do on a click,
    /// such as "arrows move between fields".
    pub fn note(key: &'static str, label: &'static str) -> Self {
        Self {
            key,
            label,
            action: None,
        }
    }
}

/// Draws the bottom bar and returns the clickable regions it produced.
pub fn render_status_bar<A: Copy>(
    frame: &mut Frame<'_>,
    area: Rect,
    hints: &[Hint<A>],
    theme: &Theme,
) -> Vec<(Rect, A)> {
    let mut spans = Vec::with_capacity(hints.len() * 3);
    let mut buttons = Vec::new();
    let mut used = 0u16;
    // The bar has a top border, so its text sits on the second row.
    let text_row = area.y + 1;

    for hint in hints {
        let key_width = hint.key.chars().count() as u16 + 2;
        let label_width = hint.label.chars().count() as u16;
        // Drop whole hints that do not fit rather than slicing one in half; a
        // half-written binding is worse than an absent one.
        let width = key_width + label_width + 2;
        if used + width > area.width {
            break;
        }
        if let Some(action) = hint.action {
            // The whole cap-plus-label is the click target: aiming at a
            // two-character key cap would be needlessly precise.
            buttons.push((
                Rect {
                    x: area.x + used,
                    y: text_row,
                    width: key_width + label_width,
                    height: 1,
                },
                action,
            ));
        }
        used += width;
        spans.push(Span::styled(
            format!(" {} ", hint.key),
            if hint.action.is_some() {
                theme.button_style()
            } else {
                theme.dim_style()
            },
        ));
        spans.push(Span::styled(hint.label.to_string(), theme.dim_style()));
        spans.push(Span::raw("  "));
    }

    frame.render_widget(
        Paragraph::new(Line::from(spans)).block(
            Block::default()
                .borders(Borders::TOP)
                .border_style(theme.border_style()),
        ),
        area,
    );
    buttons
}

/// A list that actually scrolls. The old screens rendered a plain `List` with no
/// state, so a selection past the visible rows simply vanished.
pub fn render_scroll_list(
    frame: &mut Frame<'_>,
    area: Rect,
    title: &str,
    items: Vec<ListItem<'static>>,
    state: &mut ListState,
    theme: &Theme,
) -> RowRegion {
    let total = items.len();
    let list = List::new(items)
        .block(panel(title, theme))
        .highlight_style(theme.selected_style())
        // Static strings: building this per frame would leak on every redraw.
        .highlight_symbol(if theme.unicode { "▸ " } else { "> " });
    frame.render_stateful_widget(list, area, state);

    // Only worth drawing a scrollbar when there is something off-screen.
    let visible = area.height.saturating_sub(2) as usize;
    if total > visible && visible > 0 {
        let mut scrollbar_state = ScrollbarState::new(total.saturating_sub(visible))
            .position(state.offset().min(total.saturating_sub(visible)));
        frame.render_stateful_widget(
            Scrollbar::new(ScrollbarOrientation::VerticalRight)
                .begin_symbol(None)
                .end_symbol(None)
                .thumb_symbol(theme.glyphs.scroll_thumb)
                .track_symbol(Some(theme.glyphs.scroll_track))
                .style(theme.border_style()),
            area.inner(ratatui::layout::Margin {
                horizontal: 0,
                vertical: 1,
            }),
            &mut scrollbar_state,
        );
    }
    // Read after rendering: that is when ratatui has settled the offset needed
    // to keep the selection visible.
    RowRegion::panel(area, state.offset())
}

pub fn render_toasts(frame: &mut Frame<'_>, area: Rect, toasts: &Toasts, theme: &Theme) {
    if toasts.is_empty() {
        return;
    }
    let lines: Vec<Line<'static>> = toasts
        .visible()
        .map(|toast| {
            Line::from(vec![
                Span::styled(
                    format!(" {:<5} ", toast.level.tag()),
                    toast.level.style(theme),
                ),
                Span::styled(toast.text.clone(), theme.text_style()),
            ])
        })
        .collect();

    let height = (lines.len() as u16 + 2).min(area.height);
    let width = area.width.saturating_sub(4).min(
        lines
            .iter()
            .map(|line| line.width() as u16 + 4)
            .max()
            .unwrap_or(20)
            .max(24),
    );
    let popup = Rect {
        x: area.x + area.width.saturating_sub(width + 1),
        y: area.y + area.height.saturating_sub(height + 1),
        width,
        height,
    };
    frame.render_widget(Clear, popup);
    frame.render_widget(
        Paragraph::new(lines)
            .block(panel("", theme))
            .wrap(Wrap { trim: true }),
        popup,
    );
}

/// A centred modal, used for help, the palette and confirmations.
pub fn modal_area(area: Rect, percent_x: u16, percent_y: u16) -> Rect {
    let vertical = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - percent_y) / 2),
            Constraint::Percentage(percent_y),
            Constraint::Percentage((100 - percent_y) / 2),
        ])
        .split(area);
    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - percent_x) / 2),
            Constraint::Percentage(percent_x),
            Constraint::Percentage((100 - percent_x) / 2),
        ])
        .split(vertical[1])[1]
}

pub fn render_too_small(frame: &mut Frame<'_>, area: Rect, theme: &Theme) {
    let message = Paragraph::new(vec![
        Line::styled("terminal too small", theme.danger_style()),
        Line::styled(
            format!(
                "need at least {MIN_WIDTH}x{MIN_HEIGHT}, have {}x{}",
                area.width, area.height
            ),
            theme.dim_style(),
        ),
    ])
    .alignment(Alignment::Center)
    .wrap(Wrap { trim: true });
    frame.render_widget(Clear, area);
    frame.render_widget(message, area);
}

/// Joins labelled segments, keeping only those that fit. Segments are given in
/// priority order, so a narrow terminal loses the least important first.
pub fn fit_segments(segments: Vec<(String, Style)>, width: u16) -> Vec<Span<'static>> {
    let mut spans = Vec::new();
    let mut used = 0usize;
    let budget = width as usize;
    for (text, style) in segments {
        let len = text.chars().count();
        if used + len > budget {
            break;
        }
        used += len;
        spans.push(Span::styled(text, style));
    }
    spans
}

/// Key/value line used across the detail panes.
pub fn field_line(label: &str, value: impl Into<String>, theme: &Theme) -> Line<'static> {
    Line::from(vec![
        Span::styled(format!("{label:<14}"), theme.dim_style()),
        Span::styled(value.into(), theme.text_style()),
    ])
}

pub fn dim_line(text: impl Into<String>, theme: &Theme) -> Line<'static> {
    Line::styled(text.into(), theme.dim_style())
}

/// Keeps a selection index inside a list that may have shrunk, and returns the
/// value clamped so callers cannot index out of bounds.
pub fn clamp_selection(state: &mut ListState, len: usize) {
    if len == 0 {
        state.select(None);
        return;
    }
    let selected = state.selected().unwrap_or(0).min(len - 1);
    state.select(Some(selected));
}

pub fn move_selection(state: &mut ListState, len: usize, delta: i32) {
    if len == 0 {
        state.select(None);
        return;
    }
    let current = state.selected().unwrap_or(0) as i32;
    let next = (current + delta).clamp(0, len as i32 - 1);
    state.select(Some(next as usize));
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::widgets::ListState;

    #[test]
    fn segments_are_dropped_whole_when_they_do_not_fit() {
        let style = Style::default();
        let segments = vec![
            ("alpha".to_string(), style),
            ("-beta".to_string(), style),
            ("-gamma".to_string(), style),
        ];
        let fitted = fit_segments(segments.clone(), 10);
        assert_eq!(fitted.len(), 2);
        assert_eq!(fitted[1].content.as_ref(), "-beta");
        // Nothing fits at all.
        assert!(fit_segments(segments, 2).is_empty());
    }

    #[test]
    fn metric_text_is_clipped_with_a_column_to_spare() {
        // Exactly-fitting text used to touch the next card.
        assert_eq!(clip("+68.8K/s, 65 536 to go", 22).chars().count(), 21);
        assert_eq!(clip("short", 22), "short");
        assert_eq!(clip("anything", 0), "");
    }

    #[test]
    fn a_click_maps_to_the_row_under_it() {
        let region = RowRegion::panel(Rect::new(0, 4, 20, 10), 7);
        // The border row is not a row of content.
        assert_eq!(region.index_at(4), None);
        assert_eq!(region.index_at(5), Some(7));
        assert_eq!(region.index_at(9), Some(11));
        // Bottom border, and beyond.
        assert_eq!(region.index_at(13), None);
        assert_eq!(region.index_at(40), None);
    }

    #[test]
    fn a_region_without_a_border_starts_at_its_first_row() {
        let region = RowRegion {
            area: Rect::new(0, 4, 20, 3),
            first_index: 0,
            top_inset: 0,
        };
        assert_eq!(region.index_at(4), Some(0));
        assert_eq!(region.index_at(6), Some(2));
        assert_eq!(region.index_at(7), None);
    }

    #[test]
    fn containment_is_checked_in_both_axes() {
        let region = RowRegion::panel(Rect::new(10, 4, 20, 10), 0);
        assert!(region.contains(10, 4));
        assert!(region.contains(29, 13));
        assert!(!region.contains(9, 5));
        assert!(!region.contains(30, 5));
        assert!(!region.contains(15, 3));
        assert!(!region.contains(15, 14));
    }

    #[test]
    fn selection_clamps_when_the_list_shrinks() {
        let mut state = ListState::default();
        state.select(Some(9));
        clamp_selection(&mut state, 3);
        assert_eq!(state.selected(), Some(2));
    }

    #[test]
    fn an_empty_list_has_no_selection() {
        let mut state = ListState::default();
        state.select(Some(4));
        clamp_selection(&mut state, 0);
        assert_eq!(state.selected(), None);
    }

    #[test]
    fn movement_stops_at_the_ends_instead_of_wrapping() {
        let mut state = ListState::default();
        state.select(Some(0));
        move_selection(&mut state, 3, -1);
        assert_eq!(state.selected(), Some(0));
        move_selection(&mut state, 3, 10);
        assert_eq!(state.selected(), Some(2));
    }

    #[test]
    fn modal_area_stays_inside_its_parent() {
        let area = Rect::new(0, 0, 100, 40);
        let modal = modal_area(area, 60, 50);
        assert!(modal.width <= area.width);
        assert!(modal.height <= area.height);
        assert!(modal.x >= area.x);
    }
}
