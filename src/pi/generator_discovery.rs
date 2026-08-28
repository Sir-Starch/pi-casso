use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};

use anyhow::{Result, anyhow};
use sha2::{Digest, Sha256};

use super::generator_backend::GeneratorVariant;
use super::y_cruncher::ValidatedYCruncher;

pub(super) fn forced_variant() -> Result<Option<GeneratorVariant>> {
    if !crate::gpu_ring::test_mode_enabled() {
        return Ok(None);
    }
    let Some(value) = std::env::var_os("PI_CASSO_TEST_GENERATOR_VARIANT") else {
        return Ok(None);
    };
    let value = value
        .into_string()
        .map_err(|_| anyhow!("invalid_test_generator_variant"))?;
    GeneratorVariant::parse(&value)
        .map(Some)
        .ok_or_else(|| anyhow!("invalid_test_generator_variant"))
}

pub(super) fn test_y_cruncher_path() -> Option<PathBuf> {
    crate::gpu_ring::test_mode_enabled()
        .then(|| std::env::var_os("PI_CASSO_TEST_YCRUNCHER_PATH"))
        .flatten()
        .map(PathBuf::from)
}

pub(super) fn discover_y_cruncher() -> Option<ValidatedYCruncher> {
    std::env::var_os("PATH")
        .into_iter()
        .flat_map(|path| std::env::split_paths(&path).collect::<Vec<PathBuf>>())
        .map(|directory| directory.join("y-cruncher"))
        .find_map(|candidate| ValidatedYCruncher::parse(&candidate).ok())
}

pub(super) fn current_executable_sha256() -> Result<String> {
    #[cfg(target_os = "linux")]
    {
        sha256_file(Path::new("/proc/self/exe"))
    }

    #[cfg(not(target_os = "linux"))]
    {
        let path =
            std::env::current_exe().map_err(|_| anyhow!("executable_identity_unavailable"))?;
        sha256_file(&path)
    }
}

fn sha256_file(path: &Path) -> Result<String> {
    let mut executable =
        File::open(path).map_err(|_| anyhow!("executable_identity_unavailable"))?;
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = executable
            .read(&mut buffer)
            .map_err(|_| anyhow!("executable_identity_unavailable"))?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use std::fs;
    use std::process::Command;

    use tempfile::tempdir;

    use super::current_executable_sha256;

    #[test]
    fn running_executable_identity_survives_path_replacement() {
        if std::env::var_os("PI_CASSO_TEST_HASH_DELETED_SELF").is_some() {
            fs::remove_file(std::env::current_exe().expect("child executable path"))
                .expect("remove copied child executable");
            assert_eq!(
                current_executable_sha256()
                    .expect("running executable hash")
                    .len(),
                64
            );
            return;
        }

        let directory = tempdir().expect("temporary executable directory");
        let executable = directory.path().join("identity-probe");
        fs::copy(
            std::env::current_exe().expect("test executable path"),
            &executable,
        )
        .expect("copy test executable");
        let output = Command::new(executable)
            .env("PI_CASSO_TEST_HASH_DELETED_SELF", "1")
            .args([
                "--exact",
                "pi::generator_discovery::tests::running_executable_identity_survives_path_replacement",
                "--nocapture",
            ])
            .output()
            .expect("identity probe starts");
        assert!(
            output.status.success(),
            "identity probe failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}
