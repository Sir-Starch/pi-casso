use std::io::{self, Stdout};
use std::time::{Duration, Instant};

use anyhow::Result;
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};

use crate::art::Bitmap;
use crate::search::{FinishReason, SearchReporter, SearchSnapshot};
use crate::storage::{BestEventRecord, RunRecord, RunStatus};

pub struct PlainReporter {
    color: bool,
    json: bool,
    last_update: Instant,
}

impl PlainReporter {
    pub fn new(color: bool, json: bool) -> Self {
        Self {
            color,
            json,
            last_update: Instant::now() - Duration::from_secs(60),
        }
    }
}

impl SearchReporter for PlainReporter {
    fn on_update(&mut self, snapshot: &SearchSnapshot) -> Result<()> {
        if self.json || self.last_update.elapsed() < Duration::from_secs(2) {
            return Ok(());
        }
        self.last_update = Instant::now();
        println!(
            "[{}] offset={} scanned={} speed={}/s best={:.2}% at offset={}",
            fmt_duration(snapshot.session_elapsed),
            snapshot.run.current_offset,
            snapshot.run.scanned_windows,
            fmt_rate(snapshot.speed_windows_per_sec),
            snapshot.run.best_score * 100.0,
            snapshot
                .run
                .best_offset
                .map(|value| value.to_string())
                .unwrap_or_else(|| "-".to_string()),
        );
        Ok(())
    }

    fn on_new_best(&mut self, _snapshot: &SearchSnapshot, event: &BestEventRecord) -> Result<()> {
        if self.json {
            println!("{}", serde_json::to_string(event)?);
            return Ok(());
        }
        println!(
            "\nnew best: {:.2}% at offset {} after {} scanned windows",
            event.score * 100.0,
            event.offset,
            event.scanned_windows
        );
        println!("{}", render_preview(&event.bitmap, self.color));
        Ok(())
    }

    fn on_finish(&mut self, snapshot: &SearchSnapshot, reason: FinishReason) -> Result<()> {
        if self.json {
            return Ok(());
        }
        println!(
            "\nfinished: {} (offset={}, scanned={}, best={:.2}%)",
            finish_reason(reason),
            snapshot.run.current_offset,
            snapshot.run.scanned_windows,
            snapshot.run.best_score * 100.0
        );
        match reason {
            FinishReason::PerfectFound => {
                println!("pi fully accepted the shape at this scanned offset.");
            }
            FinishReason::SourceExhausted => {
                println!(
                    "local digit source exhausted. This does not prove no match exists in pi."
                );
            }
            FinishReason::Interrupted
            | FinishReason::LimitReached
            | FinishReason::MaxOffsetReached => {
                println!("progress saved. Resume will continue from the current offset.");
            }
        }
        Ok(())
    }
}

pub struct TuiReporter {
    terminal: Terminal<CrosstermBackend<Stdout>>,
    color: bool,
    last_draw: Instant,
}

impl TuiReporter {
    pub fn new(color: bool) -> Result<Self> {
        enable_raw_mode()?;
        let mut stdout = io::stdout();
        execute!(stdout, EnterAlternateScreen)?;
        let backend = CrosstermBackend::new(stdout);
        let mut terminal = Terminal::new(backend)?;
        terminal.clear()?;
        Ok(Self {
            terminal,
            color,
            last_draw: Instant::now() - Duration::from_secs(60),
        })
    }

    fn draw(&mut self, snapshot: &SearchSnapshot) -> Result<()> {
        let color = self.color;
        self.terminal.draw(|frame| {
            let area = frame.area();
            let rows = Layout::default()
                .direction(Direction::Vertical)
                .constraints([
                    Constraint::Length(8),
                    Constraint::Min(10),
                    Constraint::Length(8),
                ])
                .split(area);
            let top = Paragraph::new(header_lines(snapshot, color))
                .block(Block::default().title(" pi-casso ").borders(Borders::ALL))
                .wrap(Wrap { trim: true });
            frame.render_widget(top, rows[0]);

            let columns = Layout::default()
                .direction(Direction::Horizontal)
                .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
                .split(rows[1]);
            let target = Paragraph::new(bitmap_lines(&snapshot.run.target_bitmap, color, None))
                .block(Block::default().title(" target ").borders(Borders::ALL));
            let best_lines = best_manifestation_lines(&snapshot.run, color);
            let best = Paragraph::new(best_lines).block(
                Block::default()
                    .title(" best manifestation ")
                    .borders(Borders::ALL),
            );
            frame.render_widget(target, columns[0]);
            frame.render_widget(best, columns[1]);

            let history = Paragraph::new(history_lines(&snapshot.recent_events)).block(
                Block::default()
                    .title(" recent improvements ")
                    .borders(Borders::ALL),
            );
            frame.render_widget(history, rows[2]);
        })?;
        Ok(())
    }
}

impl Drop for TuiReporter {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let _ = execute!(self.terminal.backend_mut(), LeaveAlternateScreen);
        let _ = self.terminal.show_cursor();
    }
}

impl SearchReporter for TuiReporter {
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
        self.draw(snapshot)?;
        Ok(())
    }
}

pub fn render_preview(bitmap: &Bitmap, color: bool) -> String {
    if !color {
        return bitmap.render_ascii('#', '.');
    }
    let mut out = String::new();
    for y in 0..bitmap.height {
        for x in 0..bitmap.width {
            if bitmap.get(x, y) == 1 {
                out.push_str("\x1b[36m#\x1b[0m");
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

pub fn print_list(runs: &[RunRecord]) {
    if runs.is_empty() {
        println!("no saved runs");
        return;
    }
    println!(
        "{:<18} {:<12} {:>10} {:>12} {:>9} {:<16}",
        "name", "status", "offset", "scanned", "best", "id"
    );
    for run in runs {
        println!(
            "{:<18} {:<12} {:>10} {:>12} {:>8.2}% {:<16}",
            run.name,
            run.status.as_str(),
            run.current_offset,
            run.scanned_windows,
            run.best_score * 100.0,
            &run.id[..16.min(run.id.len())]
        );
    }
}

pub fn print_status(run: &RunRecord) {
    println!("Run: {}", run.name);
    println!("ID: {}", run.id);
    println!("Status: {}", run.status.as_str());
    println!("Source: {}", run.source.source_type);
    if let Some(path) = &run.source.source_path {
        println!("Source path: {path}");
    }
    println!("Size: {}x{}", run.width, run.height);
    println!("Canvas: {}x{}", run.canvas_width, run.canvas_height);
    println!("Match mode: {}", run.match_mode.as_str());
    println!("Threshold: {}", run.threshold);
    println!("Invert enabled: {}", run.invert_enabled);
    println!("Current offset: {}", run.current_offset);
    println!("Scanned windows: {}", run.scanned_windows);
    if run.source.source_type == "cache" {
        println!("Generated digits: {}", run.generated_digit_count);
    }
    println!("Best score: {:.2}%", run.best_score * 100.0);
    println!(
        "Best offset: {}",
        run.best_offset
            .map(|value| value.to_string())
            .unwrap_or_else(|| "-".to_string())
    );
    if let Some(details) = &run.best_match {
        if let Some(digit) = details.digit {
            println!(
                "Best digit: {digit} at x={}, y={}",
                details.x.unwrap_or_default(),
                details.y.unwrap_or_default()
            );
        }
        if let Some(coverage) = details.coverage {
            println!("Coverage: {:.2}%", coverage * 100.0);
        }
        if let Some(leakage) = details.leakage {
            println!("Leakage: {:.2}%", leakage * 100.0);
        }
    }
    println!("Last checkpoint: {}", run.updated_at);
    println!(
        "Total runtime: {}",
        fmt_duration(Duration::from_secs_f64(run.total_runtime_secs))
    );
}

pub fn print_history(run: &RunRecord, history: &[BestEventRecord], color: bool) {
    if history.is_empty() {
        println!("no best-match improvements recorded for {}", run.name);
        return;
    }
    for event in history {
        println!(
            "\n{} offset={} score={:.2}% scanned={}",
            event.timestamp,
            event.offset,
            event.score * 100.0,
            event.scanned_windows
        );
        println!("{}", render_preview(&event.bitmap, color));
    }
}

pub fn print_best(run: &RunRecord, color: bool) {
    let displayed_score = run
        .best_bitmap
        .as_ref()
        .and_then(|best| {
            if run.best_inverted {
                None
            } else {
                run.target_bitmap.similarity(best).ok()
            }
        })
        .unwrap_or(run.best_score);
    println!("Best match found so far:");
    println!("Run: {}", run.name);
    println!("Score: {:.2}%", displayed_score * 100.0);
    println!(
        "Offset: {}",
        run.best_offset
            .map(|value| value.to_string())
            .unwrap_or_else(|| "-".to_string())
    );
    println!("Size: {}x{}", run.width, run.height);
    println!("\nTarget:");
    println!("{}", render_preview(&run.target_bitmap, color));
    println!("\npi manifestation:");
    if let Some(details) = &run.best_match {
        if let Some(raw) = &details.raw_canvas_digits {
            println!(
                "{}",
                render_digit_canvas(raw, details.canvas_width as usize)
            );
        } else if let Some(best) = &run.best_bitmap {
            println!("{}", render_preview(best, color));
        } else {
            println!("no best match recorded yet");
        }
    } else if let Some(best) = &run.best_bitmap {
        println!("{}", render_preview(best, color));
    } else {
        println!("no best match recorded yet");
    }
    println!("\nResult:");
    println!("{:.2}% similarity", displayed_score * 100.0);
    if displayed_score < 1.0 {
        println!("pi has not fully accepted the shape yet.");
        println!("This only describes the scanned range, not all of pi.");
    }
}

fn header_lines(snapshot: &SearchSnapshot, color: bool) -> Vec<Line<'static>> {
    let run = &snapshot.run;
    let best_offset = run
        .best_offset
        .map(|value| value.to_string())
        .unwrap_or_else(|| "-".to_string());
    let mut lines = vec![
        Line::from(vec![
            styled("run ", color, Color::Gray),
            Span::raw(run.name.clone()),
            styled("  art ", color, Color::Gray),
            Span::raw(
                run.template_name
                    .clone()
                    .unwrap_or_else(|| "custom".to_string()),
            ),
            styled("  source ", color, Color::Gray),
            Span::raw(snapshot.source_kind.clone()),
            styled("  mode ", color, Color::Gray),
            Span::raw(run.match_mode.as_str().to_string()),
        ]),
        Line::raw(format!(
            "offset={} scanned={} source_digits={} canvas={}x{}",
            run.current_offset,
            run.scanned_windows,
            snapshot.source_len,
            run.canvas_width,
            run.canvas_height
        )),
        source_detail_line(snapshot),
        Line::raw(format!(
            "speed={}/s windows  digits={}/s  best={:.2}% at {}",
            fmt_rate(snapshot.speed_windows_per_sec),
            fmt_rate(snapshot.speed_digits_per_sec),
            run.best_score * 100.0,
            best_offset
        )),
        Line::raw(format!(
            "elapsed={} total_runtime={}",
            fmt_duration(snapshot.session_elapsed),
            fmt_duration(Duration::from_secs_f64(run.total_runtime_secs)),
        )),
    ];
    if let Some(progress) = snapshot.progress {
        lines.push(Line::raw(format!(
            "bounded progress={:.2}%",
            progress * 100.0
        )));
    } else {
        lines.push(Line::raw("bounded progress=unbounded search"));
    }
    if snapshot.source_kind == "demo" {
        lines.push(Line::raw(
            "demo pi source only; use --pi-file for a meaningful local scan",
        ));
    }
    lines
}

fn best_manifestation_lines(run: &RunRecord, color: bool) -> Vec<Line<'static>> {
    if let Some(details) = &run.best_match {
        if let Some(raw) = &details.raw_canvas_digits {
            return digit_canvas_lines(raw, details, &run.target_bitmap, color);
        }
    }
    if let Some(best) = &run.best_bitmap {
        bitmap_lines(best, color, Some((&run.target_bitmap, run.best_inverted)))
    } else {
        vec![Line::raw("no match has improved the starting score yet")]
    }
}

fn digit_canvas_lines(
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
    let mut lines = Vec::with_capacity(height);
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

fn render_digit_canvas(raw: &str, width: usize) -> String {
    if width == 0 {
        return raw.to_string();
    }
    raw.as_bytes()
        .chunks(width)
        .map(|chunk| String::from_utf8_lossy(chunk).into_owned())
        .collect::<Vec<_>>()
        .join("\n")
}

fn source_detail_line(snapshot: &SearchSnapshot) -> Line<'static> {
    let status = live_status_label(snapshot);
    if snapshot.source_is_growing {
        Line::raw(format!(
            "generated={} cache_gap={} status={status}",
            snapshot.source_len, snapshot.cache_gap_digits
        ))
    } else {
        Line::raw(format!(
            "source={} source_digits={} status={status}",
            snapshot.source_kind, snapshot.source_len
        ))
    }
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

fn bitmap_lines(
    bitmap: &Bitmap,
    color: bool,
    compare: Option<(&Bitmap, bool)>,
) -> Vec<Line<'static>> {
    let mut lines = Vec::with_capacity(bitmap.height);
    for y in 0..bitmap.height {
        let mut spans = Vec::with_capacity(bitmap.width);
        for x in 0..bitmap.width {
            let pixel = bitmap.get(x, y);
            let symbol = if pixel == 1 { "#" } else { "." };
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
        return vec![Line::raw("no improvements yet")];
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

fn styled(text: &str, color: bool, fg: Color) -> Span<'static> {
    if color {
        Span::styled(text.to_string(), Style::default().fg(fg))
    } else {
        Span::raw(text.to_string())
    }
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

fn finish_reason(reason: FinishReason) -> &'static str {
    match reason {
        FinishReason::PerfectFound => "perfect match found",
        FinishReason::SourceExhausted => "source exhausted",
        FinishReason::Interrupted => "interrupted",
        FinishReason::LimitReached => "limit reached",
        FinishReason::MaxOffsetReached => "max offset reached",
    }
}
