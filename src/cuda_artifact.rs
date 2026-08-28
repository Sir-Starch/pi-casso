use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use serde::Deserialize;
use sha2::{Digest, Sha256};

const SOURCE_PATH: &str = "kernels/cuda/emergence.cu";
const ARTIFACT_PATH: &str = "kernels/cuda/emergence.ptx";
const README_PATH: &str = "kernels/cuda/README.md";
const MANIFEST_PATH: &str = "kernels/cuda/handoff.json";

#[derive(Debug)]
pub(crate) enum ArtifactState {
    Missing,
    Invalid(String),
    Verified(VerifiedCudaArtifacts),
}

#[derive(Clone, Debug)]
pub(crate) struct VerifiedCudaArtifacts {
    pub(crate) architecture: String,
    pub(crate) source_sha256: String,
    pub(crate) artifact_sha256: String,
    pub(crate) ptx: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct HandoffManifest {
    schema_version: u32,
    owner: String,
    source_path: String,
    artifact_path: String,
    architecture: String,
    toolchain: String,
    nvcc_command: String,
    source_sha256: String,
    artifact_sha256: String,
    designated_host: DesignatedHost,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DesignatedHost {
    gpu: String,
    driver: String,
    toolkit: String,
}

pub(crate) fn inspect() -> ArtifactState {
    let root = artifact_root();
    let manifest_path = root.join(MANIFEST_PATH);
    let readme_path = root.join(README_PATH);
    let source_path = root.join(SOURCE_PATH);
    let artifact_path = root.join(ARTIFACT_PATH);
    let paths = [&manifest_path, &readme_path, &source_path, &artifact_path];
    if paths.iter().any(|path| !path.exists()) {
        return ArtifactState::Missing;
    }
    if paths.iter().any(|path| {
        fs::symlink_metadata(path).map_or(true, |metadata| !metadata.file_type().is_file())
    }) {
        return ArtifactState::Invalid(
            "CUDA handoff paths must be regular files inside the selected artifact root"
                .to_string(),
        );
    }
    match verify(&manifest_path, &readme_path, &source_path, &artifact_path) {
        Ok(verified) => ArtifactState::Verified(verified),
        Err(reason) => ArtifactState::Invalid(reason),
    }
}

pub(crate) fn verified() -> Result<VerifiedCudaArtifacts, String> {
    match inspect() {
        ArtifactState::Verified(verified) => Ok(verified),
        ArtifactState::Missing => Err("CUDA artifact handoff is missing".to_string()),
        ArtifactState::Invalid(reason) => Err(reason),
    }
}

fn artifact_root() -> PathBuf {
    if crate::gpu_ring::test_mode_enabled() {
        if let Some(root) = std::env::var_os("PI_CASSO_TEST_CUDA_ARTIFACT_ROOT") {
            return PathBuf::from(root);
        }
    }
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn verify(
    manifest_path: &Path,
    readme_path: &Path,
    source_path: &Path,
    artifact_path: &Path,
) -> Result<VerifiedCudaArtifacts, String> {
    let manifest_bytes = fs::read(manifest_path).map_err(display_error)?;
    let manifest: HandoffManifest =
        serde_json::from_slice(&manifest_bytes).map_err(display_error)?;
    validate_manifest(&manifest)?;
    let readme = fs::read_to_string(readme_path).map_err(display_error)?;
    let metadata = machine_metadata(&readme)?;
    let source = fs::read(source_path).map_err(display_error)?;
    let artifact = fs::read(artifact_path).map_err(display_error)?;
    let source_sha256 = digest(&source);
    let artifact_sha256 = digest(&artifact);
    require_metadata(&metadata, "source_sha256", &source_sha256)?;
    require_metadata(&metadata, "artifact_sha256", &artifact_sha256)?;
    require_metadata(&metadata, "toolchain", &manifest.toolchain)?;
    require_metadata(&metadata, "architecture", &manifest.architecture)?;
    if manifest.source_sha256 != source_sha256 || manifest.artifact_sha256 != artifact_sha256 {
        return Err("manifest hashes do not match the supplied source and artifact".to_string());
    }
    let ptx = String::from_utf8(artifact).map_err(display_error)?;
    if !ptx_target_matches(&ptx, &manifest.architecture) || !ptx.contains(".entry emergence") {
        return Err(
            "PTX does not declare the handoff architecture and emergence kernel".to_string(),
        );
    }
    Ok(VerifiedCudaArtifacts {
        architecture: manifest.architecture,
        source_sha256,
        artifact_sha256,
        ptx,
    })
}

fn validate_manifest(manifest: &HandoffManifest) -> Result<(), String> {
    if manifest.schema_version != 1
        || manifest.owner.trim().is_empty()
        || manifest.source_path != SOURCE_PATH
        || manifest.artifact_path != ARTIFACT_PATH
        || !is_cuda_architecture(&manifest.architecture)
        || !manifest.nvcc_command.starts_with("nvcc ")
        || !manifest
            .nvcc_command
            .contains(&format!("--gpu-architecture={}", manifest.architecture))
        || !manifest
            .nvcc_command
            .contains(&format!("-o {}", ARTIFACT_PATH))
        || manifest.designated_host.gpu.trim().is_empty()
        || manifest.designated_host.driver.trim().is_empty()
        || manifest.designated_host.toolkit.trim().is_empty()
        || !is_sha256(&manifest.source_sha256)
        || !is_sha256(&manifest.artifact_sha256)
    {
        return Err("handoff manifest does not satisfy schema version 1".to_string());
    }
    Ok(())
}

fn is_cuda_architecture(value: &str) -> bool {
    value.strip_prefix("compute_").is_some_and(|suffix| {
        !suffix.is_empty() && suffix.bytes().all(|byte| byte.is_ascii_digit())
    })
}

fn ptx_target_matches(ptx: &str, architecture: &str) -> bool {
    let Some(capability) = architecture.strip_prefix("compute_") else {
        return false;
    };
    let expected = format!(".target sm_{capability}");
    ptx.lines().any(|line| {
        line.split(',')
            .next()
            .is_some_and(|target| target.trim() == expected)
    })
}

fn machine_metadata(readme: &str) -> Result<BTreeMap<&str, &str>, String> {
    let mut metadata = BTreeMap::new();
    for line in readme.lines() {
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        if matches!(
            key,
            "source_sha256" | "artifact_sha256" | "toolchain" | "architecture"
        ) && metadata.insert(key, value).is_some()
        {
            return Err(format!("duplicate README metadata key {key}"));
        }
    }
    Ok(metadata)
}

fn require_metadata(
    metadata: &BTreeMap<&str, &str>,
    key: &str,
    expected: &str,
) -> Result<(), String> {
    if metadata.get(key).copied() != Some(expected) {
        return Err(format!("README metadata {key} does not match the handoff"));
    }
    Ok(())
}

fn is_sha256(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn display_error(error: impl std::fmt::Display) -> String {
    error.to_string()
}
