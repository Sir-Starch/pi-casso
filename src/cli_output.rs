//! Plain, non-interactive terminal output: everything printed by `list`,
//! `status`, `history`, `show-best`, and by a search running with `--no-tui`.
//!
//! Rendering primitives come from `crate::render`, which the TUI shares, so a
//! label or a bitmap looks the same in both.

use std::time::{Duration, Instant};

use anyhow::Result;

use crate::render::{
    Theme, finish_reason_label, fmt_duration, fmt_rate, opt_u64, render_digit_canvas,
    render_preview, snapshot_state,
};
use crate::search::{FinishReason, SearchReporter, SearchSnapshot};
use crate::storage::{BestEventRecord, RunRecord};

/// Line-per-update progress reporter. Deliberately terse: this output is what
/// ends up in log files and CI transcripts.
pub struct PlainReporter {
    theme: Theme,
    json: bool,
    last_update: Instant,
}

impl PlainReporter {
    pub fn new(theme: Theme, json: bool) -> Self {
        Self {
            theme,
            json,
            // Backdated so the very first update prints immediately.
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
        // The generation rate is only worth a column when a producer exists and
        // is doing something; on a plain file it would always read zero.
        let generation = snapshot
            .generation
            .filter(|progress| progress.active || progress.digits_per_sec > 0.0)
            .map(|progress| format!(" generating={}/s", fmt_rate(progress.digits_per_sec)))
            .unwrap_or_default();
        println!(
            "[{}] {} offset={} scanned={} speed={}/s avg={}/s{generation} best={:.2}% at offset={}",
            fmt_duration(snapshot.session_elapsed),
            snapshot_state(snapshot).label(),
            snapshot.run.current_offset,
            snapshot.run.scanned_windows,
            fmt_rate(snapshot.speed_windows_per_sec),
            fmt_rate(snapshot.average_windows_per_sec),
            snapshot.run.best_score * 100.0,
            opt_u64(snapshot.run.best_offset),
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
        println!("{}", render_preview(&event.bitmap, &self.theme));
        Ok(())
    }

    fn on_finish(&mut self, snapshot: &SearchSnapshot, reason: FinishReason) -> Result<()> {
        if self.json {
            return Ok(());
        }
        println!(
            "\nfinished: {} (offset={}, scanned={}, best={:.2}%)",
            finish_reason_label(reason),
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
    println!("Best offset: {}", opt_u64(run.best_offset));
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

pub fn print_history(run: &RunRecord, history: &[BestEventRecord], theme: &Theme) {
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
        println!("{}", render_preview(&event.bitmap, theme));
    }
}

pub fn print_best(run: &RunRecord, theme: &Theme) {
    // An inverted match scores against the inverse of the target, so recomputing
    // plain similarity would understate it; fall back to the stored score there.
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
    println!("Offset: {}", opt_u64(run.best_offset));
    println!("Size: {}x{}", run.width, run.height);
    println!("\nTarget:");
    println!("{}", render_preview(&run.target_bitmap, theme));
    println!("\npi manifestation:");
    match run
        .best_match
        .as_ref()
        .and_then(|details| details.raw_canvas_digits.as_ref().map(|raw| (raw, details)))
    {
        Some((raw, details)) => {
            println!(
                "{}",
                render_digit_canvas(raw, details.canvas_width as usize)
            );
        }
        None => match &run.best_bitmap {
            Some(best) => println!("{}", render_preview(best, theme)),
            None => println!("no best match recorded yet"),
        },
    }
    println!("\nResult:");
    println!("{:.2}% similarity", displayed_score * 100.0);
    if displayed_score < 1.0 {
        println!("pi has not fully accepted the shape yet.");
        println!("This only describes the scanned range, not all of pi.");
    }
}
