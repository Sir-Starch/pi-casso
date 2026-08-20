//! Read-only and housekeeping commands over saved runs, plus template listing
//! and previewing.

use anyhow::Result;

use crate::art::{self, ArtMapping};
use crate::cli::{ExportArgs, ExportFormat, HistoryArgs, PreviewArgs, RunArg};
use crate::cli_output::{print_best, print_history, print_list, print_status};
use crate::commands::hunt::{load_art_for, resolve_dimensions};
use crate::commands::{CommandContext, print_json};
use crate::render::render_preview;
use crate::storage::Storage;

pub fn templates() -> Result<()> {
    for name in art::template_names() {
        println!("{name}");
    }
    Ok(())
}

pub fn preview(args: PreviewArgs, context: &CommandContext) -> Result<()> {
    let (width, height) = resolve_dimensions(args.mode, args.width, args.height, 16)?;
    let bitmap = load_art_for(
        args.template.as_deref(),
        args.file.as_ref(),
        width,
        height,
        &ArtMapping::from_cli(args.empty.as_deref(), args.filled.as_deref()),
    )?;
    println!("{}", render_preview(&bitmap, &context.theme));
    Ok(())
}

pub fn list(context: &CommandContext) -> Result<()> {
    let storage = Storage::open_default()?;
    let runs = storage.list_runs()?;
    if context.json {
        print_json(&runs)
    } else {
        print_list(&runs);
        Ok(())
    }
}

pub fn status(args: RunArg, context: &CommandContext) -> Result<()> {
    let storage = Storage::open_default()?;
    let run = storage.resolve_run(&args.run)?;
    if context.json {
        print_json(&run)
    } else {
        print_status(&run);
        Ok(())
    }
}

pub fn history(args: HistoryArgs, context: &CommandContext) -> Result<()> {
    let storage = Storage::open_default()?;
    let run = storage.resolve_run(&args.run)?;
    let history = storage.history(&run.id, args.limit)?;
    if context.json {
        print_json(&history)
    } else {
        print_history(&run, &history, &context.theme);
        Ok(())
    }
}

pub fn show_best(args: RunArg, context: &CommandContext) -> Result<()> {
    let storage = Storage::open_default()?;
    let run = storage.resolve_run(&args.run)?;
    if context.json {
        print_json(&run.best_summary())
    } else {
        print_best(&run, &context.theme);
        Ok(())
    }
}

pub fn export(args: ExportArgs) -> Result<()> {
    let storage = Storage::open_default()?;
    let run = storage.resolve_run(&args.run)?;
    let history = storage.history(&run.id, None)?;
    match args.format {
        ExportFormat::Json => print_json(&serde_json::json!({
            "run": run,
            "history": history,
        })),
    }
}

pub fn delete(args: RunArg) -> Result<()> {
    let mut storage = Storage::open_default()?;
    let deleted = storage.delete_run(&args.run)?;
    println!("deleted run {} ({})", deleted.name, deleted.id);
    Ok(())
}
