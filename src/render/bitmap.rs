//! Bitmap and digit-canvas rendering.
//!
//! Both the TUI and the plain CLI output used to carry their own near-identical
//! copy of this logic (`ui.rs::bitmap_lines` vs `interactive.rs::bitmap_to_lines`,
//! and the same again for the digit canvas). There is now one implementation.

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

use crate::art::Bitmap;
use crate::render::theme::Theme;
use crate::search::BestMatchDetails;

/// What a bitmap is being drawn against. `Compare` colours each pixel by whether
/// it agrees with the target, which is how a "best match" is judged at a glance.
#[derive(Clone, Copy)]
pub enum BitmapView<'a> {
    Plain,
    Compare { target: &'a Bitmap, inverted: bool },
}

impl BitmapView<'_> {
    fn expected(&self, x: usize, y: usize) -> Option<u8> {
        match self {
            Self::Plain => None,
            Self::Compare { target, inverted } => {
                if x >= target.width || y >= target.height {
                    return None;
                }
                let value = target.get(x, y);
                Some(if *inverted { 1 - value } else { value })
            }
        }
    }
}

/// The colour a single logical pixel contributes.
///
/// In compare mode only *wrong* pixels and *correct ink* are highlighted;
/// correctly-empty pixels stay background. Painting every agreeing pixel green —
/// as the old code did — turned a mostly-empty canvas into a wall of colour that
/// said nothing.
fn pixel_color(pixel: u8, expected: Option<u8>, theme: &Theme) -> Color {
    match expected {
        None => {
            if pixel == 1 {
                theme.accent
            } else {
                theme.canvas_bg
            }
        }
        Some(expected) if pixel == expected => {
            if pixel == 1 {
                theme.success
            } else {
                theme.canvas_bg
            }
        }
        Some(_) => theme.danger,
    }
}

/// One terminal row per pixel row. Works everywhere, including monochrome and
/// ASCII-only terminals.
pub fn bitmap_cell_lines(
    bitmap: &Bitmap,
    theme: &Theme,
    view: BitmapView<'_>,
) -> Vec<Line<'static>> {
    let mut lines = Vec::with_capacity(bitmap.height);
    for y in 0..bitmap.height {
        let mut spans = Vec::with_capacity(bitmap.width);
        for x in 0..bitmap.width {
            let pixel = bitmap.get(x, y);
            let symbol = if pixel == 1 {
                theme.glyphs.filled
            } else {
                theme.glyphs.empty
            };
            let style = if theme.color {
                Style::default().fg(pixel_color(pixel, view.expected(x, y), theme))
            } else if pixel == 1 {
                Style::default().add_modifier(Modifier::BOLD)
            } else {
                Style::default().add_modifier(Modifier::DIM)
            };
            spans.push(Span::styled(symbol.to_string(), style));
        }
        lines.push(Line::from(spans));
    }
    lines
}

/// Two pixel rows per terminal row, using `▀` with the upper pixel as foreground
/// and the lower pixel as background. Doubles vertical resolution at no cost in
/// screen space, which is what makes a 24x24 emergence canvas fit next to the
/// target instead of scrolling.
///
/// Requires both colour and Unicode: without colour the two halves are
/// indistinguishable.
pub fn bitmap_half_lines(
    bitmap: &Bitmap,
    theme: &Theme,
    view: BitmapView<'_>,
) -> Vec<Line<'static>> {
    let rows = bitmap.height.div_ceil(2);
    let mut lines = Vec::with_capacity(rows);
    for row in 0..rows {
        let top_y = row * 2;
        let bottom_y = top_y + 1;
        let mut spans = Vec::with_capacity(bitmap.width);
        for x in 0..bitmap.width {
            let top = pixel_color(bitmap.get(x, top_y), view.expected(x, top_y), theme);
            // An odd-height bitmap has no bottom pixel on the last row; the cell
            // background falls back to the canvas so the edge stays flush.
            let bottom = if bottom_y < bitmap.height {
                pixel_color(bitmap.get(x, bottom_y), view.expected(x, bottom_y), theme)
            } else {
                theme.canvas_bg
            };
            spans.push(Span::styled(
                theme.glyphs.half.to_string(),
                Style::default().fg(top).bg(bottom),
            ));
        }
        lines.push(Line::from(spans));
    }
    lines
}

/// Picks the densest rendering that both the terminal and the available height
/// can support.
pub fn bitmap_lines_fit(
    bitmap: &Bitmap,
    theme: &Theme,
    view: BitmapView<'_>,
    max_height: usize,
) -> Vec<Line<'static>> {
    let half_capable = theme.color && theme.unicode;
    if half_capable && bitmap.height > max_height {
        bitmap_half_lines(bitmap, theme, view)
    } else {
        bitmap_cell_lines(bitmap, theme, view)
    }
}

/// The raw pi digits of the winning window, with the matched digit highlighted
/// where it lands on the target shape.
pub fn digit_canvas_lines(
    raw: &str,
    details: &BestMatchDetails,
    target: &Bitmap,
    theme: &Theme,
) -> Vec<Line<'static>> {
    let width = details.canvas_width as usize;
    let height = details.canvas_height as usize;
    if width == 0 || height == 0 {
        return vec![Line::raw(raw.to_string())];
    }
    let matched_digit = details.digit.map(|digit| char::from(b'0' + digit));
    let x_offset = details.x.unwrap_or_default() as usize;
    let y_offset = details.y.unwrap_or_default() as usize;
    let chars: Vec<char> = raw.chars().collect();

    let mut lines = Vec::with_capacity(height + 1);
    // Which digit emerged, where it landed, and how cleanly — the numbers that
    // explain the score sitting right above the canvas that produced it.
    if let Some(digit) = details.digit {
        lines.push(Line::from(vec![
            Span::styled("digit ", theme.dim_style()),
            Span::styled(digit.to_string(), theme.success_style()),
            Span::styled("  at ", theme.dim_style()),
            Span::styled(format!("{x_offset},{y_offset}"), theme.text_style()),
            Span::styled("  coverage ", theme.dim_style()),
            Span::styled(
                format!("{:.1}%", details.coverage.unwrap_or_default() * 100.0),
                theme.text_style(),
            ),
            Span::styled("  leakage ", theme.dim_style()),
            Span::styled(
                format!("{:.1}%", details.leakage.unwrap_or_default() * 100.0),
                theme.warning_style(),
            ),
        ]));
    }
    for y in 0..height {
        let mut spans = Vec::with_capacity(width);
        for x in 0..width {
            let ch = chars.get(y * width + x).copied().unwrap_or(' ');
            let in_target = x >= x_offset
                && y >= y_offset
                && x < x_offset + target.width
                && y < y_offset + target.height;
            let shape_pixel = in_target && target.get(x - x_offset, y - y_offset) == 1;
            let is_matched_digit = Some(ch) == matched_digit;
            let style = if !theme.color {
                if is_matched_digit && shape_pixel {
                    Style::default().add_modifier(Modifier::BOLD)
                } else {
                    Style::default()
                }
            } else if is_matched_digit && shape_pixel {
                // The signal: the sought digit, exactly where the shape wants ink.
                Style::default()
                    .fg(theme.success)
                    .add_modifier(Modifier::BOLD)
            } else if shape_pixel {
                // Shape wants ink here but pi supplied something else.
                Style::default().fg(theme.text)
            } else if is_matched_digit && is_adjacent_to_shape(x, y, target, x_offset, y_offset) {
                // Leakage right at the silhouette edge, which is what blurs a match.
                Style::default()
                    .fg(theme.warning)
                    .add_modifier(Modifier::BOLD)
            } else if in_target {
                Style::default().fg(theme.dim).bg(theme.canvas_target_bg)
            } else {
                Style::default().fg(theme.dim)
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

/// Plain-stdout preview. Deliberately stays on ASCII `#`/`.` regardless of theme
/// glyphs: this output gets piped into scripts and diffed.
pub fn render_preview(bitmap: &Bitmap, theme: &Theme) -> String {
    if !theme.color {
        return bitmap.render_ascii('#', '.');
    }
    let accent = ansi_fg(theme.accent);
    let mut out = String::new();
    for y in 0..bitmap.height {
        for x in 0..bitmap.width {
            if bitmap.get(x, y) == 1 {
                out.push_str(&accent);
                out.push('#');
                out.push_str("\x1b[0m");
            } else {
                out.push('.');
            }
        }
        if y + 1 != bitmap.height {
            out.push('\n');
        }
    }
    out
}

pub fn render_digit_canvas(raw: &str, width: usize) -> String {
    if width == 0 {
        return raw.to_string();
    }
    raw.chars()
        .collect::<Vec<_>>()
        .chunks(width)
        .map(|chunk| chunk.iter().collect::<String>())
        .collect::<Vec<_>>()
        .join("\n")
}

fn ansi_fg(color: Color) -> String {
    match color {
        Color::Rgb(r, g, b) => format!("\x1b[38;2;{r};{g};{b}m"),
        Color::Indexed(index) => format!("\x1b[38;5;{index}m"),
        Color::Cyan => "\x1b[36m".to_string(),
        Color::Green => "\x1b[32m".to_string(),
        Color::Yellow => "\x1b[33m".to_string(),
        Color::Red => "\x1b[31m".to_string(),
        _ => "\x1b[36m".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::render::theme::ThemeName;

    fn bitmap(width: usize, height: usize, bits: &str) -> Bitmap {
        Bitmap::from_bit_string(width, height, bits).unwrap()
    }

    #[test]
    fn cell_render_is_one_line_per_row() {
        let theme = Theme::new(ThemeName::Dark, true, true);
        let art = bitmap(2, 3, "101010");
        let lines = bitmap_cell_lines(&art, &theme, BitmapView::Plain);
        assert_eq!(lines.len(), 3);
        assert_eq!(lines[0].spans.len(), 2);
    }

    #[test]
    fn half_render_halves_even_height() {
        let theme = Theme::new(ThemeName::Dark, true, true);
        let art = bitmap(2, 4, "10101010");
        let lines = bitmap_half_lines(&art, &theme, BitmapView::Plain);
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].spans.len(), 2);
    }

    #[test]
    fn half_render_rounds_up_odd_height() {
        let theme = Theme::new(ThemeName::Dark, true, true);
        let art = bitmap(2, 3, "101010");
        let lines = bitmap_half_lines(&art, &theme, BitmapView::Plain);
        // Three pixel rows still need two terminal rows; the missing lower half
        // of the last row must not panic or drop a column.
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[1].spans.len(), 2);
        assert_eq!(lines[1].spans[0].style.bg, Some(theme.canvas_bg));
    }

    #[test]
    fn fit_falls_back_to_cells_without_colour() {
        let theme = Theme::new(ThemeName::Mono, false, true);
        let art = bitmap(2, 8, "1010101010101010");
        // Mono cannot express two pixels per cell, so height wins over density.
        let lines = bitmap_lines_fit(&art, &theme, BitmapView::Plain, 2);
        assert_eq!(lines.len(), 8);
    }

    #[test]
    fn fit_uses_half_blocks_when_too_tall() {
        let theme = Theme::new(ThemeName::Dark, true, true);
        let art = bitmap(2, 8, "1010101010101010");
        let lines = bitmap_lines_fit(&art, &theme, BitmapView::Plain, 4);
        assert_eq!(lines.len(), 4);
    }

    #[test]
    fn compare_marks_only_ink_and_errors() {
        let theme = Theme::new(ThemeName::Dark, true, true);
        let target = bitmap(2, 1, "10");
        let found = bitmap(2, 1, "11");
        let lines = bitmap_cell_lines(
            &found,
            &theme,
            BitmapView::Compare {
                target: &target,
                inverted: false,
            },
        );
        // Pixel 0 agrees and is ink -> success; pixel 1 disagrees -> danger.
        assert_eq!(lines[0].spans[0].style.fg, Some(theme.success));
        assert_eq!(lines[0].spans[1].style.fg, Some(theme.danger));
    }

    #[test]
    fn preview_stays_ascii_for_pipes() {
        let theme = Theme::new(ThemeName::Dark, false, true);
        let art = bitmap(2, 2, "1001");
        assert_eq!(render_preview(&art, &theme), "#.\n.#");
    }

    fn details(digit: Option<u8>) -> BestMatchDetails {
        BestMatchDetails {
            mode: crate::search::MatchMode::Emergence,
            digit,
            x: Some(1),
            y: Some(0),
            canvas_width: 3,
            canvas_height: 2,
            raw_canvas_digits: Some("777888".to_string()),
            coverage: Some(0.5),
            leakage: Some(0.25),
        }
    }

    #[test]
    fn the_digit_canvas_explains_the_score_above_the_grid() {
        let theme = Theme::new(ThemeName::Dark, true, true);
        let target = bitmap(2, 2, "1001");
        let lines = digit_canvas_lines("777888", &details(Some(7)), &target, &theme);
        // One header plus one line per canvas row.
        assert_eq!(lines.len(), 3);
        let header: String = lines[0]
            .spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect();
        assert!(header.contains("digit 7"), "{header}");
        assert!(header.contains("coverage 50.0%"), "{header}");
        assert!(header.contains("leakage 25.0%"), "{header}");
    }

    #[test]
    fn a_canvas_without_a_matched_digit_has_no_header() {
        let theme = Theme::new(ThemeName::Dark, true, true);
        let target = bitmap(2, 2, "1001");
        let lines = digit_canvas_lines("777888", &details(None), &target, &theme);
        assert_eq!(lines.len(), 2);
    }

    #[test]
    fn a_zero_sized_canvas_falls_back_to_the_raw_string() {
        let theme = Theme::new(ThemeName::Dark, true, true);
        let target = bitmap(2, 2, "1001");
        let mut zero = details(Some(7));
        zero.canvas_width = 0;
        let lines = digit_canvas_lines("777888", &zero, &target, &theme);
        assert_eq!(lines.len(), 1);
    }

    #[test]
    fn digit_canvas_wraps_on_char_boundaries() {
        assert_eq!(render_digit_canvas("123456", 3), "123\n456");
        assert_eq!(render_digit_canvas("123", 0), "123");
    }
}
