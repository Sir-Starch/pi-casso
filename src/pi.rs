use std::fs::File;
use std::io::Read;
use std::path::PathBuf;
use std::sync::{
    Arc, Mutex, OnceLock,
    atomic::{AtomicBool, Ordering},
};

use anyhow::{Context, Result, anyhow, bail};
use num_bigint::BigInt;
use num_traits::{One, ToPrimitive, Zero};
#[cfg(not(target_env = "msvc"))]
use rug::{Integer, ops::Pow};

use crate::digits::DigitRead;

mod cache_publication;
mod generator_backend;
mod generator_discovery;
mod producer;
mod y_cruncher;

pub(crate) use generator_backend::{GeneratorSelection, UnavailableGenerator, resolve_generator};
pub use producer::CachedGrowingPiSource;
pub(crate) use producer::GENERATOR_FIXED_BYTES;
#[cfg(test)]
use producer::GenerationBackend;
pub(crate) use producer::{GenerationBudget, GenerationPermit};
pub use producer::{GenerationDemand, GenerationMetrics, GenerationPlan};

pub const DEFAULT_GENERATE_CHUNK: usize = 10_000;

#[cfg(not(target_env = "msvc"))]
const CHUDNOVSKY_DIGITS_PER_TERM: f64 = 14.181_647_462_725_477;
#[cfg(not(target_env = "msvc"))]
const CHUDNOVSKY_C3_OVER_24: u64 = 10_939_058_860_032_000;
#[cfg(not(target_env = "msvc"))]
const CHUDNOVSKY_PARALLEL_THRESHOLD: u64 = 32;
static PI_GENERATION_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

#[derive(Clone, Debug)]
pub struct PiCache {
    path: PathBuf,
}

#[derive(Clone, Debug)]
pub struct PiCacheInfo {
    pub path: PathBuf,
    pub digits: u64,
    pub bytes: u64,
    pub published_digits: u64,
    pub raw_file_size: u64,
    pub published_prefix_sha256: String,
    pub valid_ascii: bool,
    pub sidecar_status: String,
}

impl PiCache {
    pub fn default() -> Result<Self> {
        Ok(Self {
            path: cache_path()?,
        })
    }

    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }

    pub fn path(&self) -> &PathBuf {
        &self.path
    }

    pub fn ensure_parent(&self) -> Result<()> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("failed to create {}", parent.display()))?;
        }
        Ok(())
    }

    pub fn info(&self) -> Result<PiCacheInfo> {
        self.ensure_parent()?;
        let snapshot = cache_publication::info(&self.path)?;
        Ok(PiCacheInfo {
            path: self.path.clone(),
            digits: snapshot.digits,
            bytes: snapshot.raw_file_size,
            published_digits: snapshot.published_digits,
            raw_file_size: snapshot.raw_file_size,
            published_prefix_sha256: snapshot.published_prefix_sha256,
            valid_ascii: snapshot.valid_ascii,
            sidecar_status: snapshot.sidecar_status.to_owned(),
        })
    }

    pub fn validate_reset_lock(&self) -> Result<()> {
        cache_publication::validate_reset_lock(&self.path)
    }

    pub fn digit_count(&self) -> Result<u64> {
        Ok(self.info()?.digits)
    }

    pub(crate) fn published_digit_count(&self) -> Result<u64> {
        self.ensure_parent()?;
        cache_publication::published_digit_count(&self.path)
    }

    pub fn append_digits(&self, digits: &[u8]) -> Result<()> {
        self.ensure_parent()?;
        cache_publication::append_digits(&self.path, digits)
    }

    pub fn append_from_validated_source(&self, source: &std::path::Path) -> Result<u64> {
        self.ensure_parent()?;
        cache_publication::append_from_validated_source(&self.path, source)
    }

    pub fn replace_from_validated_source(&self, source: &std::path::Path) -> Result<u64> {
        self.ensure_parent()?;
        cache_publication::replace_from_validated_source(&self.path, source)
    }

    pub fn repair_publication(&self) -> Result<()> {
        self.ensure_parent()?;
        cache_publication::repair_publication(&self.path)
    }

    fn with_publication_writer<T>(&self, operation: impl FnOnce() -> Result<T>) -> Result<T> {
        cache_publication::lock::with_exclusive(&self.path, operation)
    }

    pub fn read_range(&self, offset: u64, len: usize) -> Result<Vec<u8>> {
        Ok(self.read_range_timed(offset, len)?.digits)
    }

    pub fn read_range_timed(&self, offset: u64, len: usize) -> Result<DigitRead> {
        cache_publication::read_range_timed(&self.path, offset, len)
    }

    pub fn validate(&self) -> Result<()> {
        let mut file = match File::open(&self.path) {
            Ok(file) => file,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(err) => {
                return Err(err).with_context(|| format!("failed to open {}", self.path.display()));
            }
        };
        let mut offset = 0_u64;
        let mut buf = [0_u8; 64 * 1024];
        loop {
            let read = file.read(&mut buf)?;
            if read == 0 {
                break;
            }
            for (idx, byte) in buf[..read].iter().enumerate() {
                if !byte.is_ascii_digit() {
                    bail!(
                        "pi cache contains invalid byte 0x{byte:02x} at byte offset {}",
                        offset + idx as u64
                    );
                }
            }
            offset += read as u64;
        }
        Ok(())
    }
}

#[derive(Clone, Debug)]
pub struct PiGenerator {
    state: SpigotState,
}

impl PiGenerator {
    pub fn new() -> Self {
        Self {
            state: SpigotState::default(),
        }
    }

    pub fn skip(&mut self, digits: u64, stop: &AtomicBool) -> Result<()> {
        for _ in 0..digits {
            if stop.load(Ordering::SeqCst) {
                break;
            }
            self.next_digit()?;
        }
        Ok(())
    }

    pub fn generate(&mut self, digits: usize, stop: &AtomicBool) -> Result<Vec<u8>> {
        let mut out = Vec::with_capacity(digits);
        while out.len() < digits && !stop.load(Ordering::SeqCst) {
            out.push(self.next_digit()?);
        }
        Ok(out)
    }

    fn next_digit(&mut self) -> Result<u8> {
        self.state.next_digit()
    }
}

impl Default for PiGenerator {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Clone, Debug)]
struct SpigotState {
    q: BigInt,
    r: BigInt,
    t: BigInt,
    k: BigInt,
    n: BigInt,
    l: BigInt,
}

impl Default for SpigotState {
    fn default() -> Self {
        Self {
            q: BigInt::one(),
            r: BigInt::zero(),
            t: BigInt::one(),
            k: BigInt::one(),
            n: BigInt::from(3_u8),
            l: BigInt::from(3_u8),
        }
    }
}

impl SpigotState {
    fn next_digit(&mut self) -> Result<u8> {
        loop {
            if (&self.q * 4 + &self.r - &self.t) < (&self.n * &self.t) {
                let digit = self
                    .n
                    .to_u8()
                    .ok_or_else(|| anyhow!("pi generator produced an out-of-range digit"))?;
                let old_q = self.q.clone();
                let old_r = self.r.clone();
                let old_t = self.t.clone();
                let old_n = self.n.clone();
                self.q = old_q.clone() * 10;
                self.r = (old_r.clone() - &old_n * &old_t) * 10;
                self.t = old_t.clone();
                self.n = ((old_q * 3 + old_r) * 10 / old_t) - old_n * 10;
                return Ok(digit);
            }

            let old_q = self.q.clone();
            let old_r = self.r.clone();
            let old_t = self.t.clone();
            let old_k = self.k.clone();
            let old_l = self.l.clone();
            self.q = &old_q * &old_k;
            self.r = (&old_q * 2 + &old_r) * &old_l;
            self.t = old_t.clone() * &old_l;
            self.k = &old_k + 1;
            self.n = (&old_q * (&old_k * 7 + 2) + old_r * &old_l) / (&old_t * &old_l);
            self.l = old_l + 2;
        }
    }
}

pub(crate) fn generate_with_selection(
    cache: &PiCache,
    digits: u64,
    selection: &GeneratorSelection,
    workers: usize,
) -> Result<u64> {
    match selection.selected_variant {
        Some(generator_backend::GeneratorVariant::Chudnovsky) => {
            cache.with_publication_writer(|| {
                let _guard = generation_guard()?;
                generate_into_cache_fast(cache, digits, Arc::new(AtomicBool::new(false)))
            })
        }
        Some(generator_backend::GeneratorVariant::Spigot) => cache.with_publication_writer(|| {
            let _guard = generation_guard()?;
            generate_into_cache_spigot(cache, digits, Arc::new(AtomicBool::new(false)))
        }),
        Some(generator_backend::GeneratorVariant::YCruncher) => {
            let executable = selection
                .executable
                .as_ref()
                .ok_or_else(|| anyhow!(selection.reason.clone()))?;
            let target = cache
                .digit_count()?
                .checked_add(digits)
                .ok_or_else(|| anyhow!("requested pi cache size overflowed"))?;
            y_cruncher::generate_to_target(cache, target, executable, workers)
                .map(|generation| generation.generated_digits)
                .map_err(|failure| anyhow!(failure.as_str()))
        }
        None => Err(anyhow!(selection.reason.clone())),
    }
}

fn generation_guard() -> Result<std::sync::MutexGuard<'static, ()>> {
    PI_GENERATION_LOCK
        .get_or_init(|| Mutex::new(()))
        .lock()
        .map_err(|_| anyhow!("pi generation lock was poisoned"))
}

pub(super) fn generation_guard_with_stop(
    stop: &AtomicBool,
) -> Result<std::sync::MutexGuard<'static, ()>> {
    loop {
        if stop.load(Ordering::Acquire) {
            return Err(anyhow!("pi generation was cancelled"));
        }

        match PI_GENERATION_LOCK.get_or_init(|| Mutex::new(())).try_lock() {
            Ok(guard) => return Ok(guard),
            Err(std::sync::TryLockError::Poisoned(_)) => {
                return Err(anyhow!("pi generation lock was poisoned"));
            }
            Err(std::sync::TryLockError::WouldBlock) => {
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
        }
    }
}

fn generate_into_cache_spigot(cache: &PiCache, digits: u64, stop: Arc<AtomicBool>) -> Result<u64> {
    let existing = cache.digit_count()?;
    let mut generator = PiGenerator::new();
    generator.skip(existing, &stop)?;
    let mut generated = 0_u64;
    while generated < digits && !stop.load(Ordering::SeqCst) {
        let requested = ((digits - generated) as usize).min(DEFAULT_GENERATE_CHUNK);
        let chunk = generator.generate(requested, &stop)?;
        if chunk.is_empty() {
            break;
        }
        cache.append_digits(&chunk)?;
        generated += chunk.len() as u64;
    }
    Ok(generated)
}

fn generate_into_cache_fast(cache: &PiCache, digits: u64, stop: Arc<AtomicBool>) -> Result<u64> {
    let existing = cache.digit_count()?;
    if digits == 0 || stop.load(Ordering::SeqCst) {
        return Ok(0);
    }
    let target_digits = existing
        .checked_add(digits)
        .ok_or_else(|| anyhow!("requested pi cache size overflowed"))?;
    if target_digits > u32::MAX as u64 {
        bail!(
            "fast pi generator supports up to {} digits per batch",
            u32::MAX
        );
    }
    let prefix = chudnovsky_pi_digits(target_digits as usize)?;
    if stop.load(Ordering::SeqCst) {
        return Ok(0);
    }
    let start = existing as usize;
    cache.append_digits(&prefix[start..])?;
    Ok(prefix.len().saturating_sub(start) as u64)
}

#[cfg(not(target_env = "msvc"))]
pub fn chudnovsky_pi_digits(digits: usize) -> Result<Vec<u8>> {
    chudnovsky_pi_digits_with_parallelism(digits, true)
}

#[cfg(not(target_env = "msvc"))]
pub(crate) fn chudnovsky_pi_digits_sequential(digits: usize) -> Result<Vec<u8>> {
    chudnovsky_pi_digits_with_parallelism(digits, false)
}

#[cfg(not(target_env = "msvc"))]
fn chudnovsky_pi_digits_with_parallelism(digits: usize, parallel: bool) -> Result<Vec<u8>> {
    if digits == 0 {
        return Ok(Vec::new());
    }
    let guard_digits = 8usize;
    let scale_digits = digits + guard_digits;
    let terms = ((scale_digits as f64 / CHUDNOVSKY_DIGITS_PER_TERM).ceil() as u64 + 2).max(2);
    let (_p, q, t) = chudnovsky_bs_gmp(0, terms, parallel);
    if t == 0 {
        bail!("pi generator produced a zero Chudnovsky sum");
    }
    let one = Integer::from(10).pow(scale_digits as u32);
    let sqrt_input = Integer::from(10005) * &one * &one;
    let sqrt = sqrt_input.sqrt();
    let pi_scaled = (q * Integer::from(426_880) * sqrt) / t.abs();
    let trimmed = pi_scaled / Integer::from(10).pow(guard_digits as u32);
    let mut text = trimmed.to_string();
    if text.len() < digits {
        text.push_str(&"0".repeat(digits - text.len()));
    }
    text.truncate(digits);
    text.into_bytes()
        .into_iter()
        .map(|byte| {
            if byte.is_ascii_digit() {
                Ok(byte - b'0')
            } else {
                bail!("pi generator produced a non-digit byte 0x{byte:02x}")
            }
        })
        .collect()
}

#[cfg(target_env = "msvc")]
pub fn chudnovsky_pi_digits(_digits: usize) -> Result<Vec<u8>> {
    bail!(
        "Fast built-in CPU generation (Chudnovsky) is not supported on Windows MSVC. Please use the y-cruncher backend or the unbounded (spigot) mode."
    );
}

#[cfg(target_env = "msvc")]
pub(crate) fn chudnovsky_pi_digits_sequential(digits: usize) -> Result<Vec<u8>> {
    chudnovsky_pi_digits(digits)
}

#[cfg(not(target_env = "msvc"))]
fn chudnovsky_bs_gmp(a: u64, b: u64, parallel: bool) -> (Integer, Integer, Integer) {
    if b - a == 1 {
        if a == 0 {
            return (
                Integer::from(1),
                Integer::from(1),
                Integer::from(13_591_409),
            );
        }
        let p = Integer::from(6 * a - 5) * Integer::from(2 * a - 1) * Integer::from(6 * a - 1);
        let q = Integer::from(a)
            * Integer::from(a)
            * Integer::from(a)
            * Integer::from(CHUDNOVSKY_C3_OVER_24);
        let mut t = Integer::from(&p) * Integer::from(13_591_409_u64 + 545_140_134_u64 * a);
        if a % 2 == 1 {
            t = -t;
        }
        return (p, q, t);
    }

    let mid = (a + b) / 2;
    let (left, right) = if parallel && b - a >= CHUDNOVSKY_PARALLEL_THRESHOLD {
        rayon::join(
            || chudnovsky_bs_gmp(a, mid, true),
            || chudnovsky_bs_gmp(mid, b, true),
        )
    } else {
        (
            chudnovsky_bs_gmp(a, mid, parallel),
            chudnovsky_bs_gmp(mid, b, parallel),
        )
    };
    let (p1, q1, t1) = left;
    let (p2, q2, t2) = right;
    let p = Integer::from(&p1 * &p2);
    let q = Integer::from(&q1 * &q2);
    let t = (t1 * &q2) + (p1 * t2);
    (p, q, t)
}

pub fn cache_path() -> Result<PathBuf> {
    Ok(crate::storage::app_data_dir()?.join("pi-cache.txt"))
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::sync::atomic::AtomicBool;

    use tempfile::tempdir;

    use super::*;

    #[test]
    fn spigot_generates_pi_prefix() {
        let stop = AtomicBool::new(false);
        let mut generator = PiGenerator::new();
        let digits = generator.generate(20, &stop).unwrap();
        assert_eq!(
            digits,
            vec![3, 1, 4, 1, 5, 9, 2, 6, 5, 3, 5, 8, 9, 7, 9, 3, 2, 3, 8, 4]
        );
    }

    #[test]
    #[cfg(not(target_env = "msvc"))]
    fn chudnovsky_generates_pi_prefix() {
        let digits = chudnovsky_pi_digits(32).unwrap();
        assert_eq!(
            digits,
            vec![
                3, 1, 4, 1, 5, 9, 2, 6, 5, 3, 5, 8, 9, 7, 9, 3, 2, 3, 8, 4, 6, 2, 6, 4, 3, 3, 8, 3,
                2, 7, 9, 5
            ]
        );
    }

    #[test]
    #[cfg(not(target_env = "msvc"))]
    fn sequential_chudnovsky_matches_parallel_generation() {
        // Given: a target large enough to cross the parallel binary-split threshold.
        let target = 1_024;

        // When: the direct sequential and Rayon-capable generators compute the same prefix.
        let sequential = chudnovsky_pi_digits_sequential(target).unwrap();
        let parallel = chudnovsky_pi_digits(target).unwrap();

        // Then: scheduling does not change the generated digits.
        assert_eq!(sequential, parallel);
    }

    #[test]
    fn validated_continuation_rejects_mismatching_prefix_without_mutation() {
        let directory = tempdir().unwrap();
        let cache = PiCache::new(directory.path().join("pi-cache.txt"));
        cache.append_digits(&[3, 1, 4, 1, 5]).unwrap();
        let raw_before = fs::read(cache.path()).unwrap();
        let sidecar = directory.path().join("pi-cache.digits.json");
        let sidecar_before = fs::read(&sidecar).unwrap();
        let source = directory.path().join("continuation.txt");
        fs::write(&source, b"2718281828").unwrap();

        let error = cache
            .append_from_validated_source(&source)
            .expect_err("mismatching continuation must be rejected");

        assert!(error.to_string().contains("does not match"));
        assert_eq!(fs::read(cache.path()).unwrap(), raw_before);
        assert_eq!(fs::read(sidecar).unwrap(), sidecar_before);
    }
}

#[cfg(test)]
#[path = "pi/producer_tests.rs"]
mod producer_tests;
