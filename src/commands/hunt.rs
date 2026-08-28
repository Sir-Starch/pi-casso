//! `start`, `hunt` and `resume`: creating a run and driving it to a reporter.

use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, Result, bail};

use crate::art::{self, ArtMapping, Bitmap};
use crate::benchmark_contract::{
    AUTO_MIN_WORK_WINDOWS, BackendPreflightRequest, BackendResolution, CudaPreflight,
    WgpuPreflight, cuda_preflight, resolve_backend_preflight,
};
use crate::capability::GpuCapability;
use crate::cli::{
    MatchModeArg, ResumeArgs, ResumeBooleanOverrides, SizeMode, StartArgs, StressTestArgs,
};
use crate::cli_output::PlainReporter;
use crate::commands::{CommandContext, CommandExit, print_json};
use crate::digits::{self, DigitSource, DigitSourceSpec, FileDigitSource};
use crate::performance::{
    GeneratorBackendChoice, GpuMode, PerformanceOverrides, PerformanceProfile, PerformanceSettings,
    PerformanceSnapshot, SearchBackendChoice, ThermalMode,
};
use crate::pi;
use crate::search::{
    BackendSelectionError, MatchMode, SearchOptions, SnapshotIncompatible, run_search,
};
use crate::storage::{self, CheckpointProgress, NewRun, RunStatus, Storage};
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

    let mapping = ArtMapping::from_cli(args.empty.as_deref(), args.filled.as_deref());
    let default_size = if infinite { 12 } else { 16 };
    let (width, height) = resolve_dimensions(args.mode, args.width, args.height, default_size)?;
    let match_mode = match_mode_arg(args.match_mode);
    let mut performance = performance_from_start(&args, match_mode);
    if let Err(error) = normalize_start_backend_pair(&mut performance) {
        return preparation_error(context, error);
    }
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
    let requested_limit = if infinite && !args.stress_test {
        None
    } else {
        args.limit
    };
    let count_limit = SearchOptions::intersect_count_bounds(args.work_windows, requested_limit);
    let preflight = match current_host_preflight(&performance, count_limit.unwrap_or_default()) {
        Ok(preflight) => preflight,
        Err(error) => return preparation_error(context, error),
    };
    let mut execution_performance = performance.clone();
    apply_resolved_backend(&mut execution_performance, preflight.resolution.resolved);
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
    let mut snapshot = PerformanceSnapshot::from_settings(
        performance.clone(),
        args.start_offset,
        args.work_windows,
        args.limit,
    );
    snapshot.max_offset = if infinite { None } else { args.max_offset };
    snapshot.keep_going_after_perfect = args.keep_going_after_perfect;
    snapshot.no_tui = args.no_tui;
    record_current_host_capability(&mut snapshot, &preflight)?;
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
        "work_windows": args.work_windows,
        "performance_snapshot": snapshot.encode_value(),
    });
    let mut storage = Storage::open_default()?;
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
        work_windows: args.work_windows,
        limit: count_limit,
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
        performance: execution_performance,
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
        args.profile.unwrap_or(PerformanceProfile::Balanced),
        args.backend
            .unwrap_or(crate::performance::SearchBackendChoice::Auto),
        args.generator_backend
            .unwrap_or(crate::performance::GeneratorBackendChoice::Auto),
        args.gpu.unwrap_or(crate::performance::GpuMode::Auto),
        args.gpu_device.clone(),
        args.thermal_mode
            .unwrap_or(crate::performance::ThermalMode::Normal),
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
            checkpoint_every_secs: Some(args.checkpoint_every.unwrap_or(5)),
            background_yield_ms: args.background_yield_ms,
            max_fps: args.max_fps,
            pause_when_on_battery: args.pause_when_on_battery,
        },
    )
}

#[derive(Clone, Debug)]
struct CurrentHostPreflight {
    resolution: BackendResolution,
    capability: Option<GpuCapability>,
}

fn normalize_start_backend_pair(settings: &mut PerformanceSettings) -> Result<()> {
    let normalized = match (settings.backend, settings.gpu) {
        (SearchBackendChoice::Cpu, GpuMode::Off | GpuMode::Auto)
        | (SearchBackendChoice::Auto, GpuMode::Off) => (SearchBackendChoice::Cpu, GpuMode::Off),
        (SearchBackendChoice::Gpu, GpuMode::On | GpuMode::Auto)
        | (SearchBackendChoice::Auto, GpuMode::On) => (SearchBackendChoice::Gpu, GpuMode::On),
        (SearchBackendChoice::Cuda, GpuMode::On | GpuMode::Auto) => {
            (SearchBackendChoice::Cuda, GpuMode::On)
        }
        (SearchBackendChoice::Auto, GpuMode::Auto) => (SearchBackendChoice::Auto, GpuMode::Auto),
        (backend, _) => {
            return Err(selection_error(
                "backend and gpu selections are inconsistent",
                backend,
            ));
        }
    };
    settings.backend = normalized.0;
    settings.gpu = normalized.1;
    if normalized == (SearchBackendChoice::Cpu, GpuMode::Off) {
        if settings
            .gpu_device
            .as_deref()
            .is_some_and(|device| device != "auto")
        {
            return Err(selection_error(
                "gpu_device cannot select an adapter for the CPU backend",
                settings.backend,
            ));
        }
        settings.gpu_device = Some("auto".to_string());
    }
    Ok(())
}

fn merge_backend_pair(
    settings: &mut PerformanceSettings,
    overrides: &ResumeOverrides,
) -> Result<()> {
    let pair = match (overrides.backend, overrides.gpu) {
        (Some(backend), Some(gpu)) => (backend, gpu),
        (Some(SearchBackendChoice::Cpu), None) => (SearchBackendChoice::Cpu, GpuMode::Off),
        (Some(SearchBackendChoice::Gpu), None) => (SearchBackendChoice::Gpu, GpuMode::On),
        (Some(SearchBackendChoice::Cuda), None) => (SearchBackendChoice::Cuda, GpuMode::On),
        (Some(SearchBackendChoice::Auto), None) => (SearchBackendChoice::Auto, GpuMode::Auto),
        (None, Some(GpuMode::Off)) => (SearchBackendChoice::Cpu, GpuMode::Off),
        (None, Some(GpuMode::On)) => (SearchBackendChoice::Gpu, GpuMode::On),
        (None, Some(GpuMode::Auto)) => (SearchBackendChoice::Auto, GpuMode::Auto),
        (None, None) => (settings.backend, settings.gpu),
    };
    if !matches!(
        pair,
        (SearchBackendChoice::Cpu, GpuMode::Off)
            | (SearchBackendChoice::Gpu, GpuMode::On)
            | (SearchBackendChoice::Cuda, GpuMode::On)
            | (SearchBackendChoice::Auto, GpuMode::Auto)
    ) {
        return Err(selection_error(
            "backend and gpu selections are inconsistent",
            pair.0,
        ));
    }
    settings.backend = pair.0;
    settings.gpu = pair.1;
    if pair == (SearchBackendChoice::Cpu, GpuMode::Off) {
        if overrides
            .gpu_device
            .as_deref()
            .is_some_and(|device| device != "auto")
        {
            return Err(selection_error(
                "gpu_device cannot select an adapter for the CPU backend",
                pair.0,
            ));
        }
        settings.gpu_device = Some("auto".to_string());
    } else if settings.gpu_device.is_none() {
        settings.gpu_device = Some("auto".to_string());
    }
    Ok(())
}

fn current_host_preflight(
    settings: &PerformanceSettings,
    effective_work_windows: u64,
) -> Result<CurrentHostPreflight> {
    let probes_cuda = matches!(
        (settings.backend, settings.gpu),
        (SearchBackendChoice::Cuda, GpuMode::On)
            | (SearchBackendChoice::Auto, GpuMode::Auto)
                if settings.backend == SearchBackendChoice::Cuda
                    || effective_work_windows >= AUTO_MIN_WORK_WINDOWS
    );
    let cuda_capability = probes_cuda.then(|| {
        #[cfg(feature = "cuda-native")]
        {
            crate::cuda::detect_capability()
        }
        #[cfg(not(feature = "cuda-native"))]
        {
            GpuCapability::cuda_unavailable("cuda_not_compiled", "not_attempted")
        }
    });
    let cuda = cuda_capability
        .as_ref()
        .map_or(CudaPreflight::NotProbed, cuda_preflight);
    let probes_wgpu = !matches!(cuda, CudaPreflight::Eligible)
        && matches!(
            (settings.backend, settings.gpu),
            (SearchBackendChoice::Gpu, GpuMode::On)
                | (SearchBackendChoice::Auto, GpuMode::Auto)
                    if settings.backend == SearchBackendChoice::Gpu
                        || effective_work_windows >= AUTO_MIN_WORK_WINDOWS
        );
    let wgpu_capability = probes_wgpu.then(|| {
        if test_forces_wgpu_unavailable() {
            GpuCapability::unavailable("pipeline_preflight_unavailable")
        } else {
            GpuCapability::detect_with_filter(
                settings
                    .gpu_device
                    .as_deref()
                    .filter(|device| *device != "auto"),
            )
        }
    });
    let resolution = resolve_backend_preflight(BackendPreflightRequest {
        backend: Some(settings.backend),
        gpu: Some(settings.gpu),
        effective_work_windows,
        cuda,
        wgpu: wgpu_capability
            .as_ref()
            .map_or(WgpuPreflight::NotProbed, wgpu_preflight),
    });
    if resolution.status != "ok" {
        return Err(anyhow::Error::new(BackendSelectionError {
            status: resolution.status,
            reason: resolution.reason,
            requested_backend: resolution.requested.to_string(),
        }));
    }
    let capability = match resolution.resolved {
        Some("cuda") => cuda_capability,
        Some("wgpu") => wgpu_capability,
        Some("cpu") | Some(_) | None => cuda_capability.or(wgpu_capability),
    };
    Ok(CurrentHostPreflight {
        resolution,
        capability,
    })
}

fn wgpu_preflight(capability: &GpuCapability) -> WgpuPreflight {
    if capability.capability_state == "preflight_ok" {
        WgpuPreflight::Eligible
    } else {
        WgpuPreflight::Unavailable(match capability.reason.as_str() {
            "adapter_unavailable" => "adapter_unavailable",
            _ => "pipeline_preflight_unavailable",
        })
    }
}

fn apply_resolved_backend(settings: &mut PerformanceSettings, resolved: Option<&str>) {
    match resolved {
        Some("cpu") => {
            settings.backend = SearchBackendChoice::Cpu;
            settings.gpu = GpuMode::Off;
            settings.gpu_device = Some("auto".to_string());
        }
        Some("wgpu") => {
            settings.backend = SearchBackendChoice::Gpu;
            settings.gpu = GpuMode::On;
        }
        Some("cuda") => {
            settings.backend = SearchBackendChoice::Cuda;
            settings.gpu = GpuMode::On;
        }
        Some(_) | None => {}
    }
}

fn record_current_host_capability(
    snapshot: &mut PerformanceSnapshot,
    preflight: &CurrentHostPreflight,
) -> Result<()> {
    let resolution = &preflight.resolution;
    snapshot.legacy_extra.insert(
        "current_host_resolution".to_string(),
        serde_json::json!({
            "status": resolution.status,
            "requested": resolution.requested,
            "resolved": resolution.resolved,
            "gpu_mode": resolution.gpu_mode,
            "fallback": resolution.fallback,
            "reason": resolution.reason,
            "backend_candidates": resolution.backend_candidates.iter().map(|candidate| {
                serde_json::json!({
                    "backend": candidate.backend,
                    "status": candidate.status,
                    "eligible": candidate.eligible,
                    "reason": candidate.reason,
                })
            }).collect::<Vec<_>>(),
        }),
    );
    if let Some(capability) = &preflight.capability {
        snapshot.legacy_extra.insert(
            "current_host_capability".to_string(),
            serde_json::to_value(capability)?,
        );
    }
    Ok(())
}

fn test_forces_wgpu_unavailable() -> bool {
    crate::gpu_ring::test_mode_enabled()
        && std::env::var("PI_CASSO_TEST_FORCE_CAPABILITY").as_deref() == Ok("wgpu-unavailable")
}

fn selection_error(reason: &str, backend: SearchBackendChoice) -> anyhow::Error {
    anyhow::Error::new(BackendSelectionError {
        status: "selection_error",
        reason: reason.to_string(),
        requested_backend: backend.as_str().to_string(),
    })
}

fn preparation_error(context: &CommandContext, error: anyhow::Error) -> Result<()> {
    if let Some(snapshot) = error.downcast_ref::<SnapshotIncompatible>() {
        if context.json {
            print_json(snapshot)?;
            return Err(anyhow::Error::new(CommandExit(2)));
        }
    }
    if let Some(selection) = error.downcast_ref::<BackendSelectionError>() {
        if context.json {
            print_json(selection)?;
            return Err(anyhow::Error::new(CommandExit(2)));
        }
    }
    Err(error)
}

#[derive(Clone, Debug, Default)]
pub(crate) struct ResumeOverrides {
    pub max_offset: Option<i64>,
    pub limit: Option<u64>,
    pub work_windows: Option<u64>,
    pub workers: Option<usize>,
    pub cpu_workers: Option<usize>,
    pub checkpoint_every: Option<u64>,
    pub profile: Option<PerformanceProfile>,
    pub backend: Option<SearchBackendChoice>,
    pub generator_backend: Option<GeneratorBackendChoice>,
    pub gpu: Option<GpuMode>,
    pub gpu_device: Option<String>,
    pub cpu_utilization: Option<u8>,
    pub gpu_utilization: Option<u8>,
    pub chunk_size: Option<usize>,
    pub queue_depth: Option<usize>,
    pub memory_limit_mb: Option<usize>,
    pub ui_refresh_ms: Option<u64>,
    pub thermal_mode: Option<ThermalMode>,
    pub background_yield_ms: Option<u64>,
    pub max_fps: Option<u32>,
    pub booleans: ResumeBooleanOverrides,
}

pub(crate) fn merge_resume_snapshot(
    mut snapshot: PerformanceSnapshot,
    overrides: &ResumeOverrides,
) -> Result<PerformanceSnapshot> {
    let persisted_backend = snapshot.settings.backend;
    let persisted_gpu = snapshot.settings.gpu;
    let persisted_device = snapshot.settings.gpu_device.clone();
    if let Some(profile) = overrides.profile {
        snapshot.settings.profile = profile;
    }
    if let Some(generator_backend) = overrides.generator_backend {
        snapshot.settings.generator_backend = generator_backend;
    }
    if let Some(gpu_device) = &overrides.gpu_device {
        snapshot.settings.gpu_device = Some(gpu_device.clone());
    }
    merge_backend_pair(&mut snapshot.settings, overrides)?;
    if (snapshot.settings.backend, snapshot.settings.gpu) != (persisted_backend, persisted_gpu)
        || snapshot.settings.gpu_device != persisted_device
    {
        snapshot.legacy_extra.insert(
            "historical_backend".to_string(),
            persisted_backend.as_str().into(),
        );
        snapshot
            .legacy_extra
            .insert("historical_gpu".to_string(), persisted_gpu.as_str().into());
        snapshot.legacy_extra.insert(
            "historical_gpu_device".to_string(),
            persisted_device
                .unwrap_or_else(|| "auto".to_string())
                .into(),
        );
    }

    let limits = &mut snapshot.settings.limits;
    if overrides.workers.is_some()
        && overrides.cpu_workers.is_some()
        && overrides.workers != overrides.cpu_workers
    {
        return Err(selection_error(
            "workers and cpu_workers must match",
            snapshot.settings.backend,
        ));
    }
    if let Some(value) = overrides.cpu_workers.or(overrides.workers) {
        limits.cpu_workers = value;
    }
    if let Some(value) = overrides.cpu_utilization {
        limits.cpu_utilization = value;
    }
    if let Some(value) = overrides.gpu_utilization {
        limits.gpu_utilization = Some(value);
    }
    if let Some(value) = overrides.chunk_size {
        limits.chunk_size = value;
    }
    if let Some(value) = overrides.queue_depth {
        limits.queue_depth = value;
    }
    if let Some(value) = overrides.memory_limit_mb {
        limits.memory_limit_mb = value;
    }
    if let Some(value) = overrides.ui_refresh_ms {
        limits.ui_refresh_ms = value;
    }
    if let Some(value) = overrides.checkpoint_every {
        limits.checkpoint_every_secs = value;
    }
    if let Some(value) = overrides.background_yield_ms {
        limits.background_yield_ms = value;
    }
    if let Some(value) = overrides.max_fps {
        limits.max_fps = value;
    }

    if let Some(value) = overrides.thermal_mode {
        snapshot.settings.thermal_mode = value;
    }
    if let Some(value) = overrides.max_offset {
        snapshot.max_offset = Some(u64::try_from(value).map_err(|_| {
            selection_error("max_offset must be nonnegative", snapshot.settings.backend)
        })?);
    }
    if let Some(value) = overrides.limit {
        snapshot.limit = Some(value);
    }
    if let Some(value) = overrides.work_windows {
        snapshot.work_windows = Some(value);
    }

    let booleans = overrides.booleans;
    if let Some(value) = booleans.keep_going_after_perfect {
        snapshot.keep_going_after_perfect = value;
    }
    if let Some(value) = booleans.no_tui {
        snapshot.no_tui = value;
    }
    if let Some(value) = booleans.show_metrics {
        snapshot.settings.show_metrics = value;
    }
    if let Some(value) = booleans.pause_when_on_battery {
        snapshot.settings.limits.pause_when_on_battery = value;
    }
    if let Some(value) = booleans.stress_test {
        snapshot.settings.stress_test = value;
    }
    if let Some(value) = booleans.stress_no_checkpoint {
        snapshot.legacy_extra.insert(
            "stress_no_checkpoint".to_string(),
            serde_json::Value::Bool(value),
        );
    }

    Ok(snapshot)
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
        args.profile.unwrap_or(PerformanceProfile::Balanced),
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

pub(crate) fn prepare_resume(
    args: &ResumeArgs,
    storage: &mut Storage,
) -> Result<crate::tui::PreparedResume> {
    let mut run = storage.resolve_run(&args.run)?;
    let params = serde_json::from_str::<serde_json::Value>(&run.params_json)
        .map_err(|error| anyhow::Error::new(SnapshotIncompatible::new(error.to_string(), None)))?;
    if !params.is_object() {
        return Err(anyhow::Error::new(SnapshotIncompatible::new(
            "persisted run parameters must be a JSON object",
            None,
        )));
    }
    let persisted_top = params
        .get("top")
        .and_then(serde_json::Value::as_u64)
        .and_then(|value| usize::try_from(value).ok());
    let snapshot = match params.get("performance_snapshot").cloned() {
        Some(value) => {
            let schema_version = value
                .get("schema_version")
                .and_then(serde_json::Value::as_u64);
            PerformanceSnapshot::decode_value(value).map_err(|error| {
                anyhow::Error::new(SnapshotIncompatible::new(error.to_string(), schema_version))
            })?
        }
        None => PerformanceSnapshot::from_settings(
            performance_from_resume(args, run.match_mode),
            Some(run.current_offset),
            None,
            None,
        ),
    };
    let overrides = ResumeOverrides {
        max_offset: args.max_offset,
        limit: args.limit,
        work_windows: args.work_windows,
        workers: args.workers,
        cpu_workers: args.cpu_workers,
        checkpoint_every: args.checkpoint_every,
        profile: args.profile,
        backend: args.backend,
        generator_backend: args.generator_backend,
        gpu: args.gpu,
        gpu_device: args.gpu_device.clone(),
        cpu_utilization: args.cpu_utilization,
        gpu_utilization: args.gpu_utilization,
        chunk_size: args.chunk_size,
        queue_depth: args.queue_depth,
        memory_limit_mb: args.memory_limit_mb,
        ui_refresh_ms: args.ui_refresh_ms,
        thermal_mode: args.thermal_mode,
        background_yield_ms: args.background_yield_ms,
        max_fps: args.max_fps,
        booleans: args.booleans(),
    };
    let mut snapshot = merge_resume_snapshot(snapshot, &overrides)?;
    snapshot.current_offset = Some(run.current_offset);
    let effective_work_windows =
        SearchOptions::intersect_count_bounds(snapshot.work_windows, snapshot.limit)
            .unwrap_or_default();
    let preflight = current_host_preflight(&snapshot.settings, effective_work_windows)?;
    record_current_host_capability(&mut snapshot, &preflight)?;
    let mut execution_performance = snapshot.settings.clone();
    apply_resolved_backend(&mut execution_performance, preflight.resolution.resolved);
    let options = SearchOptions {
        max_offset: snapshot.max_offset,
        work_windows: snapshot.work_windows,
        limit: SearchOptions::intersect_count_bounds(snapshot.work_windows, snapshot.limit),
        match_mode: run.match_mode,
        canvas_width: run.canvas_width as usize,
        canvas_height: run.canvas_height as usize,
        threshold: run.threshold,
        invert: run.invert_enabled,
        workers: Some(snapshot.settings.limits.cpu_workers),
        checkpoint_every: Duration::from_secs(snapshot.settings.limits.checkpoint_every_secs),
        top_n: args
            .top
            .or(persisted_top)
            .unwrap_or_else(|| run.top_matches.len().max(10)),
        keep_going_after_perfect: snapshot.keep_going_after_perfect,
        chunk_windows: snapshot.settings.limits.chunk_size,
        performance: execution_performance,
    };
    let progress = CheckpointProgress {
        current_offset: run.current_offset,
        scanned_windows: run.scanned_windows,
        best_score: run.best_score,
        best_offset: run.best_offset,
        stop_reason: run.checkpoint_state().progress.stop_reason,
        checkpoint_sequence: run.checkpoint_state().progress.checkpoint_sequence,
    };
    let checkpoint = storage
        .checkpoint_with_snapshot(&run.id, &progress, &snapshot)
        .map_err(|failure| failure.cause)?;
    run.params_json = checkpoint.params_json;
    Ok(crate::tui::PreparedResume {
        run,
        snapshot,
        options,
        capability: preflight.resolution,
    })
}

pub(crate) fn prepare_resume_selected(
    run: &storage::RunRecord,
    storage: &mut Storage,
) -> Result<crate::tui::PreparedResume> {
    let args = ResumeArgs {
        run: run.id.clone(),
        max_offset: None,
        limit: None,
        work_windows: None,
        workers: None,
        cpu_workers: None,
        checkpoint_every: None,
        top: None,
        no_tui: false,
        tui: false,
        keep_going_after_perfect: false,
        stop_after_perfect: false,
        profile: None,
        backend: None,
        generator_backend: None,
        gpu: None,
        gpu_device: None,
        cpu_utilization: None,
        gpu_utilization: None,
        chunk_size: None,
        queue_depth: None,
        memory_limit_mb: None,
        ui_refresh_ms: None,
        thermal_mode: None,
        background_yield_ms: None,
        pause_when_on_battery: false,
        allow_on_battery: false,
        max_fps: None,
        stress_test: false,
        no_stress_test: false,
        stress_no_checkpoint: false,
        checkpoint: false,
        show_metrics: false,
        no_show_metrics: false,
        yes: false,
        force: false,
    };
    prepare_resume(&args, storage)
}

pub fn resume(args: ResumeArgs, context: &CommandContext) -> Result<()> {
    if args.launches_tui() && context.json {
        bail!("resume --tui cannot be combined with --json");
    }
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
    let prepared = match prepare_resume(&args, &mut storage) {
        Ok(prepared) => prepared,
        Err(error) => return preparation_error(context, error),
    };
    if args.launches_tui() {
        let launch = crate::tui::TuiLaunch {
            prepared_resume: Some(prepared),
        };
        let tui_context = CommandContext {
            config: context.config.clone(),
            theme: context.theme,
            json: context.json,
        };
        return crate::tui::run_with_launch(tui_context, launch);
    }
    let source = prepared.run.source.open()?;
    run_search_with_reporter(
        &mut storage,
        prepared.run,
        source.as_ref(),
        prepared.options,
        plain,
        context,
    )
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::ResumeBooleanOverrides;
    use crate::performance::{GeneratorBackendChoice, GpuMode, SearchBackendChoice, ThermalMode};

    fn persisted_snapshot(
        backend: SearchBackendChoice,
        gpu: GpuMode,
    ) -> crate::performance::PerformanceSnapshot {
        crate::performance::PerformanceSnapshot::from_settings(
            PerformanceSettings::from_profile(
                PerformanceProfile::Eco,
                backend,
                GeneratorBackendChoice::Cpu,
                gpu,
                Some("persisted-adapter".to_string()),
                ThermalMode::Quiet,
                false,
                true,
                MatchMode::Emergence,
                PerformanceOverrides {
                    cpu_workers: Some(2),
                    gpu_utilization: Some(0),
                    ..PerformanceOverrides::default()
                },
            ),
            Some(41),
            Some(17),
            Some(19),
        )
    }

    #[test]
    fn resume_precedence_persisted_gpu_cli_backend_cpu_normalizes_pair() {
        // Given: a strict persisted wgpu run and a CLI-only CPU backend override.
        let snapshot = persisted_snapshot(SearchBackendChoice::Gpu, GpuMode::On);
        let overrides = ResumeOverrides {
            backend: Some(SearchBackendChoice::Cpu),
            ..ResumeOverrides::default()
        };

        // When: the shared resume merge applies the coupled backend rule.
        let merged = merge_resume_snapshot(snapshot, &overrides).expect("merge succeeds");

        // Then: CPU forces GPU off and the historical pair remains reportable.
        assert_eq!(merged.settings.backend, SearchBackendChoice::Cpu);
        assert_eq!(merged.settings.gpu, GpuMode::Off);
        assert_eq!(merged.settings.gpu_device.as_deref(), Some("auto"));
        assert_eq!(merged.legacy_extra["historical_backend"], "gpu");
        assert_eq!(merged.legacy_extra["historical_gpu"], "on");
    }

    #[test]
    fn resume_precedence_persisted_backend_cpu_cli_gpu_on_selects_wgpu() {
        // Given: a persisted CPU run and a CLI-only GPU-on override.
        let snapshot = persisted_snapshot(SearchBackendChoice::Cpu, GpuMode::Off);
        let overrides = ResumeOverrides {
            gpu: Some(GpuMode::On),
            ..ResumeOverrides::default()
        };

        // When: the shared resume merge applies the coupled GPU rule.
        let merged = merge_resume_snapshot(snapshot, &overrides).expect("merge succeeds");

        // Then: GPU-on selects strict wgpu and leaves unrelated persisted limits intact.
        assert_eq!(merged.settings.backend, SearchBackendChoice::Gpu);
        assert_eq!(merged.settings.gpu, GpuMode::On);
        assert_eq!(merged.settings.limits.cpu_workers, 2);
        assert_eq!(merged.settings.limits.gpu_utilization, Some(0));
    }

    #[test]
    fn resume_rejects_contradictory_persisted_backend_gpu_pair() {
        // Given: persisted settings that could never pass the public CLI matrix.
        let snapshot = persisted_snapshot(SearchBackendChoice::Cpu, GpuMode::On);

        // When: no CLI override repairs or replaces that pair.
        let result = merge_resume_snapshot(snapshot, &ResumeOverrides::default());

        // Then: validation fails before any source-bearing operation is available.
        assert!(result.is_err());
    }

    #[test]
    fn resume_boolean_overrides_when_explicit_false_replace_persisted_true() {
        // Given: persisted true values and explicit negative CLI forms.
        let mut snapshot = persisted_snapshot(SearchBackendChoice::Cpu, GpuMode::Off);
        snapshot.keep_going_after_perfect = true;
        snapshot.no_tui = true;
        snapshot.settings.show_metrics = true;
        snapshot.settings.stress_test = true;
        snapshot.settings.limits.pause_when_on_battery = true;
        snapshot.legacy_extra.insert(
            "stress_no_checkpoint".to_string(),
            serde_json::Value::Bool(true),
        );
        let overrides = ResumeOverrides {
            booleans: ResumeBooleanOverrides {
                keep_going_after_perfect: Some(false),
                no_tui: Some(false),
                show_metrics: Some(false),
                pause_when_on_battery: Some(false),
                stress_test: Some(false),
                stress_no_checkpoint: Some(false),
            },
            ..ResumeOverrides::default()
        };

        // When: the shared merge applies explicit false values.
        let merged = merge_resume_snapshot(snapshot, &overrides).expect("merge succeeds");

        // Then: false remains a value rather than being mistaken for omission.
        assert!(!merged.keep_going_after_perfect);
        assert!(!merged.no_tui);
        assert!(!merged.settings.show_metrics);
        assert!(!merged.settings.stress_test);
        assert!(!merged.settings.limits.pause_when_on_battery);
        assert_eq!(merged.legacy_extra["stress_no_checkpoint"], false);
    }
}
