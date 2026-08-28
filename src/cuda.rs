use cudarc::driver::{CudaContext, sys};
use cudarc::nvrtc::Ptx;

use crate::capability::GpuCapability;
use crate::cuda_artifact::{ArtifactState, VerifiedCudaArtifacts};

pub(crate) fn detect_capability() -> GpuCapability {
    let verified = match crate::cuda_artifact::inspect() {
        ArtifactState::Missing => {
            return GpuCapability::cuda_unavailable("artifact_handoff_missing", "not_attempted");
        }
        ArtifactState::Invalid(_detail) => {
            return GpuCapability::cuda_unavailable("artifact_handoff_invalid", "failed");
        }
        ArtifactState::Verified(verified) => verified,
    };
    if fake_preflight_enabled() {
        return fake_capability(&verified);
    }
    real_capability(verified)
}

pub(crate) fn fake_execution_enabled() -> bool {
    crate::gpu_ring::test_mode_enabled()
        && std::env::var_os("PI_CASSO_TEST_FAKE_CUDA_PREFLIGHT").is_some()
        && std::env::var_os("PI_CASSO_TEST_FAKE_CUDA_EXECUTION").is_some()
}

pub(crate) fn fake_preflight_enabled() -> bool {
    crate::gpu_ring::test_mode_enabled()
        && std::env::var_os("PI_CASSO_TEST_FAKE_CUDA_PREFLIGHT").is_some()
}

fn fake_capability(verified: &VerifiedCudaArtifacts) -> GpuCapability {
    GpuCapability {
        schema_version: 1,
        capability_state: "preflight_ok".to_string(),
        available: true,
        backend: "cuda".to_string(),
        device: "test-only mock CUDA compute capability 8.9".to_string(),
        driver: "test-only mock CUDA driver".to_string(),
        feature: "cuda-native".to_string(),
        reason: String::new(),
        cuda_feature_enabled: true,
        cuda_driver_loaded: true,
        cuda_device_count: 1,
        cuda_available: true,
        cuda_driver_version: "test-only".to_string(),
        cuda_device_compute_capability: "8.9".to_string(),
        cuda_ptx_compatible: true,
        kernel_arch: verified.architecture.clone(),
        kernel_sha256: verified.artifact_sha256.clone(),
        kernel_source_sha256: verified.source_sha256.clone(),
        kernel_load_status: "ok".to_string(),
    }
}

fn real_capability(verified: VerifiedCudaArtifacts) -> GpuCapability {
    let count = match CudaContext::device_count() {
        Ok(count) => count,
        Err(_) => return unavailable_with_artifact("driver_unavailable", &verified),
    };
    let count_u32 = u32::try_from(count).unwrap_or_default();
    if count <= 0 {
        let mut capability = unavailable_with_artifact("device_unavailable", &verified);
        capability.cuda_driver_loaded = true;
        capability.cuda_device_count = count_u32;
        capability.cuda_driver_version = driver_version().unwrap_or_default();
        return capability;
    }
    let context = match CudaContext::new(0) {
        Ok(context) => context,
        Err(_) => return unavailable_with_artifact("device_unavailable", &verified),
    };
    let device = context
        .name()
        .unwrap_or_else(|_| "CUDA device 0".to_string());
    let compute = match context.compute_capability() {
        Ok(compute) => compute,
        Err(_) => return unavailable_with_artifact("device_unavailable", &verified),
    };
    let compute_capability = format!("{}.{}", compute.0, compute.1);
    let module = match context.load_module(Ptx::from_src(verified.ptx.clone())) {
        Ok(module) => module,
        Err(_) => {
            return failed_kernel_capability(
                device,
                count_u32,
                compute_capability,
                false,
                &verified,
            );
        }
    };
    if module.load_function("emergence").is_err() {
        return failed_kernel_capability(device, count_u32, compute_capability, true, &verified);
    }
    GpuCapability {
        schema_version: 1,
        capability_state: "preflight_ok".to_string(),
        available: true,
        backend: "cuda".to_string(),
        device,
        driver: "CUDA Driver API".to_string(),
        feature: "cuda-native".to_string(),
        reason: String::new(),
        cuda_feature_enabled: true,
        cuda_driver_loaded: true,
        cuda_device_count: count_u32,
        cuda_available: true,
        cuda_driver_version: driver_version().unwrap_or_default(),
        cuda_device_compute_capability: compute_capability,
        cuda_ptx_compatible: true,
        kernel_arch: verified.architecture,
        kernel_sha256: verified.artifact_sha256,
        kernel_source_sha256: verified.source_sha256,
        kernel_load_status: "ok".to_string(),
    }
}

fn unavailable_with_artifact(reason: &str, verified: &VerifiedCudaArtifacts) -> GpuCapability {
    let mut capability = GpuCapability::cuda_unavailable(reason, "not_attempted");
    capability.kernel_arch.clone_from(&verified.architecture);
    capability
        .kernel_sha256
        .clone_from(&verified.artifact_sha256);
    capability
        .kernel_source_sha256
        .clone_from(&verified.source_sha256);
    capability
}

fn failed_kernel_capability(
    device: String,
    count: u32,
    compute_capability: String,
    ptx_compatible: bool,
    verified: &VerifiedCudaArtifacts,
) -> GpuCapability {
    let mut capability = unavailable_with_artifact("kernel_load_failed", verified);
    capability.device = device;
    capability.cuda_driver_loaded = true;
    capability.cuda_device_count = count;
    capability.cuda_driver_version = driver_version().unwrap_or_default();
    capability.cuda_device_compute_capability = compute_capability;
    capability.cuda_ptx_compatible = ptx_compatible;
    capability.kernel_load_status = "failed".to_string();
    capability
}

fn driver_version() -> Result<String, cudarc::driver::DriverError> {
    cudarc::driver::result::init()?;
    let mut version = 0_i32;
    // SAFETY: Category 8, FFI boundary. The initialized CUDA driver writes one
    // integer to the valid exclusive pointer supplied for cuDriverGetVersion.
    unsafe { sys::cuDriverGetVersion(&raw mut version).result()? };
    Ok(version.to_string())
}
