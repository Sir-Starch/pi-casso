//! `pi import`, `pi generate`, `pi cache-info`, including the y-cruncher
//! integration those commands can delegate to.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, atomic::AtomicBool};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};

use crate::cli::PiCommands;
use crate::digits::{DigitSource, FileDigitSource};
use crate::performance::GeneratorBackendChoice;
use crate::pi;

pub fn dispatch(command: PiCommands) -> Result<()> {
    match command {
        PiCommands::Import {
            file,
            allow_decimal_prefix,
        } => import(file, allow_decimal_prefix),
        PiCommands::CacheInfo | PiCommands::Info => cache_info(),
        PiCommands::Generate {
            digits,
            generator_backend,
            y_cruncher_path,
            workers,
        } => generate(digits, generator_backend, y_cruncher_path, workers),
    }
}

fn import(file: PathBuf, allow_decimal_prefix: bool) -> Result<()> {
    let source = FileDigitSource::new_with_options(file, allow_decimal_prefix);
    let cache = pi::PiCache::default()?;
    let dest = cache.path().clone();
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let imported = source.copy_digits_to(&dest)?;
    println!("imported {} digits into {}", imported, dest.display());
    Ok(())
}

fn cache_info() -> Result<()> {
    let cache = pi::PiCache::default()?;
    let info = cache.info()?;
    println!("cache path: {}", info.path.display());
    println!("generated/usable digits: {}", info.digits);
    println!("size on disk: {} bytes", info.bytes);
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
