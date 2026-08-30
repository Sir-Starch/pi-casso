pub(crate) mod lock;
mod paths;
mod snapshot;
#[cfg(test)]
mod tests;

use std::fs::{self, File, OpenOptions};
use std::io::{BufReader, BufWriter, Read, Seek, SeekFrom, Write};
use std::path::Path;
use std::time::Instant;

use anyhow::{Context, Result, anyhow, bail};

use crate::digits::{CachePublicationError, DigitRead, convert_ascii_digits};

use self::lock::{LockState, with_writer_lock};
use self::paths::{PublicationPaths, reject_symlink_components};
use self::snapshot::{RawSnapshot, Sidecar, SidecarRead, inspect, read_sidecar};

const COPY_BUFFER_BYTES: usize = 64 * 1024;
const READ_SNAPSHOT_RETRIES: usize = 3;

pub(crate) struct CacheSnapshot {
    pub(crate) digits: u64,
    pub(crate) raw_file_size: u64,
    pub(crate) published_digits: u64,
    pub(crate) published_prefix_sha256: String,
    pub(crate) valid_ascii: bool,
    pub(crate) sidecar_status: &'static str,
}

struct FastCacheSnapshot {
    raw_file_size: u64,
    published_digits: u64,
    sidecar_status: &'static str,
    snapshot_id: String,
}

pub(crate) fn info(raw_path: &Path) -> Result<CacheSnapshot> {
    let paths = PublicationPaths::new(raw_path)?;
    let sidecar = read_sidecar(&paths.sidecar)?;
    let published = match &sidecar {
        SidecarRead::Parsed(value) => Some(value.published_digits),
        SidecarRead::Missing | SidecarRead::Invalid => None,
    };
    let raw = RawSnapshot::read(&paths.raw, published)?;
    let lock_state = lock::observe(&paths.lock)?;

    Ok(match sidecar {
        SidecarRead::Missing => CacheSnapshot {
            digits: raw.valid_digits,
            raw_file_size: raw.file_size,
            published_digits: 0,
            published_prefix_sha256: String::new(),
            valid_ascii: raw.valid_ascii,
            sidecar_status: "missing",
        },
        SidecarRead::Invalid => CacheSnapshot {
            digits: raw.valid_digits,
            raw_file_size: raw.file_size,
            published_digits: 0,
            published_prefix_sha256: String::new(),
            valid_ascii: raw.valid_ascii,
            sidecar_status: "invalid",
        },
        SidecarRead::Parsed(sidecar) => {
            let exact = sidecar.matches_exact(&raw);
            let active_prefix =
                matches!(lock_state, LockState::Live) && sidecar.matches_published_prefix(&raw);
            let status = if exact || active_prefix {
                "ok"
            } else {
                "inconsistent"
            };
            CacheSnapshot {
                digits: if exact || active_prefix {
                    sidecar.published_digits
                } else {
                    raw.valid_digits.min(sidecar.published_digits)
                },
                raw_file_size: raw.file_size,
                published_digits: sidecar.published_digits,
                published_prefix_sha256: sidecar.published_prefix_sha256,
                valid_ascii: raw.valid_ascii,
                sidecar_status: status,
            }
        }
    })
}

fn fast_info(raw_path: &Path) -> Result<FastCacheSnapshot> {
    let paths = PublicationPaths::new(raw_path)?;
    let sidecar = read_sidecar(&paths.sidecar)?;
    #[cfg(windows)]
    let raw_file = open_file_if_present(&paths.raw)?;
    #[cfg(not(windows))]
    let raw_file: Option<File> = None;
    let raw_metadata = metadata_if_present(&paths.raw)?;
    let raw_metadata = raw_file
        .as_ref()
        .map(File::metadata)
        .transpose()?
        .or(raw_metadata);
    let raw_file_size = raw_metadata.as_ref().map_or(0, fs::Metadata::len);
    let raw_identity = snapshot::file_identity(raw_file.as_ref(), raw_metadata.as_ref());
    let raw_fingerprint = snapshot::metadata_fingerprint(raw_file.as_ref(), raw_metadata.as_ref());
    let lock_state = lock::observe(&paths.lock)?;

    match sidecar {
        SidecarRead::Missing => Ok(FastCacheSnapshot {
            raw_file_size,
            published_digits: 0,
            sidecar_status: "missing",
            snapshot_id: fast_snapshot_id(
                "missing",
                None,
                raw_file_size,
                raw_identity.as_deref(),
                raw_fingerprint.as_deref(),
            ),
        }),
        SidecarRead::Invalid => Ok(FastCacheSnapshot {
            raw_file_size,
            published_digits: 0,
            sidecar_status: "invalid",
            snapshot_id: fast_snapshot_id(
                "invalid",
                None,
                raw_file_size,
                raw_identity.as_deref(),
                raw_fingerprint.as_deref(),
            ),
        }),
        SidecarRead::Parsed(sidecar) => {
            let exact_size = sidecar.raw_file_size == raw_file_size;
            let exact = if exact_size {
                match sidecar.raw_metadata_fingerprint.as_ref() {
                    Some(expected) => raw_fingerprint.as_ref() == Some(expected),
                    None => sidecar.matches_exact(&RawSnapshot::read(&paths.raw, None)?),
                }
            } else {
                false
            };
            let active_prefix = if matches!(lock_state, LockState::Live)
                && sidecar.published_digits == sidecar.raw_file_size
                && sidecar.published_digits <= raw_file_size
            {
                match sidecar.raw_file_identity.as_ref() {
                    Some(expected) => raw_identity.as_ref() == Some(expected),
                    None => sidecar.matches_published_prefix(&RawSnapshot::read(
                        &paths.raw,
                        Some(sidecar.published_digits),
                    )?),
                }
            } else {
                false
            };
            let status = if exact || active_prefix {
                "ok"
            } else {
                "inconsistent"
            };
            Ok(FastCacheSnapshot {
                raw_file_size,
                published_digits: sidecar.published_digits,
                sidecar_status: status,
                snapshot_id: fast_snapshot_id(
                    status,
                    Some(&sidecar),
                    raw_file_size,
                    raw_identity.as_deref(),
                    raw_fingerprint.as_deref(),
                ),
            })
        }
    }
}

pub(crate) fn published_digit_count(raw_path: &Path) -> Result<u64> {
    let snapshot = fast_info(raw_path)?;
    readable_published_digits(
        raw_path,
        snapshot.sidecar_status,
        snapshot.raw_file_size,
        snapshot.published_digits,
    )
}

fn metadata_if_present(path: &Path) -> Result<Option<fs::Metadata>> {
    match fs::metadata(path) {
        Ok(metadata) => Ok(Some(metadata)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error).with_context(|| format!("failed to stat {}", path.display())),
    }
}

#[cfg(windows)]
fn open_file_if_present(path: &Path) -> Result<Option<File>> {
    match File::open(path) {
        Ok(file) => Ok(Some(file)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => {
            Err(error).with_context(|| format!("failed to open pi cache {}", path.display()))
        }
    }
}

pub(crate) fn read_range_timed(raw_path: &Path, offset: u64, len: usize) -> Result<DigitRead> {
    if crate::gpu_ring::test_mode_enabled()
        && std::env::var("PI_CASSO_TEST_FAIL_IF_SOURCE_OPEN").is_ok_and(|value| value == "1")
    {
        bail!("PI_CASSO_TEST_FAIL_IF_SOURCE_OPEN reached the source-open boundary");
    }

    let requested_end = offset
        .checked_add(u64::try_from(len)?)
        .context("requested pi cache range overflowed")?;

    for attempt in 0..READ_SNAPSHOT_RETRIES {
        let before = fast_info(raw_path)?;
        let published_digits = readable_published_digits(
            raw_path,
            before.sidecar_status,
            before.raw_file_size,
            before.published_digits,
        )?;
        let read_started = Instant::now();
        let bytes = if offset >= published_digits || offset >= requested_end {
            Vec::new()
        } else {
            let end = requested_end.min(published_digits);
            let read_len = usize::try_from(end - offset)?;
            read_raw_range(raw_path, offset, read_len)?
        };
        let read = read_started.elapsed();
        let parse_started = Instant::now();
        let digits = match convert_ascii_digits(&bytes) {
            Ok(digits) => digits,
            Err(error) => {
                let current = info(raw_path)?;
                let _ = readable_published_digits(
                    raw_path,
                    current.sidecar_status,
                    current.raw_file_size,
                    current.published_digits,
                )?;
                return Err(error);
            }
        };
        let parse = parse_started.elapsed();
        let after = fast_info(raw_path)?;
        if before.snapshot_id == after.snapshot_id {
            return Ok(DigitRead {
                digits,
                read,
                parse,
            });
        }
        if attempt + 1 == READ_SNAPSHOT_RETRIES {
            return Err(publication_error(raw_path, "changed"));
        }
    }
    Err(publication_error(raw_path, "changed"))
}

fn readable_published_digits(
    raw_path: &Path,
    sidecar_status: &str,
    raw_file_size: u64,
    published_digits: u64,
) -> Result<u64> {
    if sidecar_status == "missing" && raw_file_size == 0 {
        return Ok(0);
    }
    if sidecar_status != "ok" {
        return Err(publication_error(raw_path, sidecar_status));
    }
    if published_digits == 0 && raw_file_size > 0 {
        return Err(publication_error(raw_path, "unpublished"));
    }
    Ok(published_digits)
}

fn read_raw_range(raw_path: &Path, offset: u64, len: usize) -> Result<Vec<u8>> {
    let mut file = match File::open(raw_path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Err(publication_error(raw_path, "changed"));
        }
        Err(error) => {
            return Err(error)
                .with_context(|| format!("failed to open pi cache {}", raw_path.display()));
        }
    };
    file.seek(SeekFrom::Start(offset))?;
    let mut bytes = vec![0_u8; len];
    let read = file.read(&mut bytes)?;
    bytes.truncate(read);
    Ok(bytes)
}

fn publication_error(raw_path: &Path, sidecar_status: &str) -> anyhow::Error {
    CachePublicationError {
        path: raw_path.to_path_buf(),
        sidecar_status: sidecar_status.to_owned(),
    }
    .into()
}

fn fast_snapshot_id(
    status: &str,
    sidecar: Option<&Sidecar>,
    raw_file_size: u64,
    raw_identity: Option<&str>,
    raw_fingerprint: Option<&str>,
) -> String {
    let sidecar = sidecar.map_or_else(String::new, |value| {
        format!(
            "{}:{}:{}:{}:{:?}:{:?}",
            value.schema_version,
            value.published_digits,
            value.raw_file_size,
            value.published_prefix_sha256,
            value.raw_file_identity.as_deref(),
            value.raw_metadata_fingerprint.as_deref()
        )
    });
    format!("{status}:{raw_file_size}:{raw_identity:?}:{raw_fingerprint:?}:{sidecar}")
}

pub(crate) fn append_digits(raw_path: &Path, digits: &[u8]) -> Result<()> {
    if digits.iter().any(|digit| *digit > 9) {
        bail!("pi cache append received a non-digit value");
    }
    let paths = PublicationPaths::new(raw_path)?;
    with_writer_lock(&paths, |writer_lock| {
        ensure_writable(&paths)?;
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&paths.raw)
            .with_context(|| format!("failed to open pi cache {}", paths.raw.display()))?;
        let mut writer = BufWriter::with_capacity(COPY_BUFFER_BYTES, file);
        let mut ascii = [0_u8; COPY_BUFFER_BYTES];
        for chunk in digits.chunks(COPY_BUFFER_BYTES) {
            for (output, digit) in ascii.iter_mut().zip(chunk) {
                *output = b'0' + *digit;
            }
            writer.write_all(&ascii[..chunk.len()])?;
        }
        writer.flush()?;
        sync_publication_data(writer.get_ref())?;
        writer_lock.fail_after_raw_sync()?;

        let raw = RawSnapshot::read(&paths.raw, None)?;
        write_sidecar(&paths, &Sidecar::from_raw(&raw))?;
        writer_lock.fail_after_sidecar_rename()?;
        Ok(())
    })
}

pub(crate) fn append_from_validated_source(raw_path: &Path, source: &Path) -> Result<u64> {
    let paths = PublicationPaths::new(raw_path)?;
    with_writer_lock(&paths, |writer_lock| {
        ensure_writable(&paths)?;
        let raw = RawSnapshot::read(&paths.raw, None)?;
        let existing = raw.file_size;
        let mut source_file = open_regular_source(source, "continuation")?;
        let total = validate_continuation(&paths.raw, &mut source_file, existing)?;
        if total == existing {
            return Ok(0);
        }

        source_file.seek(SeekFrom::Start(existing))?;
        let output = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&paths.raw)?;
        let mut writer = BufWriter::with_capacity(COPY_BUFFER_BYTES, output);
        let mut buffer = [0_u8; COPY_BUFFER_BYTES];
        loop {
            let read = source_file.read(&mut buffer)?;
            if read == 0 {
                break;
            }
            writer.write_all(&buffer[..read])?;
        }
        writer.flush()?;
        sync_publication_data(writer.get_ref())?;
        writer_lock.fail_after_raw_sync()?;
        let raw = RawSnapshot::read(&paths.raw, None)?;
        write_sidecar(&paths, &Sidecar::from_raw(&raw))?;
        writer_lock.fail_after_sidecar_rename()?;
        total
            .checked_sub(existing)
            .context("continuation digit count underflowed")
    })
}

fn validate_continuation(raw_path: &Path, source_file: &mut File, existing: u64) -> Result<u64> {
    let mut raw_file =
        if existing == 0 {
            None
        } else {
            Some(File::open(raw_path).with_context(|| {
                format!("failed to open existing pi cache {}", raw_path.display())
            })?)
        };
    let mut source_buffer = [0_u8; COPY_BUFFER_BYTES];
    let mut raw_buffer = [0_u8; COPY_BUFFER_BYTES];
    let mut source_digits = 0_u64;
    let mut prefix_remaining = existing;
    loop {
        let read = source_file.read(&mut source_buffer)?;
        if read == 0 {
            break;
        }
        if source_buffer[..read]
            .iter()
            .any(|byte| !byte.is_ascii_digit())
        {
            bail!("continuation source contains a non-digit byte");
        }
        let prefix_len = prefix_remaining.min(u64::try_from(read)?);
        if prefix_len > 0 {
            let prefix_len = usize::try_from(prefix_len)?;
            let raw_file = raw_file
                .as_mut()
                .ok_or_else(|| anyhow!("existing pi cache prefix is unavailable"))?;
            raw_file.read_exact(&mut raw_buffer[..prefix_len])?;
            if raw_buffer[..prefix_len] != source_buffer[..prefix_len] {
                bail!("continuation source does not match the existing pi cache prefix");
            }
            prefix_remaining = prefix_remaining.saturating_sub(u64::try_from(prefix_len)?);
        }
        source_digits = source_digits
            .checked_add(u64::try_from(read)?)
            .context("continuation digit count overflowed")?;
    }
    if prefix_remaining != 0 {
        bail!("continuation source is shorter than the existing pi cache");
    }
    Ok(source_digits)
}

pub(crate) fn replace_from_validated_source(raw_path: &Path, source: &Path) -> Result<u64> {
    let paths = PublicationPaths::new(raw_path)?;
    with_writer_lock(&paths, |writer_lock| {
        ensure_writable(&paths)?;
        let staged = paths.unique_temp("replacement");
        let result = stage_validated_source(source, &staged).and_then(|digits| {
            backup_previous(&paths)?;
            replace_file(&staged, &paths.raw).with_context(|| {
                format!("failed to publish replacement {}", paths.raw.display())
            })?;
            paths.sync_parent()?;
            writer_lock.fail_after_raw_sync()?;
            let published_raw = RawSnapshot::read(&paths.raw, None)?;
            write_sidecar(&paths, &Sidecar::from_raw(&published_raw))?;
            writer_lock.fail_after_sidecar_rename()?;
            cleanup_previous(&paths)?;
            Ok(digits)
        });
        if result.is_err() {
            let _ = fs::remove_file(&staged);
        }
        result
    })
}

pub(crate) fn repair_publication(raw_path: &Path) -> Result<()> {
    let paths = PublicationPaths::new(raw_path)?;
    with_writer_lock(&paths, |_writer_lock| {
        let (raw, sidecar) = inspect(&paths.raw, &paths.sidecar)?;
        if !raw.valid_ascii {
            bail!("pi cache contains non-ASCII digit data and cannot be repaired");
        }
        if matches!(&sidecar, SidecarRead::Parsed(value) if value.matches_exact(&raw)) {
            write_sidecar(&paths, &Sidecar::from_raw(&raw))?;
            return cleanup_previous(&paths);
        }
        if restore_previous(&paths)? {
            return Ok(());
        }
        match sidecar {
            SidecarRead::Parsed(sidecar) if sidecar.matches_published_prefix(&raw) => {
                let file = OpenOptions::new().write(true).open(&paths.raw)?;
                file.set_len(sidecar.published_digits)?;
                sync_publication_data(&file)?;
                paths.sync_parent()?;
                let repaired = RawSnapshot::read(&paths.raw, None)?;
                write_sidecar(&paths, &Sidecar::from_raw(&repaired))?;
            }
            SidecarRead::Parsed(sidecar) if raw.file_size < sidecar.raw_file_size => {
                write_sidecar(&paths, &Sidecar::from_raw(&raw))?;
            }
            SidecarRead::Missing | SidecarRead::Invalid => {
                write_sidecar(&paths, &Sidecar::from_raw(&raw))?;
            }
            SidecarRead::Parsed(_) => {
                bail!("pi cache contents do not match the published sidecar")
            }
        }
        cleanup_previous(&paths)
    })
}

pub(crate) fn validate_reset_lock(raw_path: &Path) -> Result<()> {
    let paths = PublicationPaths::new(raw_path)?;
    match lock::observe(&paths.lock)? {
        LockState::Missing => Ok(()),
        #[cfg(any(unix, windows))]
        LockState::Dead => Ok(()),
        LockState::Live => bail!("pi cache writer lock belongs to a live process"),
        LockState::Unverifiable => bail!("pi cache writer lock cannot be verified"),
    }
}

fn ensure_writable(paths: &PublicationPaths) -> Result<()> {
    let (raw, sidecar) = inspect(&paths.raw, &paths.sidecar)?;
    if !raw.valid_ascii {
        bail!("pi cache contains invalid non-digit bytes");
    }
    match sidecar {
        SidecarRead::Missing => Ok(()),
        SidecarRead::Parsed(sidecar) if sidecar.matches_exact(&raw) => Ok(()),
        SidecarRead::Invalid => bail!("pi cache sidecar is invalid; repair publication first"),
        SidecarRead::Parsed(_) => {
            bail!("pi cache publication is inconsistent; repair publication first")
        }
    }
}

fn stage_validated_source(source: &Path, staged: &Path) -> Result<u64> {
    let input = open_regular_source(source, "replacement")?;
    let output = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(staged)?;
    let mut reader = BufReader::with_capacity(COPY_BUFFER_BYTES, input);
    let mut writer = BufWriter::with_capacity(COPY_BUFFER_BYTES, output);
    let mut buffer = [0_u8; COPY_BUFFER_BYTES];
    let mut digits = 0_u64;
    loop {
        let read = reader.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        if buffer[..read].iter().any(|byte| !byte.is_ascii_digit()) {
            bail!("replacement source contains a non-digit byte");
        }
        writer.write_all(&buffer[..read])?;
        digits = digits
            .checked_add(u64::try_from(read)?)
            .context("replacement digit count overflowed")?;
    }
    writer.flush()?;
    sync_publication_data(writer.get_ref())?;
    Ok(digits)
}

fn open_regular_source(source: &Path, purpose: &str) -> Result<File> {
    reject_symlink_components(source)?;
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW);
    }
    let input = options
        .open(source)
        .with_context(|| format!("failed to open {purpose} source {}", source.display()))?;
    if !input.metadata()?.is_file() {
        bail!(
            "{purpose} source {} must be a regular file",
            source.display()
        );
    }
    Ok(input)
}

/// Uses `sync_all` as the stronger form of the publication's `sync_data` durability contract.
fn sync_publication_data(file: &File) -> Result<()> {
    file.sync_all().map_err(Into::into)
}

fn replace_file(source: &Path, destination: &Path) -> Result<()> {
    #[cfg(windows)]
    {
        match fs::remove_file(destination) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(error).with_context(|| {
                    format!(
                        "failed to remove existing publication {}",
                        destination.display()
                    )
                });
            }
        }
    }

    fs::rename(source, destination).with_context(|| {
        format!(
            "failed to replace publication {} with {}",
            destination.display(),
            source.display()
        )
    })
}

fn write_sidecar(paths: &PublicationPaths, sidecar: &Sidecar) -> Result<()> {
    let staged = paths.unique_temp("sidecar");
    let result = (|| {
        let mut file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&staged)?;
        serde_json::to_writer(&mut file, sidecar)?;
        file.flush()?;
        sync_publication_data(&file)?;
        drop(file);
        replace_file(&staged, &paths.sidecar)?;
        paths.sync_parent()
    })();
    if result.is_err() {
        let _ = fs::remove_file(staged);
    }
    result
}

fn backup_previous(paths: &PublicationPaths) -> Result<()> {
    cleanup_previous(paths)?;
    copy_synced_if_present(&paths.raw, &paths.previous_raw)?;
    copy_synced_if_present(&paths.sidecar, &paths.previous_sidecar)?;
    paths.sync_parent()
}

fn copy_synced_if_present(source: &Path, destination: &Path) -> Result<()> {
    match fs::copy(source, destination) {
        Ok(_) => {
            let file = OpenOptions::new()
                .read(true)
                .write(true)
                .open(destination)
                .with_context(|| {
                    format!(
                        "failed to open copied publication {}",
                        destination.display()
                    )
                })?;
            sync_publication_data(&file).with_context(|| {
                format!(
                    "failed to synchronize copied publication {}",
                    destination.display()
                )
            })
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error).with_context(|| {
            format!(
                "failed to copy publication {} to {}",
                source.display(),
                destination.display()
            )
        }),
    }
}

fn restore_previous(paths: &PublicationPaths) -> Result<bool> {
    let (previous_raw, previous_sidecar) = inspect(&paths.previous_raw, &paths.previous_sidecar)?;
    let SidecarRead::Parsed(sidecar) = previous_sidecar else {
        return Ok(false);
    };
    if !sidecar.matches_exact(&previous_raw) {
        return Ok(false);
    }
    replace_file(&paths.previous_raw, &paths.raw)?;
    paths.sync_parent()?;
    replace_file(&paths.previous_sidecar, &paths.sidecar)?;
    paths.sync_parent()?;
    cleanup_previous(paths)?;
    Ok(true)
}

fn cleanup_previous(paths: &PublicationPaths) -> Result<()> {
    for path in [&paths.previous_raw, &paths.previous_sidecar] {
        match fs::remove_file(path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}
