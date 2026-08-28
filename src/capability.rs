use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct GpuCapability {
    pub schema_version: u32,
    pub capability_state: String,
    pub available: bool,
    pub backend: String,
    pub device: String,
    pub driver: String,
    pub feature: String,
    pub reason: String,
    pub cuda_feature_enabled: bool,
    pub cuda_driver_loaded: bool,
    pub cuda_device_count: u32,
    pub cuda_available: bool,
    pub cuda_driver_version: String,
    pub cuda_device_compute_capability: String,
    pub cuda_ptx_compatible: bool,
    pub kernel_arch: String,
    pub kernel_sha256: String,
    pub kernel_source_sha256: String,
    pub kernel_load_status: String,
}

impl GpuCapability {
    pub fn detect() -> Self {
        if crate::gpu_ring::test_mock_enabled() {
            return Self::detect_with_filter(None);
        }
        #[cfg(feature = "cuda-native")]
        if crate::cuda::fake_preflight_enabled() {
            return crate::cuda::detect_capability();
        }
        #[cfg(feature = "gpu")]
        {
            Self::detect_with_filter(None)
        }
        #[cfg(all(not(feature = "gpu"), feature = "cuda-native"))]
        {
            crate::cuda::detect_capability()
        }
        #[cfg(all(not(feature = "gpu"), not(feature = "cuda-native")))]
        {
            Self::detect_with_filter(None)
        }
    }

    pub fn detect_with_filter(device_filter: Option<&str>) -> Self {
        #[cfg(not(feature = "gpu"))]
        let _ = device_filter;
        if crate::gpu_ring::test_mock_enabled() {
            return Self {
                schema_version: 1,
                capability_state: "preflight_ok".to_string(),
                available: true,
                backend: "wgpu".to_string(),
                device: "test-only mock wgpu".to_string(),
                driver: "test-only mock".to_string(),
                feature: "gpu".to_string(),
                reason: String::new(),
                ..Self::unavailable("")
            };
        }
        let adapters = crate::gpu::list_adapters();
        let adapter = adapters.first();
        #[cfg(feature = "gpu")]
        let preflight = crate::gpu::GpuSearchEngine::new(device_filter);
        #[cfg(not(feature = "gpu"))]
        let preflight: anyhow::Result<crate::gpu::GpuSearchEngine> =
            Err(anyhow::anyhow!("GPU support was disabled at compile time"));

        match preflight {
            Ok(engine) => Self {
                schema_version: 1,
                capability_state: "preflight_ok".to_string(),
                available: true,
                backend: "wgpu".to_string(),
                device: engine.device_name().to_string(),
                driver: adapter.map_or_else(String::new, |info| info.driver.clone()),
                feature: "gpu".to_string(),
                reason: String::new(),
                ..Self::unavailable("")
            },
            Err(_) => {
                let reason = if adapters.is_empty() {
                    "adapter_unavailable"
                } else {
                    "pipeline_preflight_unavailable"
                };
                let mut capability = Self::unavailable(reason);
                if let Some(info) = adapter {
                    capability.device.clone_from(&info.name);
                    capability.driver.clone_from(&info.driver);
                }
                capability
            }
        }
    }

    pub fn unavailable(reason: &str) -> Self {
        Self {
            schema_version: 1,
            capability_state: "unavailable".to_string(),
            available: false,
            backend: "wgpu".to_string(),
            device: String::new(),
            driver: String::new(),
            feature: if cfg!(feature = "gpu") {
                "gpu".to_string()
            } else {
                "not_compiled".to_string()
            },
            reason: reason.to_string(),
            cuda_feature_enabled: false,
            cuda_driver_loaded: false,
            cuda_device_count: 0,
            cuda_available: false,
            cuda_driver_version: String::new(),
            cuda_device_compute_capability: String::new(),
            cuda_ptx_compatible: false,
            kernel_arch: String::new(),
            kernel_sha256: String::new(),
            kernel_source_sha256: String::new(),
            kernel_load_status: "not_attempted".to_string(),
        }
    }

    pub fn cuda_unavailable(reason: &str, kernel_load_status: &str) -> Self {
        Self {
            backend: "cuda".to_string(),
            feature: if cfg!(feature = "cuda-native") {
                "cuda-native".to_string()
            } else {
                "not_compiled".to_string()
            },
            cuda_feature_enabled: cfg!(feature = "cuda-native"),
            kernel_arch: String::new(),
            kernel_load_status: kernel_load_status.to_string(),
            ..Self::unavailable(reason)
        }
    }

    pub fn probe_exit_code(&self) -> i32 {
        if matches!(
            self.reason.as_str(),
            "artifact_handoff_invalid" | "kernel_load_failed"
        ) {
            1
        } else {
            0
        }
    }

    pub fn record_runtime_fault(&mut self, reason: &str) {
        self.capability_state = "runtime_fault".to_string();
        self.available = true;
        self.reason = reason.to_string();
    }
}
