//! `benchmark` and `stress-test`: measuring throughput, and deliberately
//! loading the machine while doing it.

use std::sync::{Arc, atomic::AtomicBool};
use std::time::Duration;

use anyhow::Result;

use crate::art;
use crate::cli::{self, BenchmarkArgs, MatchModeArg, SizeMode, StartArgs, StressTestArgs};
use crate::commands::hunt::{confirm_max_stress, start_or_hunt};
use crate::commands::{CommandContext, print_json};
use crate::digits::DigitSourceSpec;
use crate::performance::{
    GeneratorBackendChoice, GpuMode, PerformanceOverrides, PerformanceSettings,
    SearchBackendChoice, ThermalMode,
};
use crate::pi;
use crate::search::{
    FinishReason, MatchMode, SearchOptions, SearchReporter, SearchSnapshot, run_search,
};
use crate::storage::{BestEventRecord, NewRun, Storage};

pub fn benchmark(args: BenchmarkArgs, context: &CommandContext) -> Result<()> {
    let json = context.json;
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

pub fn stress_test(args: StressTestArgs, context: &CommandContext) -> Result<()> {
    let json = context.json;
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
    start_or_hunt(start, true, context)
}
