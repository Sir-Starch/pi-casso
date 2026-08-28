//! `pi import`, `pi generate`, `pi cache-info`, including the y-cruncher
//! integration those commands can delegate to.

use std::path::PathBuf;

use anyhow::{Result, bail};

use crate::cli::PiCommands;
use crate::commands::{CommandContext, print_json};
use crate::digits::FileDigitSource;
use crate::performance::GeneratorBackendChoice;
use crate::pi;

pub fn dispatch(command: PiCommands, context: &CommandContext) -> Result<()> {
    match command {
        PiCommands::Import {
            file,
            allow_decimal_prefix,
        } => import(file, allow_decimal_prefix),
        PiCommands::CacheInfo | PiCommands::Info => cache_info(context),
        PiCommands::CacheRepair { force } => cache_repair(force),
        PiCommands::Generate {
            digits,
            generator_backend,
            y_cruncher_path,
            workers,
        } => generate(digits, generator_backend, y_cruncher_path, workers),
        PiCommands::Benchmark(args) => benchmark(args, context),
    }
}

fn benchmark(args: crate::cli::PiBenchmarkArgs, context: &CommandContext) -> Result<()> {
    let outcome = crate::pi_benchmark::run(args)?;
    if context.json {
        print_json(&outcome.report)?;
    } else {
        println!(
            "pi benchmark status={} backend={} variant={}",
            outcome.report.status, outcome.report.selected_backend, outcome.report.selected_variant
        );
    }
    if outcome.exit_code == 0 {
        Ok(())
    } else {
        Err(anyhow::anyhow!(crate::commands::CommandExit(
            outcome.exit_code
        )))
    }
}

fn import(file: PathBuf, allow_decimal_prefix: bool) -> Result<()> {
    let source = FileDigitSource::new_with_options(file, allow_decimal_prefix);
    let cache = pi::PiCache::default()?;
    let dest = cache.path().clone();
    let imported = source.replace_cache(&cache)?;
    println!("imported {} digits into {}", imported, dest.display());
    Ok(())
}

fn cache_info(context: &CommandContext) -> Result<()> {
    let cache = pi::PiCache::default()?;
    let info = cache.info()?;
    if context.json {
        return print_json(&serde_json::json!({
            "schema_version": 1,
            "path": info.path,
            "digits": info.digits,
            "published_digits": info.published_digits,
            "raw_file_size": info.raw_file_size,
            "published_prefix_sha256": info.published_prefix_sha256,
            "valid_ascii": info.valid_ascii,
            "sidecar_status": info.sidecar_status,
        }));
    }
    println!("cache path: {}", info.path.display());
    println!("sidecar schema: 1");
    println!("sidecar status: {}", info.sidecar_status);
    println!("generated/usable digits: {}", info.digits);
    println!("published digits: {}", info.published_digits);
    println!("size on disk: {} bytes", info.bytes);
    Ok(())
}

fn cache_repair(force: bool) -> Result<()> {
    if !force {
        bail!("cache repair requires --force");
    }
    let cache = pi::PiCache::default()?;
    cache.repair_publication()?;
    println!("repaired cache publication at {}", cache.path().display());
    Ok(())
}

fn generate(
    digits: u64,
    generator_backend: GeneratorBackendChoice,
    y_cruncher_path: Option<PathBuf>,
    workers: Option<usize>,
) -> Result<()> {
    let cache = pi::PiCache::default()?;
    let generated = generate_pi_cache(&cache, digits, generator_backend, y_cruncher_path, workers)?;
    let info = cache.info()?;
    println!(
        "generated {} digits into {} (total={})",
        generated,
        info.path.display(),
        info.digits
    );
    Ok(())
}

pub(crate) fn generate_pi_cache(
    cache: &pi::PiCache,
    digits: u64,
    backend: GeneratorBackendChoice,
    y_cruncher_path: Option<PathBuf>,
    workers: Option<usize>,
) -> Result<u64> {
    let mut selection = pi::resolve_generator(backend, y_cruncher_path.as_deref())?;
    if !selection.is_available() {
        bail!(selection.reason.clone());
    }
    let workers = workers
        .unwrap_or_else(|| std::thread::available_parallelism().map_or(1, std::num::NonZero::get));
    match pi::generate_with_selection(cache, digits, &selection, workers) {
        Ok(generated) => Ok(generated),
        Err(error) if matches!(backend, GeneratorBackendChoice::Auto) => {
            selection = selection.fallback_after_failure(&error.to_string())?;
            pi::generate_with_selection(cache, digits, &selection, workers)
        }
        Err(error) => Err(error),
    }
}
