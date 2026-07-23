mod art;
mod cli;
mod digits;
mod gpu;
mod interactive;
mod performance;
mod pi;
mod search;
mod storage;
mod ui;

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, atomic::AtomicBool};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};
use clap::Parser;
use serde::Serialize;

use crate::art::{ArtMapping, Bitmap};
use crate::cli::{
    BenchmarkArgs, Cli, Commands, ExportFormat, GpuCommands, MatchModeArg, PiCommands, ResumeArgs,
    SizeMode, StartArgs, StressTestArgs,
};
use crate::digits::{DigitSource, DigitSourceSpec, FileDigitSource};
use crate::performance::{
    GeneratorBackendChoice, GpuMode, PerformanceOverrides, PerformanceProfile, PerformanceSettings,
    SearchBackendChoice, ThermalMode,
};
use crate::search::{
    FinishReason, MatchMode, SearchOptions, SearchReporter, SearchSnapshot, run_search,
};
use crate::storage::{BestEventRecord, NewRun, RunStatus, Storage};
use crate::ui::{
    PlainReporter, TuiReporter, print_best, print_history, print_list, print_status, render_preview,
};

fn main() {
    if let Err(err) = real_main() {
        eprintln!("error: {err:#}");
        std::process::exit(1);
    }
}

fn real_main() -> Result<()> {
    let cli = Cli::parse();
    let color = !cli.no_color && std::env::var_os("NO_COLOR").is_none();

    match cli.command {
        None => interactive::run(color)?,
        Some(command) => match command {
            Commands::Templates => {
                for name in art::template_names() {
                    println!("{name}");
                }
            }
            Commands::Preview(args) => {
                let (width, height) = resolve_dimensions(args.mode, args.width, args.height, 16)?;
                let bitmap = load_art(
                    args.template.as_deref(),
                    args.file.as_ref(),
                    width,
                    height,
                    &ArtMapping::from_cli(args.empty.as_deref(), args.filled.as_deref()),
                )?;
                println!("{}", render_preview(&bitmap, color));
            }
            Commands::Start(args) => start_or_hunt(args, false, cli.json, color)?,
            Commands::Hunt(args) => start_or_hunt(args, true, cli.json, color)?,
            Commands::Resume(args) => {
                confirm_max_resume(&args, args.no_tui || cli.json)?;
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
                run_search_with_reporter(
                    &mut storage,
                    run,
                    source.as_ref(),
                    options,
                    args.no_tui || cli.json,
                    cli.json,
                    color,
                )?;
            }
            Commands::List => {
                let storage = Storage::open_default()?;
                let runs = storage.list_runs()?;
                if cli.json {
                    print_json(&runs)?;
                } else {
                    print_list(&runs);
                }
            }
            Commands::Status(args) => {
                let storage = Storage::open_default()?;
                let run = storage.resolve_run(&args.run)?;
                if cli.json {
                    print_json(&run)?;
                } else {
                    print_status(&run);
                }
            }
            Commands::History(args) => {
                let storage = Storage::open_default()?;
                let run = storage.resolve_run(&args.run)?;
                let history = storage.history(&run.id, args.limit)?;
                if cli.json {
                    print_json(&history)?;
                } else {
                    print_history(&run, &history, color);
                }
            }
            Commands::ShowBest(args) => {
                let storage = Storage::open_default()?;
                let run = storage.resolve_run(&args.run)?;
                if cli.json {
                    print_json(&run.best_summary())?;
                } else {
                    print_best(&run, color);
                }
            }
            Commands::Export(args) => {
                let storage = Storage::open_default()?;
                let run = storage.resolve_run(&args.run)?;
                let history = storage.history(&run.id, None)?;
                match args.format {
                    ExportFormat::Json => print_json(&serde_json::json!({
                        "run": run,
                        "history": history,
                    }))?,
                }
            }
            Commands::Delete(args) => {
                let mut storage = Storage::open_default()?;
                let deleted = storage.delete_run(&args.run)?;
                println!("deleted run {} ({})", deleted.name, deleted.id);
            }
            Commands::Benchmark(args) => run_benchmark(args, cli.json)?,
            Commands::StressTest(args) => run_stress_test(args, cli.json, color)?,
            Commands::Gpu { command } => match command {
                GpuCommands::Info => print_gpu_info(),
            },
            Commands::Pi { command } => match command {
                PiCommands::Import {
                    file,
                    allow_decimal_prefix,
                } => {
                    let source =
                        FileDigitSource::new_with_options(file.clone(), allow_decimal_prefix);
                    let cache = pi::PiCache::default()?;
                    let dest = cache.path().clone();
                    if let Some(parent) = dest.parent() {
                        std::fs::create_dir_all(parent)?;
                    }
                    let imported = source.copy_digits_to(&dest)?;
                    println!("imported {} digits into {}", imported, dest.display());
                }
                PiCommands::CacheInfo | PiCommands::Info => {
                    let cache = pi::PiCache::default()?;
                    let info = cache.info()?;
                    println!("cache path: {}", info.path.display());
                    println!("generated/usable digits: {}", info.digits);
                    println!("size on disk: {} bytes", info.bytes);
                }
                PiCommands::Generate {
                    digits,
                    generator_backend,
                    y_cruncher_path,
                    workers,
                } => {
                    let cache = pi::PiCache::default()?;
                    let generated = generate_pi_cache(
                        &cache,
                        digits,
                        generator_backend,
                        y_cruncher_path,
                        workers,
                    )?;
                    let info = cache.info()?;
                    println!(
                        "generated {} digits into {} (total={})",
                        generated,
                        info.path.display(),
                        info.digits
                    );
                }
            },
        },
    }

    Ok(())
}

fn generate_pi_cache(
    cache: &pi::PiCache,
    digits: u64,
    backend: GeneratorBackendChoice,
    y_cruncher_path: Option<PathBuf>,
    workers: Option<usize>,
) -> Result<u64> {
    match (backend, y_cruncher_path.or_else(find_y_cruncher)) {
        (GeneratorBackendChoice::YCruncher, Some(path)) => {
            generate_pi_cache_with_y_cruncher(cache, digits, &path, workers)
        }
        (GeneratorBackendChoice::YCruncher, None) => bail!(
            "y-cruncher was requested but not found; pass --y-cruncher-path or put y-cruncher in PATH"
        ),
        (GeneratorBackendChoice::Auto, Some(path)) => {
            generate_pi_cache_with_y_cruncher(cache, digits, &path, workers)
        }
        (GeneratorBackendChoice::Auto | GeneratorBackendChoice::Cpu, _) => {
            let stop = Arc::new(AtomicBool::new(false));
            pi::generate_into_cache(cache, digits, stop)
        }
    }
}

fn generate_pi_cache_with_y_cruncher(
    cache: &pi::PiCache,
    digits: u64,
    y_cruncher: &Path,
    workers: Option<usize>,
) -> Result<u64> {
    cache.ensure_parent()?;
    let existing = cache.digit_count()?;
    let target_digits = existing
        .checked_add(digits)
        .context("requested pi digit count overflowed")?;
    if digits == 0 {
        return Ok(0);
    }

    let temp_dir = make_y_cruncher_temp_dir()?;
    let mut command = Command::new(y_cruncher);
    command
        .arg("pause:-2")
        .arg("skip-warnings")
        .arg("colors:0")
        .arg("custom")
        .arg("pi")
        .arg(format!("-dec:{}", target_digits.saturating_sub(1)))
        .arg("-hex:0")
        .arg("-od:1")
        .arg("-compress:0")
        .arg("-verify:0")
        .arg("-mode:ram")
        .arg("-o")
        .arg(&temp_dir);
    if let Some(workers) = workers {
        command.arg(format!("-TD:{workers}"));
    }

    let status = command
        .status()
        .with_context(|| format!("failed to run y-cruncher at {}", y_cruncher.display()))?;
    if !status.success() {
        bail!("y-cruncher exited with status {status}");
    }

    let copied =
        append_y_cruncher_output(cache, &temp_dir, &temp_dir, existing).with_context(|| {
            format!(
                "failed to import y-cruncher output from {}",
                temp_dir.display()
            )
        })?;
    let _ = std::fs::remove_dir_all(&temp_dir);
    Ok(copied)
}

fn append_y_cruncher_output(
    cache: &pi::PiCache,
    temp_dir: &Path,
    preferred_output: &Path,
    start_digit: u64,
) -> Result<u64> {
    let mut candidates = Vec::new();
    if preferred_output.is_file() {
        candidates.push(preferred_output.to_path_buf());
    }
    collect_files(temp_dir, &mut candidates)?;

    let mut errors = Vec::new();
    for candidate in candidates {
        let source = FileDigitSource::new_with_options(candidate.clone(), true);
        match source.validate() {
            Ok(()) => {
                let total_digits = source.len()?;
                if total_digits <= start_digit {
                    continue;
                }
                return source.append_digits_from_to(cache.path(), start_digit);
            }
            Err(err) => errors.push(format!("{}: {err:#}", candidate.display())),
        }
    }

    if errors.is_empty() {
        bail!("y-cruncher did not produce a reusable digit file");
    }
    bail!(
        "y-cruncher output did not contain a valid pi digit file:\n{}",
        errors.join("\n")
    )
}

fn collect_files(dir: &Path, files: &mut Vec<PathBuf>) -> Result<()> {
    if !dir.exists() {
        return Ok(());
    }
    for entry in std::fs::read_dir(dir)
        .with_context(|| format!("failed to read directory {}", dir.display()))?
    {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            collect_files(&path, files)?;
        } else if path.is_file() && !files.iter().any(|existing| existing == &path) {
            files.push(path);
        }
    }
    Ok(())
}

fn make_y_cruncher_temp_dir() -> Result<PathBuf> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("system clock is before the Unix epoch")?
        .as_nanos();
    let dir =
        std::env::temp_dir().join(format!("pi-casso-y-cruncher-{}-{now}", std::process::id()));
    std::fs::create_dir_all(&dir).with_context(|| format!("failed to create {}", dir.display()))?;
    Ok(dir)
}

fn find_y_cruncher() -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path) {
        let candidate = dir.join("y-cruncher");
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

fn start_or_hunt(args: StartArgs, hunt_command: bool, json: bool, color: bool) -> Result<()> {
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
        json,
        color,
    )?;
    Ok(())
}

fn run_search_with_reporter(
    storage: &mut Storage,
    run: storage::RunRecord,
    source: &dyn digits::DigitSource,
    options: SearchOptions,
    plain: bool,
    json: bool,
    color: bool,
) -> Result<()> {
    if plain {
        let mut reporter = PlainReporter::new(color, json);
        let final_run = run_search(storage, run, source, options, &mut reporter)?;
        if json {
            print_json(&final_run)?;
        }
    } else {
        let mut reporter = TuiReporter::new(color)?;
        run_search(storage, run, source, options, &mut reporter)?;
    }
    Ok(())
}

fn load_art(
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

fn resolve_dimensions(
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

fn resolve_canvas_dimensions(
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

fn match_mode_arg(value: MatchModeArg) -> MatchMode {
    match value {
        MatchModeArg::Emergence => MatchMode::Emergence,
        MatchModeArg::Threshold => MatchMode::Threshold,
        MatchModeArg::Exact => MatchMode::Exact,
    }
}

fn performance_from_start(args: &StartArgs, match_mode: MatchMode) -> PerformanceSettings {
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

fn performance_from_resume(args: &ResumeArgs, match_mode: MatchMode) -> PerformanceSettings {
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

fn confirm_max_start(args: &StartArgs, non_interactive: bool) -> Result<()> {
    confirm_max_mode(
        args.profile,
        args.stress_test,
        args.yes || args.force,
        non_interactive,
    )
}

fn confirm_max_resume(args: &ResumeArgs, non_interactive: bool) -> Result<()> {
    confirm_max_mode(
        args.profile,
        args.stress_test,
        args.yes || args.force,
        non_interactive,
    )
}

fn confirm_max_stress(args: &StressTestArgs, non_interactive: bool) -> Result<()> {
    confirm_max_mode(args.profile, true, args.yes || args.force, non_interactive)
}

fn confirm_max_mode(
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

fn print_gpu_info() {
    println!("CPU backend: available=true");
    let adapters = gpu::list_adapters();
    println!("GPU backend: available={}", !adapters.is_empty());
    if adapters.is_empty() {
        println!("GPU status: no compatible adapters reported by wgpu");
        return;
    }
    for (index, adapter) in adapters.iter().enumerate() {
        println!(
            "[{index}] {}  type={}  backend={}  driver={} {}",
            adapter.name, adapter.device_type, adapter.backend, adapter.driver, adapter.driver_info
        );
    }
}

fn run_benchmark(args: BenchmarkArgs, json: bool) -> Result<()> {
    let template = args.template.as_deref().unwrap_or("arch");
    let target = art::load_template(template, 12, 12)?;
    let match_mode = MatchMode::Emergence;
    let performance = PerformanceSettings::from_profile(
        args.profile,
        args.backend,
        args.generator_backend,
        args.gpu,
        args.gpu_device,
        ThermalMode::Normal,
        false,
        args.show_metrics,
        match_mode,
        PerformanceOverrides {
            cpu_workers: args.cpu_workers,
            cpu_utilization: args.cpu_utilization,
            chunk_size: args.chunk_size,
            checkpoint_every_secs: Some(args.seconds.max(1)),
            ..PerformanceOverrides::default()
        },
    );
    let cache = pi::PiCache::default()?;
    if cache.digit_count()? < 20_000 {
        let stop = Arc::new(AtomicBool::new(false));
        let _ = pi::generate_into_cache(&cache, 20_000, stop)?;
    }
    let source = DigitSourceSpec::cache(cache.path().clone());
    let source_impl = source.open()?;
    let db_path = std::env::temp_dir().join(format!(
        "pi-casso-benchmark-{}.db",
        chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default()
    ));
    let mut storage = Storage::open_path(db_path)?;
    let run = storage.create_run(NewRun {
        name: "benchmark".to_string(),
        source,
        template_name: Some(template.to_string()),
        art_hash: target.sha256(),
        width: target.width as u32,
        height: target.height as u32,
        canvas_width: 24,
        canvas_height: 24,
        match_mode,
        threshold: 5,
        invert_enabled: false,
        start_offset: Some(0),
        target_bitmap: target,
        generated_digit_count: cache.digit_count()?,
        params_json: "{}".to_string(),
    })?;
    let options = SearchOptions {
        max_offset: None,
        limit: Some(performance.limits.chunk_size as u64 * args.seconds.max(1)),
        match_mode,
        canvas_width: 24,
        canvas_height: 24,
        threshold: 5,
        invert: false,
        workers: Some(performance.limits.cpu_workers),
        checkpoint_every: Duration::from_secs(performance.limits.checkpoint_every_secs),
        top_n: 3,
        keep_going_after_perfect: true,
        chunk_windows: performance.limits.chunk_size,
        performance,
    };
    let mut reporter = SilentReporter;
    let started = std::time::Instant::now();
    let final_run = run_search(
        &mut storage,
        run,
        source_impl.as_ref(),
        options,
        &mut reporter,
    )?;
    let elapsed = started.elapsed().as_secs_f64().max(0.001);
    if json {
        print_json(&serde_json::json!({
            "profile": args.profile.as_str(),
            "backend": args.backend.as_str(),
            "windows_per_sec": final_run.scanned_windows as f64 / elapsed,
            "digits_per_sec": final_run.scanned_windows as f64 / elapsed,
            "best_score": final_run.best_score,
        }))?;
    } else {
        println!(
            "benchmark profile={} backend={}",
            args.profile.as_str(),
            args.backend.as_str()
        );
        println!(
            "windows/sec: {:.0}",
            final_run.scanned_windows as f64 / elapsed
        );
        println!(
            "digits/sec: {:.0}",
            final_run.scanned_windows as f64 / elapsed
        );
        println!("best score: {:.2}%", final_run.best_score * 100.0);
    }
    Ok(())
}

struct SilentReporter;

impl SearchReporter for SilentReporter {
    fn on_update(&mut self, _snapshot: &SearchSnapshot) -> Result<()> {
        Ok(())
    }

    fn on_new_best(&mut self, _snapshot: &SearchSnapshot, _event: &BestEventRecord) -> Result<()> {
        Ok(())
    }

    fn on_finish(&mut self, _snapshot: &SearchSnapshot, _reason: FinishReason) -> Result<()> {
        Ok(())
    }
}

fn run_stress_test(args: StressTestArgs, json: bool, color: bool) -> Result<()> {
    confirm_max_stress(&args, json)?;
    eprintln!(
        "warning: stress-test mode is intended to heavily load the machine while searching pi."
    );
    let mut start = StartArgs {
        template: Some(args.template.unwrap_or_else(|| "arch".to_string())),
        file: None,
        name: format!("stress-test-{}", chrono::Utc::now().timestamp()),
        mode: Some(SizeMode::Twelve),
        width: None,
        height: None,
        match_mode: MatchModeArg::Emergence,
        canvas_width: Some(24),
        canvas_height: Some(24),
        empty: None,
        filled: None,
        start_offset: None,
        max_offset: None,
        limit: args
            .stress_duration
            .map(|seconds| seconds.saturating_mul(10_000)),
        threshold: 5,
        invert: false,
        workers: None,
        cpu_workers: None,
        checkpoint_every: if args.stress_no_checkpoint {
            u64::MAX / 2
        } else {
            3
        },
        top: 10,
        no_tui: json,
        pi_file: None,
        allow_decimal_prefix: false,
        infinite: true,
        keep_going_after_perfect: true,
        profile: args.profile,
        backend: args.backend,
        generator_backend: GeneratorBackendChoice::Auto,
        gpu: args.gpu,
        gpu_device: None,
        cpu_utilization: Some(100),
        gpu_utilization: Some(100),
        chunk_size: None,
        queue_depth: None,
        memory_limit_mb: None,
        ui_refresh_ms: None,
        thermal_mode: ThermalMode::Aggressive,
        background_yield_ms: Some(0),
        pause_when_on_battery: false,
        max_fps: None,
        benchmark: false,
        stress_test: true,
        show_metrics: true,
        yes: true,
        force: true,
    };
    if matches!(args.stress_target, cli::StressTarget::Cpu) {
        start.gpu = GpuMode::Off;
        start.backend = SearchBackendChoice::Cpu;
    }
    start_or_hunt(start, true, json, color)
}

fn print_json<T: Serialize>(value: &T) -> Result<()> {
    println!("{}", serde_json::to_string_pretty(value)?);
    Ok(())
}
