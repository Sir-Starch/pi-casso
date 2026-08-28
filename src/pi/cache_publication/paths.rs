#[cfg(not(windows))]
use std::fs::File;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::{Context, Result, bail};

static TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);

pub(crate) struct PublicationPaths {
    pub(crate) raw: PathBuf,
    pub(crate) sidecar: PathBuf,
    pub(crate) lock: PathBuf,
    pub(crate) previous_raw: PathBuf,
    pub(crate) previous_sidecar: PathBuf,
    parent: PathBuf,
    stem: String,
}

impl PublicationPaths {
    pub(crate) fn new(raw: &Path) -> Result<Self> {
        reject_symlink_components(raw)?;
        let parent = raw.parent().unwrap_or_else(|| Path::new(".")).to_path_buf();
        std::fs::create_dir_all(&parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
        let stem = raw
            .file_stem()
            .and_then(|value| value.to_str())
            .context("pi cache path must have a UTF-8 file stem")?
            .to_owned();
        let raw_name = raw
            .file_name()
            .and_then(|value| value.to_str())
            .context("pi cache path must have a UTF-8 file name")?;
        let paths = Self {
            raw: raw.to_path_buf(),
            lock: parent.join(format!("{stem}.digits.lock")),
            previous_raw: parent.join(format!("{raw_name}.previous")),
            previous_sidecar: parent.join(format!("{stem}.digits.json.previous")),
            sidecar: parent.join(format!("{stem}.digits.json")),
            parent,
            stem,
        };
        for path in [
            &paths.raw,
            &paths.sidecar,
            &paths.lock,
            &paths.previous_raw,
            &paths.previous_sidecar,
        ] {
            validate_publication_file(path)?;
        }
        Ok(paths)
    }

    pub(crate) fn unique_temp(&self, purpose: &str) -> PathBuf {
        let sequence = TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        self.parent.join(format!(
            ".{}.{}.{}.{}.tmp",
            self.stem,
            purpose,
            std::process::id(),
            sequence
        ))
    }

    #[cfg(windows)]
    pub(crate) fn sync_parent(&self) -> Result<()> {
        Ok(())
    }

    #[cfg(not(windows))]
    pub(crate) fn sync_parent(&self) -> Result<()> {
        File::open(&self.parent)?
            .sync_all()
            .with_context(|| format!("failed to synchronize {}", self.parent.display()))
    }
}

pub(crate) fn reject_symlink_components(path: &Path) -> Result<()> {
    let mut current = PathBuf::new();
    for component in path.components() {
        current.push(component.as_os_str());
        match std::fs::symlink_metadata(&current) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                bail!("path component {} must not be a symlink", current.display());
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

fn validate_publication_file(path: &Path) -> Result<()> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            bail!("publication path {} must not be a symlink", path.display());
        }
        Ok(metadata) if !metadata.is_file() => {
            bail!("publication path {} must be a regular file", path.display());
        }
        Ok(_) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}
