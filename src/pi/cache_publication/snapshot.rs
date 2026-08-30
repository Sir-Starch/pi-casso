use std::fs::{File, Metadata};
use std::io::Read;
#[cfg(unix)]
use std::os::unix::fs::MetadataExt;
#[cfg(windows)]
use std::os::windows::fs::MetadataExt;
#[cfg(windows)]
use std::os::windows::io::AsRawHandle;
use std::path::Path;
#[cfg(test)]
use std::{cell::Cell, thread_local};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

const READ_BUFFER_BYTES: usize = 64 * 1024;

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Sidecar {
    pub(crate) schema_version: u8,
    pub(crate) published_digits: u64,
    pub(crate) raw_file_size: u64,
    pub(crate) published_prefix_sha256: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) raw_file_identity: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) raw_metadata_fingerprint: Option<String>,
}

impl Sidecar {
    pub(crate) fn from_raw(raw: &RawSnapshot) -> Self {
        Self {
            schema_version: 1,
            published_digits: raw.file_size,
            raw_file_size: raw.file_size,
            published_prefix_sha256: raw.full_sha256.clone(),
            raw_file_identity: raw.file_identity.clone(),
            raw_metadata_fingerprint: raw.metadata_fingerprint.clone(),
        }
    }

    pub(crate) fn matches_exact(&self, raw: &RawSnapshot) -> bool {
        raw.valid_ascii
            && self.published_digits == raw.file_size
            && self.raw_file_size == raw.file_size
            && self.published_prefix_sha256 == raw.full_sha256
    }

    pub(crate) fn matches_published_prefix(&self, raw: &RawSnapshot) -> bool {
        raw.valid_ascii
            && self.published_digits == self.raw_file_size
            && self.published_digits <= raw.file_size
            && self
                .raw_file_identity
                .as_ref()
                .is_none_or(|expected| raw.file_identity.as_ref() == Some(expected))
            && raw.prefix_bytes == self.published_digits
            && raw.prefix_sha256.as_ref() == Some(&self.published_prefix_sha256)
    }

    fn structurally_valid(&self) -> bool {
        self.schema_version == 1
            && self.published_digits == self.raw_file_size
            && self.published_prefix_sha256.len() == 64
            && self
                .raw_file_identity
                .as_ref()
                .is_none_or(|identity| !identity.is_empty())
            && self
                .raw_metadata_fingerprint
                .as_ref()
                .is_none_or(|fingerprint| !fingerprint.is_empty())
            && self
                .published_prefix_sha256
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit())
    }
}

pub(crate) enum SidecarRead {
    Missing,
    Invalid,
    Parsed(Sidecar),
}

pub(crate) struct RawSnapshot {
    pub(crate) file_size: u64,
    pub(crate) valid_digits: u64,
    pub(crate) valid_ascii: bool,
    pub(crate) full_sha256: String,
    pub(crate) prefix_bytes: u64,
    pub(crate) prefix_sha256: Option<String>,
    pub(crate) file_identity: Option<String>,
    pub(crate) metadata_fingerprint: Option<String>,
}

impl RawSnapshot {
    pub(crate) fn read(path: &Path, prefix_limit: Option<u64>) -> Result<Self> {
        #[cfg(test)]
        RAW_SNAPSHOT_READS.with(|reads| reads.set(reads.get().saturating_add(1)));

        let mut file = match File::open(path) {
            Ok(file) => Some(file),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("failed to open pi cache {}", path.display()));
            }
        };
        let metadata = file.as_ref().map(|open| open.metadata()).transpose()?;
        let file_size = metadata.as_ref().map_or(0, Metadata::len);
        let file_identity = file_identity(file.as_ref(), metadata.as_ref());
        let metadata_fingerprint = metadata_fingerprint(file.as_ref(), metadata.as_ref());
        let mut full_hasher = Sha256::new();
        let mut prefix_hasher = prefix_limit.map(|_| Sha256::new());
        let mut valid_digits = 0_u64;
        let mut prefix_bytes = 0_u64;
        let mut valid_ascii = true;
        let mut buffer = [0_u8; READ_BUFFER_BYTES];
        while let Some(open) = file.as_mut() {
            let read = open.read(&mut buffer)?;
            if read == 0 {
                break;
            }
            let scan_len = prefix_limit
                .map(|limit| {
                    usize::try_from(limit.saturating_sub(prefix_bytes))
                        .map(|remaining| remaining.min(read))
                })
                .transpose()?
                .unwrap_or(read);
            if scan_len == 0 {
                break;
            }
            let valid = buffer[..scan_len]
                .iter()
                .position(|byte| !byte.is_ascii_digit())
                .unwrap_or(scan_len);
            full_hasher.update(&buffer[..valid]);
            if let (Some(limit), Some(hasher)) = (prefix_limit, prefix_hasher.as_mut()) {
                let remaining = limit.saturating_sub(prefix_bytes);
                let take = usize::try_from(remaining.min(u64::try_from(valid)?))?;
                hasher.update(&buffer[..take]);
                prefix_bytes = prefix_bytes.saturating_add(u64::try_from(take)?);
            }
            valid_digits = valid_digits.saturating_add(u64::try_from(valid)?);
            if valid != scan_len {
                valid_ascii = false;
                break;
            }
            if prefix_limit.is_some_and(|limit| prefix_bytes >= limit) {
                break;
            }
        }
        Ok(Self {
            file_size,
            valid_digits,
            valid_ascii,
            full_sha256: format!("{:x}", full_hasher.finalize()),
            prefix_bytes,
            prefix_sha256: prefix_hasher.map(|hasher| format!("{:x}", hasher.finalize())),
            file_identity,
            metadata_fingerprint,
        })
    }
}

pub(super) fn file_identity(file: Option<&File>, metadata: Option<&Metadata>) -> Option<String> {
    #[cfg(unix)]
    {
        let _ = file;
        let metadata = metadata?;
        Some(format!("unix:{}:{}", metadata.dev(), metadata.ino()))
    }
    #[cfg(windows)]
    {
        use windows_sys::Win32::Storage::FileSystem::{
            BY_HANDLE_FILE_INFORMATION, GetFileInformationByHandle,
        };

        let _ = metadata;
        let file = file?;
        let mut information = BY_HANDLE_FILE_INFORMATION::default();
        // SAFETY: `file` owns a valid handle for the duration of this call and
        // `information` is a valid writable buffer of the documented type.
        let result = unsafe { GetFileInformationByHandle(file.as_raw_handle(), &mut information) };
        if result == 0 {
            return None;
        }
        let file_index =
            (u64::from(information.nFileIndexHigh) << 32) | u64::from(information.nFileIndexLow);
        Some(format!(
            "windows:{}:{}",
            information.dwVolumeSerialNumber, file_index
        ))
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = (file, metadata);
        None
    }
}

pub(super) fn metadata_fingerprint(
    file: Option<&File>,
    metadata: Option<&Metadata>,
) -> Option<String> {
    let metadata = metadata?;
    let identity = file_identity(file, Some(metadata))?;
    #[cfg(unix)]
    {
        Some(format!(
            "{identity}:{}:{}:{}:{}",
            metadata.ctime(),
            metadata.ctime_nsec(),
            metadata.mtime(),
            metadata.mtime_nsec()
        ))
    }
    #[cfg(windows)]
    {
        Some(format!("{identity}:{}", metadata.last_write_time()))
    }
    #[cfg(not(any(unix, windows)))]
    {
        None
    }
}

#[cfg(test)]
thread_local! {
    static RAW_SNAPSHOT_READS: Cell<usize> = const { Cell::new(0) };
}

#[cfg(test)]
pub(super) fn reset_raw_snapshot_reads() {
    RAW_SNAPSHOT_READS.with(|reads| reads.set(0));
}

#[cfg(test)]
pub(super) fn raw_snapshot_reads() -> usize {
    RAW_SNAPSHOT_READS.with(Cell::get)
}

pub(crate) fn read_sidecar(path: &Path) -> Result<SidecarRead> {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(SidecarRead::Missing);
        }
        Err(error) => return Err(error.into()),
    };
    let Ok(sidecar) = serde_json::from_slice::<Sidecar>(&bytes) else {
        return Ok(SidecarRead::Invalid);
    };
    if sidecar.structurally_valid() {
        Ok(SidecarRead::Parsed(sidecar))
    } else {
        Ok(SidecarRead::Invalid)
    }
}

pub(crate) fn inspect(raw: &Path, sidecar: &Path) -> Result<(RawSnapshot, SidecarRead)> {
    let sidecar = read_sidecar(sidecar)?;
    let prefix = match &sidecar {
        SidecarRead::Parsed(value) => Some(value.published_digits),
        SidecarRead::Missing | SidecarRead::Invalid => None,
    };
    Ok((RawSnapshot::read(raw, prefix)?, sidecar))
}
