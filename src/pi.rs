use std::fs::{File, OpenOptions};
use std::io::{BufWriter, Read, Seek, SeekFrom, Write};
use std::path::PathBuf;
use std::sync::{
    Arc, Mutex, OnceLock,
    atomic::{AtomicBool, AtomicU64, Ordering},
};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use num_bigint::BigInt;
use num_traits::{One, ToPrimitive, Zero};
#[cfg(not(target_env = "msvc"))]
use rug::{Integer, ops::Pow};

use crate::digits::{DigitSource, convert_ascii_digits};

pub const DEFAULT_GENERATE_CHUNK: usize = 10_000;
pub const ON_DEMAND_CACHE_LEAD: u64 = 10_000;

const CHUDNOVSKY_DIGITS_PER_TERM: f64 = 14.181_647_462_725_477;
const CHUDNOVSKY_C3_OVER_24: u64 = 10_939_058_860_032_000;
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
        let bytes = match std::fs::metadata(&self.path) {
            Ok(metadata) => metadata.len(),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => 0,
            Err(err) => {
                return Err(err).with_context(|| format!("failed to stat {}", self.path.display()));
            }
        };
        Ok(PiCacheInfo {
            path: self.path.clone(),
            digits: bytes,
            bytes,
        })
    }

    pub fn digit_count(&self) -> Result<u64> {
        Ok(self.info()?.digits)
    }

    pub fn append_digits(&self, digits: &[u8]) -> Result<()> {
        if digits.iter().any(|digit| *digit > 9) {
            bail!("pi cache append received a non-digit value");
        }
        self.ensure_parent()?;
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
            .with_context(|| format!("failed to open pi cache {}", self.path.display()))?;
        let mut writer = BufWriter::new(file);
        for digit in digits {
            writer.write_all(&[b'0' + *digit])?;
        }
        writer.flush()?;
        Ok(())
    }

    pub fn read_range(&self, offset: u64, len: usize) -> Result<Vec<u8>> {
        if len == 0 {
            return Ok(Vec::new());
        }
        let mut file = match File::open(&self.path) {
            Ok(file) => file,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(err) => {
                return Err(err).with_context(|| format!("failed to open {}", self.path.display()));
            }
        };
        file.seek(SeekFrom::Start(offset))?;
        let mut bytes = vec![0_u8; len];
        let read = file.read(&mut bytes)?;
        bytes.truncate(read);
        convert_ascii_digits(&bytes)
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

pub struct CachedGrowingPiSource {
    cache: PiCache,
    desired_digits: Arc<AtomicU64>,
    stop_prefetch: Arc<AtomicBool>,
    prefetch_handle: Mutex<Option<JoinHandle<Result<()>>>>,
}

impl CachedGrowingPiSource {
    pub fn new(cache: PiCache) -> Self {
        let desired_digits = Arc::new(AtomicU64::new(0));
        let stop_prefetch = Arc::new(AtomicBool::new(false));
        let worker_cache = cache.clone();
        let worker_desired = Arc::clone(&desired_digits);
        let worker_stop = Arc::clone(&stop_prefetch);
        let prefetch_handle = thread::spawn(move || {
            background_prefetch_loop(worker_cache, worker_desired, worker_stop)
        });

        Self {
            cache,
            desired_digits,
            stop_prefetch,
            prefetch_handle: Mutex::new(Some(prefetch_handle)),
        }
    }

    fn request_min_digits(&self, min_digits: u64) {
        let mut current = self.desired_digits.load(Ordering::Acquire);
        while min_digits > current {
            match self.desired_digits.compare_exchange_weak(
                current,
                min_digits,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => break,
                Err(observed) => current = observed,
            }
        }
    }
}

impl Drop for CachedGrowingPiSource {
    fn drop(&mut self) {
        self.stop_prefetch.store(true, Ordering::Release);
        if let Ok(mut handle) = self.prefetch_handle.lock() {
            if let Some(handle) = handle.take() {
                if handle.is_finished() {
                    let _ = handle.join();
                }
            }
        }
    }
}

impl DigitSource for CachedGrowingPiSource {
    fn kind(&self) -> &'static str {
        "cache"
    }

    fn len(&self) -> Result<u64> {
        self.cache.digit_count()
    }

    fn validate(&self) -> Result<()> {
        self.cache.validate()
    }

    fn read_range(&self, offset: u64, len: usize) -> Result<Vec<u8>> {
        let required_digits = offset
            .checked_add(len as u64)
            .ok_or_else(|| anyhow!("requested pi range overflowed"))?;
        self.request_min_digits(required_digits.saturating_add(ON_DEMAND_CACHE_LEAD));
        self.cache.read_range(offset, len)
    }

    fn is_growing(&self) -> bool {
        true
    }

    fn request_prefetch(&self, min_digits: u64) -> Result<()> {
        self.request_min_digits(min_digits);
        Ok(())
    }
}

fn background_prefetch_loop(
    cache: PiCache,
    desired_digits: Arc<AtomicU64>,
    stop: Arc<AtomicBool>,
) -> Result<()> {
    while !stop.load(Ordering::Acquire) {
        let desired = desired_digits.load(Ordering::Acquire);
        let current = cache.digit_count()?;
        if desired > current {
            generate_into_cache(&cache, desired - current, Arc::clone(&stop))?;
        } else {
            thread::sleep(Duration::from_millis(20));
        }
    }
    Ok(())
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

pub fn generate_into_cache(cache: &PiCache, digits: u64, stop: Arc<AtomicBool>) -> Result<u64> {
    let _guard = generation_guard()?;
    generate_into_cache_unlocked(cache, digits, stop)
}

fn generate_into_cache_unlocked(
    cache: &PiCache,
    digits: u64,
    stop: Arc<AtomicBool>,
) -> Result<u64> {
    generate_into_cache_fast(cache, digits, stop.clone()).or_else(|fast_err| {
        eprintln!("warning: fast pi generator failed; falling back to spigot: {fast_err:#}");
        generate_into_cache_spigot(cache, digits, stop)
    })
}

fn generation_guard() -> Result<std::sync::MutexGuard<'static, ()>> {
    PI_GENERATION_LOCK
        .get_or_init(|| Mutex::new(()))
        .lock()
        .map_err(|_| anyhow!("pi generation lock was poisoned"))
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
    if digits == 0 {
        return Ok(Vec::new());
    }
    let guard_digits = 8usize;
    let scale_digits = digits + guard_digits;
    let terms = ((scale_digits as f64 / CHUDNOVSKY_DIGITS_PER_TERM).ceil() as u64 + 2).max(2);
    let (_p, q, t) = chudnovsky_bs_gmp(0, terms);
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

#[cfg(not(target_env = "msvc"))]
fn chudnovsky_bs_gmp(a: u64, b: u64) -> (Integer, Integer, Integer) {
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
    let (left, right) = if b - a >= CHUDNOVSKY_PARALLEL_THRESHOLD {
        rayon::join(|| chudnovsky_bs_gmp(a, mid), || chudnovsky_bs_gmp(mid, b))
    } else {
        (chudnovsky_bs_gmp(a, mid), chudnovsky_bs_gmp(mid, b))
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
    use std::sync::atomic::AtomicBool;

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
}
