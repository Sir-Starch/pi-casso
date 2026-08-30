#![cfg(test)]

use std::io::Write;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use anyhow::{Result, anyhow, bail};
use tempfile::NamedTempFile;

use super::DigitReaderPool;
use crate::digits::{DigitSource, DigitSourceSpec, FileDigitSource};
use crate::pi::{CachedGrowingPiSource, PiCache};

#[test]
fn direct_cached_source_delegates_snapshot_validation() -> Result<()> {
    // Given: a direct growing cache source and a complete published snapshot.
    let file = NamedTempFile::new()?;
    let cache = PiCache::new(file.path().to_path_buf());
    cache.append_digits(&[3, 1, 4, 1, 5])?;
    let source = CachedGrowingPiSource::new(cache);
    let pool = DigitReaderPool::new(&source, 1, 1, 5)?;

    // When: the raw publication changes without republishing its sidecar.
    std::fs::write(file.path(), b"271820")?;

    // Then: the source hides its raw path and the delegated read rejects the mutation.
    assert!(source.reader_path().is_none());
    let error = match pool.read_range(0, 5) {
        Ok(_) => bail!("inconsistent cache publication unexpectedly returned digits"),
        Err(error) => error.to_string(),
    };
    assert!(error.contains("sidecar status: inconsistent"));
    Ok(())
}

#[test]
fn reader_pool_size_matches_workers_and_queue_depth() {
    // Given: worker counts below, within, and above the queue depth.
    let cases = [(0, 4, 1), (1, 4, 1), (2, 8, 2), (8, 3, 3)];

    // When/Then: the pool follows min(max(1, cpu_workers), queue_depth).
    for (cpu_workers, queue_depth, expected) in cases {
        assert_eq!(
            DigitReaderPool::size_for(cpu_workers, queue_depth),
            expected
        );
    }
}

#[test]
fn digit_source_reuses_buffers_and_preserves_offsets() -> Result<()> {
    // Given: one formatted file reader with a six-digit bounded buffer.
    let mut file = NamedTempFile::new()?;
    write!(file, "314 159\n265358")?;
    let source = FileDigitSource::new_with_options(file.path().to_path_buf(), false);
    let pool = DigitReaderPool::new(&source, 4, 1, 6)?;
    let reserved = pool.telemetry().reserved_bytes;

    // When: sequential chunks overlap and a third request is fully cached.
    assert_eq!(&*pool.read_range(0, 6)?, &[3, 1, 4, 1, 5, 9]);
    assert_eq!(&*pool.read_range(4, 6)?, &[5, 9, 2, 6, 5, 3]);
    assert_eq!(&*pool.read_range(6, 4)?, &[2, 6, 5, 3]);
    let telemetry = pool.telemetry();

    // Then: the handle and capacities are reused without a steady-path seek.
    assert_eq!(telemetry.reader_pool_size, 1);
    assert_eq!(telemetry.reader_open_count, 1);
    assert_eq!(telemetry.reader_reuse_count, 2);
    assert_eq!(telemetry.reader_seek_count, 0);
    assert_eq!(telemetry.buffer_growth_count, 0);
    assert_eq!(telemetry.reserved_bytes, reserved);
    assert!(telemetry.cache_hit_count >= 2);

    // When/Then: a backward miss safely seeks and still preserves digit offsets.
    assert_eq!(&*pool.read_range(1, 3)?, &[1, 4, 1]);
    assert_eq!(pool.telemetry().reader_seek_count, 1);
    Ok(())
}

#[test]
fn growing_cache_reader_observes_appends_without_reopening() -> Result<()> {
    // Given: a published cache source whose file grows after the reader is opened.
    let file = NamedTempFile::new()?;
    let cache = PiCache::new(file.path().to_path_buf());
    cache.append_digits(&[3, 1, 4, 1, 5])?;
    let spec = DigitSourceSpec::cache(file.path().to_path_buf());
    let source = spec.open()?;
    let pool = DigitReaderPool::new(source.as_ref(), 2, 1, 8)?;
    assert_eq!(&*pool.read_range(0, 5)?, &[3, 1, 4, 1, 5]);

    // When: valid digits are appended to the same published cache inode.
    cache.append_digits(&[9, 2, 6, 5])?;

    // Then: the existing handle reads the new suffix and is reused.
    assert_eq!(&*pool.read_range(3, 6)?, &[1, 5, 9, 2, 6, 5]);
    let telemetry = pool.telemetry();
    assert_eq!(telemetry.reader_open_count, 1);
    assert_eq!(telemetry.reader_reuse_count, 1);
    Ok(())
}

#[test]
fn growing_cache_source_does_not_select_raw_path_reader() -> Result<()> {
    // Given: a valid published cache opened through the growing source.
    let file = NamedTempFile::new()?;
    let cache = PiCache::new(file.path().to_path_buf());
    cache.append_digits(&[3, 1, 4, 1, 5])?;
    let source = DigitSourceSpec::cache(file.path().to_path_buf()).open()?;

    // When/Then: pooled construction must choose the delegated snapshot-aware path.
    assert!(source.reader_path().is_none());
    let pool = DigitReaderPool::new(source.as_ref(), 1, 1, 5)?;
    assert_eq!(&*pool.read_range(0, 5)?, &[3, 1, 4, 1, 5]);
    Ok(())
}

#[test]
fn pooled_cache_reader_rejects_inconsistent_publication() -> Result<()> {
    // Given: a pooled reader opened over a complete published cache.
    let file = NamedTempFile::new()?;
    let cache = PiCache::new(file.path().to_path_buf());
    cache.append_digits(&[3, 1, 4, 1, 5])?;
    let source = DigitSourceSpec::cache(file.path().to_path_buf()).open()?;
    let pool = DigitReaderPool::new(source.as_ref(), 1, 1, 5)?;

    // When: the raw publication changes without its sidecar being republished.
    std::fs::write(file.path(), b"27182")?;

    // Then: the delegated reader rejects the inconsistent snapshot instead of reading shifted data.
    let error = match pool.read_range(0, 5) {
        Ok(_) => bail!("inconsistent cache publication unexpectedly returned digits"),
        Err(error) => error.to_string(),
    };
    assert!(error.contains("sidecar status: inconsistent"));
    Ok(())
}

#[test]
fn concurrent_file_readers_keep_exact_ranges() -> Result<()> {
    // Given: a two-slot pool over a shared formatted source.
    let mut file = NamedTempFile::new()?;
    write!(file, "314159265358979323846")?;
    let source = FileDigitSource::new_with_options(file.path().to_path_buf(), false);
    let pool = Arc::new(DigitReaderPool::new(&source, 8, 2, 8)?);

    // When: four threads request unaligned ranges concurrently.
    let ranges = [(0, 8), (3, 7), (8, 6), (13, 8)];
    let observed = std::thread::scope(|scope| {
        let handles = ranges
            .into_iter()
            .map(|(offset, len)| {
                let pool = Arc::clone(&pool);
                scope.spawn(move || -> Result<Vec<u8>> {
                    Ok(pool.read_range(offset, len)?.to_vec())
                })
            })
            .collect::<Vec<_>>();
        handles
            .into_iter()
            .map(|handle| {
                handle
                    .join()
                    .map_err(|_| anyhow!("digit reader worker panicked"))?
            })
            .collect::<Result<Vec<_>>>()
    })?;

    // Then: scheduling cannot shift any absolute digit offset.
    assert_eq!(
        observed,
        vec![
            vec![3, 1, 4, 1, 5, 9, 2, 6],
            vec![1, 5, 9, 2, 6, 5, 3],
            vec![5, 3, 5, 8, 9, 7],
            vec![7, 9, 3, 2, 3, 8, 4, 6],
        ]
    );
    assert_eq!(pool.telemetry().reader_open_count, 2);
    Ok(())
}

#[test]
fn pooled_reader_rejects_short_non_digit_and_oversized_ranges() -> Result<()> {
    // Given: a short raw digit file containing one malformed byte.
    let file = NamedTempFile::new()?;
    std::fs::write(file.path(), b"314x")?;
    let spec = DigitSourceSpec::file(file.path().to_path_buf(), false);
    let source = spec.open()?;
    let pool = DigitReaderPool::new(source.as_ref(), 1, 1, 4)?;

    // When/Then: malformed input reports its exact byte offset.
    let error = match pool.read_range(0, 4) {
        Ok(_) => bail!("malformed raw range unexpectedly succeeded"),
        Err(error) => error.to_string(),
    };
    assert!(error.contains("invalid byte 0x78"));
    assert!(error.contains("byte offset 3"));

    // Given/When/Then: a valid short source returns only available digits.
    std::fs::write(file.path(), b"314")?;
    let spec = DigitSourceSpec::file(file.path().to_path_buf(), false);
    let source = spec.open()?;
    let pool = DigitReaderPool::new(source.as_ref(), 1, 1, 4)?;
    assert_eq!(&*pool.read_range(1, 3)?, &[1, 4]);

    // When/Then: a request outside the configured chunk budget is rejected.
    assert!(pool.read_range(0, 5).is_err());
    Ok(())
}

struct FailsOnceSource {
    calls: AtomicUsize,
}

impl DigitSource for FailsOnceSource {
    fn kind(&self) -> &'static str {
        "test"
    }

    fn len(&self) -> Result<u64> {
        Ok(3)
    }

    fn validate(&self) -> Result<()> {
        Ok(())
    }

    fn read_range(&self, _offset: u64, _len: usize) -> Result<Vec<u8>> {
        if self.calls.fetch_add(1, Ordering::SeqCst) == 0 {
            bail!("injected read failure");
        }
        Ok(vec![3, 1, 4])
    }
}

#[test]
fn reader_returns_slot_after_error_for_safe_retry() -> Result<()> {
    // Given: a source that fails its first read and succeeds thereafter.
    let source = FailsOnceSource {
        calls: AtomicUsize::new(0),
    };
    let pool = DigitReaderPool::new(&source, 1, 1, 3)?;

    // When/Then: the first error is preserved and the same slot remains usable.
    assert!(pool.read_range(0, 3).is_err());
    assert_eq!(&*pool.read_range(0, 3)?, &[3, 1, 4]);
    assert_eq!(pool.telemetry().reader_reuse_count, 1);
    Ok(())
}
