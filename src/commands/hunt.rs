//! `start`, `hunt` and `resume`: creating a run and driving it to a reporter.

use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, Result, bail};

use crate::art::{self, ArtMapping, Bitmap};
use crate::cli::{MatchModeArg, ResumeArgs, SizeMode, StartArgs, StressTestArgs};
use crate::cli_output::PlainReporter;
use crate::commands::{CommandContext, print_json};
use crate::digits::{self, DigitSource, DigitSourceSpec, FileDigitSource};
use crate::performance::{PerformanceOverrides, PerformanceProfile, PerformanceSettings};
use crate::pi;
use crate::search::{MatchMode, SearchOptions, run_search};
use crate::storage::{self, NewRun, RunStatus, Storage};
use crate::tui::live::LiveReporter;

pub fn start_or_hunt(args: StartArgs, hunt_command: bool, context: &CommandContext) -> Result<()> {
    let json = context.json;
    let infinite = args.infinite || hunt_command;
    if hunt_command && !args.infinite {
        bail!("hunt requires --infinite for endless pi hunting mode");
    }
    confirm_max_start(&args, args.no_tui || json)?;
    if infinite {
        eprintln!(
            "warning: Perfect matches for large sprites may be astronomically unlikely. This mode is designed to run indefinitely."
        );
    }

    let mut storage = Storage::open_default()?;
    let mapping = ArtMapping::from_cli(args.empty.as_deref(), args.filled.as_deref());
    let default_size = if infinite { 12 } else { 16 };
    let (width, height) = resolve_dimensions(args.mode, args.width, args.height, default_size)?;
    let match_mode = match_mode_arg(args.match_mode);
    let performance = performance_from_start(&args, match_mode);
    let (canvas_width, canvas_height) = resolve_canvas_dimensions(
        args.canvas_width,
        args.canvas_height,
        match_mode,
        width,
        height,
    )?;
    let target = load_art(
        args.template.as_deref(),
        args.file.as_ref(),
        width,
        height,
        &mapping,
    )?;
    let cache = pi::PiCache::default()?;
    let source = if infinite {
        cache.ensure_parent()?;
        DigitSourceSpec::cache(cache.path().clone())
    } else if let Some(path) = args.pi_file.as_ref() {
        let canonical = path
            .canonicalize()
            .with_context(|| format!("could not resolve pi digit file {}", path.display()))?;
        FileDigitSource::new_with_options(canonical.clone(), args.allow_decimal_prefix)
            .validate()?;
        DigitSourceSpec::file(canonical, args.allow_decimal_prefix)
    } else {
        eprintln!(
            "warning: using the tiny built-in pi demo source; provide --pi-file for real searches"
        );
        DigitSourceSpec::demo()
    };

    let generated_digit_count = if infinite { cache.digit_count()? } else { 0 };
    let source_impl = source.open()?;
    let params = serde_json::json!({
        "start_offset": args.start_offset,
        "max_offset": args.max_offset,
        "limit": args.limit,
        "top": args.top,
        "allow_decimal_prefix": args.allow_decimal_prefix,
        "infinite": infinite,
        "match_mode": match_mode.as_str(),
        "canvas_width": canvas_width,
        "canvas_height": canvas_height,
        "keep_going_after_perfect": args.keep_going_after_perfect,
        "profile": performance.profile.as_str(),
        "backend": performance.backend.as_str(),
        "generator_backend": performance.generator_backend.as_str(),
        "gpu": performance.gpu.as_str(),
        "thermal_mode": performance.thermal_mode.as_str(),
        "chunk_size": performance.limits.chunk_size,
        "queue_depth": performance.limits.queue_depth,
        "memory_limit_mb": performance.limits.memory_limit_mb,
    });
    let run = storage.create_run(NewRun {
        name: args.name,
        source,
        template_name: args.template,
        art_hash: target.sha256(),
        width: target.width as u32,
        height: target.height as u32,
        canvas_width: canvas_width as u32,
        canvas_height: canvas_height as u32,
        match_mode,
        threshold: args.threshold,
        invert_enabled: args.invert,
        start_offset: args.start_offset,
        target_bitmap: target,
        generated_digit_count,
        params_json: params.to_string(),
    })?;

    let options = SearchOptions {
        max_offset: if infinite { None } else { args.max_offset },
        limit: if infinite && !args.stress_test {
            None
        } else {
            args.limit
        },
        match_mode,
        canvas_width,
        canvas_height,
        threshold: args.threshold,
        invert: args.invert,
        workers: args.cpu_workers.or(args.workers),
        checkpoint_every: Duration::from_secs(performance.limits.checkpoint_every_secs),
        top_n: args.top,
        keep_going_after_perfect: args.keep_going_after_perfect,
        chunk_windows: performance.limits.chunk_size,
        performance,
    };
    run_search_with_reporter(
        &mut storage,
        run,
        source_impl.as_ref(),
        options,
        args.no_tui || json,
        context,
    )?;
    Ok(())
}

/// `plain` covers both `--no-tui` and `--json`: neither can share the terminal
/// with a full-screen live view.
pub(crate) fn run_search_with_reporter(
    storage: &mut Storage,
    run: storage::RunRecord,
    source: &dyn digits::DigitSource,
    options: SearchOptions,
    plain: bool,
    context: &CommandContext,
) -> Result<()> {
    if plain {
        let mut reporter = PlainReporter::new(context.theme, context.json);
        let final_run = run_search(storage, run, source, options, &mut reporter)?;
        if context.json {
            print_json(&final_run)?;
        }
    } else {
        let mut reporter = LiveReporter::new(context.theme)?;
        run_search(storage, run, source, options, &mut reporter)?;
    }
    Ok(())
}

pub(crate) fn load_art(
    template: Option<&str>,
    file: Option<&PathBuf>,
    width: usize,
    height: usize,
    mapping: &ArtMapping,
) -> Result<Bitmap> {
    match (template, file) {
        (Some(name), None) => art::load_template(name, width, height),
        (None, Some(path)) => {
            let contents = std::fs::read_to_string(path)
                .with_context(|| format!("failed to read art file {}", path.display()))?;
            Bitmap::from_ascii(&contents, width, height, mapping)
        }
        (Some(_), Some(_)) => bail!("choose either --template or --file, not both"),
        (None, None) => bail!("provide --template or --file"),
    }
}

pub(crate) fn resolve_dimensions(
    mode: Option<SizeMode>,
    width: Option<usize>,
    height: Option<usize>,
    default_size: usize,
) -> Result<(usize, usize)> {
    if let Some(mode) = mode {
        if width.is_some() || height.is_some() {
            bail!("choose either --mode or custom --width/--height, not both");
        }
        return Ok(mode.dimensions());
    }

    let resolved_width = width.unwrap_or(default_size);
    let resolved_height = height.unwrap_or(default_size);
    if resolved_width == 0 || resolved_height == 0 {
        bail!("width and height must be greater than zero");
    }
    Ok((resolved_width, resolved_height))
}

pub(crate) fn resolve_canvas_dimensions(
    canvas_width: Option<usize>,
    canvas_height: Option<usize>,
    match_mode: MatchMode,
    target_width: usize,
    target_height: usize,
) -> Result<(usize, usize)> {
    if match_mode == MatchMode::Emergence {
        let resolved_width = canvas_width.unwrap_or(24);
        let resolved_height = canvas_height.unwrap_or(24);
        if resolved_width < target_width || resolved_height < target_height {
            bail!(
                "canvas size {}x{} must be at least target size {}x{}",
                resolved_width,
                resolved_height,
                target_width,
                target_height
            );
        }
        Ok((resolved_width, resolved_height))
    } else {
        if canvas_width.is_some() || canvas_height.is_some() {
            bail!("--canvas-width/--canvas-height are only valid with --match-mode emergence");
        }
        Ok((target_width, target_height))
    }
}

pub(crate) fn match_mode_arg(value: MatchModeArg) -> MatchMode {
    match value {
        MatchModeArg::Emergence => MatchMode::Emergence,
        MatchModeArg::Threshold => MatchMode::Threshold,
        MatchModeArg::Exact => MatchMode::Exact,
    }
}

pub(crate) fn performance_from_start(
    args: &StartArgs,
    match_mode: MatchMode,
) -> PerformanceSettings {
    PerformanceSettings::from_profile(
        args.profile,
        args.backend,
        args.generator_backend,
        args.gpu,
        args.gpu_device.clone(),
        args.thermal_mode,
        args.stress_test,
        args.show_metrics,
        match_mode,
        PerformanceOverrides {
            cpu_workers: args.cpu_workers.or(args.workers),
            cpu_utilization: args.cpu_utilization,
            gpu_utilization: args.gpu_utilization,
            chunk_size: args.chunk_size,
            queue_depth: args.queue_depth,
            memory_limit_mb: args.memory_limit_mb,
            ui_refresh_ms: args.ui_refresh_ms,
            checkpoint_every_secs: Some(args.checkpoint_every),
            background_yield_ms: args.background_yield_ms,
            max_fps: args.max_fps,
            pause_when_on_battery: args.pause_when_on_battery,
        },
    )
}

pub(crate) fn performance_from_resume(
    args: &ResumeArgs,
    match_mode: MatchMode,
) -> PerformanceSettings {
    PerformanceSettings::from_profile(
        args.profile,
        args.backend,
        args.generator_backend,
        args.gpu,
        args.gpu_device.clone(),
        args.thermal_mode,
        args.stress_test,
        args.show_metrics,
        match_mode,
        PerformanceOverrides {
            cpu_workers: args.cpu_workers.or(args.workers),
            cpu_utilization: args.cpu_utilization,
            gpu_utilization: args.gpu_utilization,
            chunk_size: args.chunk_size,
            queue_depth: args.queue_depth,
            memory_limit_mb: args.memory_limit_mb,
            ui_refresh_ms: args.ui_refresh_ms,
            checkpoint_every_secs: Some(args.checkpoint_every),
            background_yield_ms: args.background_yield_ms,
            max_fps: args.max_fps,
            pause_when_on_battery: args.pause_when_on_battery,
        },
    )
}

pub(crate) fn confirm_max_start(args: &StartArgs, non_interactive: bool) -> Result<()> {
    confirm_max_mode(
        args.profile,
        args.stress_test,
        args.yes || args.force,
        non_interactive,
    )
}

pub(crate) fn confirm_max_resume(args: &ResumeArgs, non_interactive: bool) -> Result<()> {
    confirm_max_mode(
        args.profile,
        args.stress_test,
        args.yes || args.force,
        non_interactive,
    )
}

pub(crate) fn confirm_max_stress(args: &StressTestArgs, non_interactive: bool) -> Result<()> {
    confirm_max_mode(args.profile, true, args.yes || args.force, non_interactive)
}

pub(crate) fn confirm_max_mode(
    profile: PerformanceProfile,
    stress_test: bool,
    confirmed: bool,
    non_interactive: bool,
) -> Result<()> {
    if confirmed {
        return Ok(());
    }
    if profile != PerformanceProfile::Max && !stress_test {
        return Ok(());
    }
    let warning = "Max mode may heavily load CPU, GPU, and cooling. Continue? [y/N]";
    if non_interactive {
        bail!("max/stress-test mode requires --yes or --force in non-interactive mode");
    }
    eprint!("{warning} ");
    let mut answer = String::new();
    std::io::stdin().read_line(&mut answer)?;
    if matches!(answer.trim(), "y" | "Y" | "yes" | "YES") {
        Ok(())
    } else {
        bail!("cancelled")
    }
}

pub fn resume(args: ResumeArgs, context: &CommandContext) -> Result<()> {
    let plain = args.no_tui || context.json;
    confirm_max_resume(&args, plain)?;
    let mut storage = Storage::open_default()?;
    let run = storage.resolve_run(&args.run)?;
    if run.status == RunStatus::PerfectFound && !args.keep_going_after_perfect {
        println!(
            "run {} already found a 100% match at offset {}",
            run.name,
            run.best_offset.unwrap_or_default()
        );
        return Ok(());
    }
    if run.status == RunStatus::SourceExhausted {
        println!(
            "run {} already exhausted its local digit source at offset {}",
            run.name, run.current_offset
        );
        return Ok(());
    }
    let source = run.source.open()?;
    let performance = performance_from_resume(&args, run.match_mode);
    let options = SearchOptions {
        max_offset: args.max_offset,
        limit: args.limit,
        match_mode: run.match_mode,
        canvas_width: run.canvas_width as usize,
        canvas_height: run.canvas_height as usize,
        threshold: run.threshold,
        invert: run.invert_enabled,
        workers: args.cpu_workers.or(args.workers),
        checkpoint_every: Duration::from_secs(performance.limits.checkpoint_every_secs),
        top_n: args.top.unwrap_or_else(|| run.top_matches.len().max(10)),
        keep_going_after_perfect: args.keep_going_after_perfect,
        chunk_windows: performance.limits.chunk_size,
        performance,
    };
    run_search_with_reporter(&mut storage, run, source.as_ref(), options, plain, context)
}

/// Shared by the wizard in the TUI, which needs the same art-loading rules as
/// `--template` / `--file` on the command line.
pub(crate) fn load_art_for(
    template: Option<&str>,
    file: Option<&PathBuf>,
    width: usize,
    height: usize,
    mapping: &ArtMapping,
) -> Result<Bitmap> {
    load_art(template, file, width, height, mapping)
}
