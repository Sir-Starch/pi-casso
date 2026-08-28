use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::process::Stdio;
use std::time::{Duration, Instant};

use sha2::{Digest, Sha256};

use crate::digits::{DigitSource, FileDigitSource};

use super::PiCache;

#[derive(Clone, Copy, Debug, thiserror::Error)]
pub(crate) enum YCruncherFailure {
    #[error("executable_missing")]
    ExecutableMissing,
    #[error("not_executable")]
    NotExecutable,
    #[error("executable_changed")]
    ExecutableChanged,
    #[error("process_failed")]
    ProcessFailed,
    #[error("output_invalid")]
    OutputInvalid,
    #[error("import_failed")]
    ImportFailed,
}

impl YCruncherFailure {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::ExecutableMissing => "executable_missing",
            Self::NotExecutable => "not_executable",
            Self::ExecutableChanged => "executable_changed",
            Self::ProcessFailed => "process_failed",
            Self::OutputInvalid => "output_invalid",
            Self::ImportFailed => "import_failed",
        }
    }

    pub(crate) fn from_reason(reason: &str) -> Self {
        match reason {
            "executable_missing" => Self::ExecutableMissing,
            "not_executable" => Self::NotExecutable,
            "executable_changed" => Self::ExecutableChanged,
            "process_failed" => Self::ProcessFailed,
            "output_invalid" => Self::OutputInvalid,
            "import_failed" => Self::ImportFailed,
            _ => Self::ProcessFailed,
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) struct ValidatedYCruncher {
    canonical_path: PathBuf,
    sha256: String,
    identity: ExecutableIdentity,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ExecutableIdentity {
    len: u64,
    #[cfg(unix)]
    device: u64,
    #[cfg(unix)]
    inode: u64,
    #[cfg(not(unix))]
    modified: Option<std::time::SystemTime>,
}

impl ValidatedYCruncher {
    pub(crate) fn parse(path: &Path) -> Result<Self, YCruncherFailure> {
        let canonical_path = fs::canonicalize(path).map_err(|error| match error.kind() {
            std::io::ErrorKind::NotFound => YCruncherFailure::ExecutableMissing,
            _ => YCruncherFailure::NotExecutable,
        })?;
        let metadata =
            fs::metadata(&canonical_path).map_err(|_| YCruncherFailure::NotExecutable)?;
        if !metadata.is_file() || !is_executable(&metadata) {
            return Err(YCruncherFailure::NotExecutable);
        }
        let bytes = fs::read(&canonical_path).map_err(|_| YCruncherFailure::NotExecutable)?;
        Ok(Self {
            canonical_path,
            sha256: format!("{:x}", Sha256::digest(bytes)),
            identity: executable_identity(&metadata),
        })
    }

    pub(crate) fn sha256(&self) -> &str {
        &self.sha256
    }

    fn revalidate(&self) -> Result<(), YCruncherFailure> {
        let current =
            Self::parse(&self.canonical_path).map_err(|_| YCruncherFailure::ExecutableChanged)?;
        if current.canonical_path != self.canonical_path
            || current.sha256 != self.sha256
            || current.identity != self.identity
        {
            return Err(YCruncherFailure::ExecutableChanged);
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct YCruncherGeneration {
    pub generated_digits: u64,
    pub cache_write: Duration,
}

pub(crate) fn generate_to_target(
    cache: &PiCache,
    target_digits: u64,
    executable: &ValidatedYCruncher,
    workers: usize,
) -> Result<YCruncherGeneration, YCruncherFailure> {
    let existing = cache
        .digit_count()
        .map_err(|_| YCruncherFailure::ImportFailed)?;
    if existing >= target_digits {
        return Ok(YCruncherGeneration {
            generated_digits: 0,
            cache_write: Duration::ZERO,
        });
    }
    executable.revalidate()?;
    let temp_dir = std::env::temp_dir().join(format!(
        "pi-casso-y-cruncher-{}-{}",
        std::process::id(),
        uuid::Uuid::new_v4()
    ));
    fs::create_dir_all(&temp_dir).map_err(|_| YCruncherFailure::ProcessFailed)?;
    let status = Command::new(&executable.canonical_path)
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
        .arg(&temp_dir)
        .arg(format!("-TD:{}", workers.max(1)))
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map_err(|_| YCruncherFailure::ProcessFailed)?;
    if !status.success() {
        let _ = fs::remove_dir_all(&temp_dir);
        return Err(YCruncherFailure::ProcessFailed);
    }
    let imported = import_output(cache, &temp_dir, existing, target_digits);
    let _ = fs::remove_dir_all(&temp_dir);
    imported
}

fn import_output(
    cache: &PiCache,
    temp_dir: &Path,
    existing: u64,
    target_digits: u64,
) -> Result<YCruncherGeneration, YCruncherFailure> {
    let mut candidates = Vec::new();
    let mut entries = 0_usize;
    let mut bytes = 0_u64;
    let max_bytes = target_digits
        .saturating_mul(2)
        .saturating_add(16 * 1024 * 1024);
    collect_files(
        temp_dir,
        &mut candidates,
        0,
        &mut entries,
        &mut bytes,
        max_bytes,
    )
    .map_err(|_| YCruncherFailure::OutputInvalid)?;
    if candidates.is_empty() {
        return Err(YCruncherFailure::OutputInvalid);
    }
    let normalization_root =
        temp_dir.join(format!(".pi-casso-normalized-{}", uuid::Uuid::new_v4()));
    fs::create_dir(&normalization_root).map_err(|_| YCruncherFailure::OutputInvalid)?;
    for (index, candidate) in candidates.into_iter().enumerate() {
        let metadata =
            fs::symlink_metadata(&candidate).map_err(|_| YCruncherFailure::OutputInvalid)?;
        if !metadata.is_file() || metadata.file_type().is_symlink() {
            return Err(YCruncherFailure::OutputInvalid);
        }
        let source = FileDigitSource::new_with_options(candidate, true);
        if source.validate().is_err() || !source.len().is_ok_and(|digits| digits > existing) {
            continue;
        }
        let normalized = normalization_root.join(format!("normalized-{index}.digits"));
        if source.copy_digits_to(&normalized).is_err() {
            continue;
        }
        let started = Instant::now();
        let generated_digits = cache
            .append_from_validated_source(&normalized)
            .map_err(|_| YCruncherFailure::ImportFailed)?;
        return Ok(YCruncherGeneration {
            generated_digits,
            cache_write: started.elapsed(),
        });
    }
    Err(YCruncherFailure::OutputInvalid)
}

fn collect_files(
    directory: &Path,
    files: &mut Vec<PathBuf>,
    depth: usize,
    entries: &mut usize,
    bytes: &mut u64,
    max_bytes: u64,
) -> std::io::Result<()> {
    if depth > 16 {
        return Err(std::io::Error::other(
            "y-cruncher output nesting is too deep",
        ));
    }
    for entry in fs::read_dir(directory)? {
        let path = entry?.path();
        *entries = (*entries).saturating_add(1);
        if *entries > 1_024 {
            return Err(std::io::Error::other(
                "y-cruncher output has too many entries",
            ));
        }
        let metadata = fs::symlink_metadata(&path)?;
        if metadata.file_type().is_symlink() {
            return Err(std::io::Error::other(
                "y-cruncher output contains a symlink",
            ));
        }
        if metadata.is_dir() {
            collect_files(&path, files, depth + 1, entries, bytes, max_bytes)?;
        } else if metadata.is_file() {
            *bytes = bytes
                .checked_add(metadata.len())
                .ok_or_else(|| std::io::Error::other("y-cruncher output size overflowed"))?;
            if *bytes > max_bytes {
                return Err(std::io::Error::other("y-cruncher output is too large"));
            }
            files.push(path);
        } else {
            return Err(std::io::Error::other(
                "y-cruncher output contains a non-regular entry",
            ));
        }
    }
    files.sort();
    Ok(())
}

#[cfg(unix)]
fn executable_identity(metadata: &fs::Metadata) -> ExecutableIdentity {
    use std::os::unix::fs::MetadataExt;

    ExecutableIdentity {
        len: metadata.len(),
        device: metadata.dev(),
        inode: metadata.ino(),
    }
}

#[cfg(not(unix))]
fn executable_identity(metadata: &fs::Metadata) -> ExecutableIdentity {
    ExecutableIdentity {
        len: metadata.len(),
        modified: metadata.modified().ok(),
    }
}

#[cfg(unix)]
fn is_executable(metadata: &fs::Metadata) -> bool {
    use std::os::unix::fs::PermissionsExt;
    metadata.permissions().mode() & 0o111 != 0
}

#[cfg(not(unix))]
fn is_executable(_metadata: &fs::Metadata) -> bool {
    true
}

#[cfg(all(test, unix))]
mod tests {
    use std::fs;
    use std::os::unix::fs::{PermissionsExt, symlink};

    use tempfile::tempdir;

    use super::*;

    #[test]
    fn changed_validated_executable_is_rejected_before_launch() {
        // Given: an executable whose identity was validated and then replaced in place.
        let root = tempdir().expect("y-cruncher identity root");
        let executable = root.path().join("y-cruncher");
        fs::write(&executable, b"#!/bin/sh\nexit 0\n").expect("fixture executable");
        fs::set_permissions(&executable, fs::Permissions::from_mode(0o755))
            .expect("executable mode");
        let validated = ValidatedYCruncher::parse(&executable).expect("validated executable");
        fs::write(&executable, b"#!/bin/sh\nexit 7\n").expect("replace executable");
        let cache = PiCache::new(root.path().join("pi-cache.txt"));

        // When: execution revalidates the recorded identity.
        let failure = generate_to_target(&cache, 1, &validated, 1)
            .expect_err("changed executable must be rejected");

        // Then: no process is launched under the stale validation identity.
        assert!(matches!(failure, YCruncherFailure::ExecutableChanged));
    }

    #[test]
    fn output_import_rejects_symlinked_candidate_outside_temp_root() {
        // Given: y-cruncher output containing only a symlink to digits outside its root.
        let root = tempdir().expect("y-cruncher output root");
        let output = root.path().join("output");
        fs::create_dir(&output).expect("output directory");
        let outside = root.path().join("outside.txt");
        fs::write(&outside, b"3141592653589793").expect("outside digits");
        symlink(&outside, output.join("pi.txt")).expect("output symlink");
        let cache = PiCache::new(root.path().join("pi-cache.txt"));

        // When: output discovery evaluates the temporary tree.
        let failure =
            import_output(&cache, &output, 0, 16).expect_err("symlinked output must be rejected");

        // Then: import fails closed instead of following the external target.
        assert!(matches!(failure, YCruncherFailure::OutputInvalid));
        assert!(!cache.path().exists());
    }
}
