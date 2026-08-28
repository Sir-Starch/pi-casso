use std::collections::BTreeMap;

use anyhow::{Result, anyhow, bail};
use serde::Serialize;
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::capability::GpuCapability;
use crate::cli::{BenchmarkArgs, BenchmarkCacheState, BenchmarkSourceMode};
use crate::performance::{GeneratorBackendChoice, GpuMode, SearchBackendChoice};

pub const AUTO_MIN_WORK_WINDOWS: u64 = 4_096;

#[derive(Clone, Debug, Serialize)]
pub struct WorkloadIdentity {
    pub template: String,
    pub match_mode: String,
    pub canvas_width: usize,
    pub canvas_height: usize,
    pub target_width: usize,
    pub target_height: usize,
    pub target_bitmap_sha256: String,
    pub start_offset: u64,
    pub work_windows: u64,
    pub max_offset: i64,
    pub chunk_size: usize,
    pub source_mode: String,
    pub cache_state: String,
    pub profile: String,
    pub requested_backend: String,
    pub gpu_mode: String,
    pub gpu_device: String,
    pub generator_backend: String,
    pub selected_variant: String,
    pub y_cruncher_path_present: bool,
    pub y_cruncher_executable_sha256: String,
    pub cpu_workers: usize,
    pub cpu_utilization: u8,
    pub queue_depth: usize,
    pub memory_limit_mb: usize,
}

impl WorkloadIdentity {
    pub fn workload_id(&self) -> Result<String> {
        self.prefixed_workload_id("bench-v1")
    }

    pub fn pi_workload_id(&self) -> Result<String> {
        self.prefixed_workload_id("pi-v1")
    }

    fn prefixed_workload_id(&self, prefix: &str) -> Result<String> {
        let value = serde_json::to_value(self)?;
        let Value::Object(object) = value else {
            bail!("benchmark identity did not serialize as an object");
        };
        let sorted: BTreeMap<_, _> = object.into_iter().collect();
        let canonical = serde_json::to_vec(&sorted)?;
        let digest = Sha256::digest(canonical);
        Ok(format!("{prefix}-{digest:x}"))
    }
}

#[derive(Clone, Debug)]
pub struct BenchmarkBounds {
    pub work_windows: u64,
    pub max_offset: Option<u64>,
    pub effective_end: u64,
    pub source_end_exclusive: u64,
}

impl BenchmarkBounds {
    pub fn parse(args: &BenchmarkArgs, chunk_size: usize, window_len: usize) -> Result<Self> {
        let work_windows = match args.work_windows {
            Some(value) => value,
            None => (chunk_size as u64)
                .checked_mul(args.seconds)
                .ok_or_else(|| anyhow!("seconds compatibility work budget overflowed"))?,
        };
        let max_offset = match args.max_offset {
            Some(value) if value < 0 => bail!("max_offset must be a nonnegative integer"),
            Some(value) => Some(u64::try_from(value)?),
            None => None,
        };
        let count_end = args
            .start_offset
            .checked_add(work_windows)
            .ok_or_else(|| anyhow!("start_offset plus work_windows overflowed"))?;
        let effective_end = max_offset.map_or(count_end, |cap| cap.min(count_end));
        let source_end_exclusive = if effective_end <= args.start_offset {
            args.start_offset
        } else {
            effective_end
                .checked_add(u64::try_from(window_len.saturating_sub(1))?)
                .ok_or_else(|| anyhow!("source_end_exclusive overflowed"))?
        };
        Ok(Self {
            work_windows,
            max_offset,
            effective_end,
            source_end_exclusive,
        })
    }

    pub const fn scanned_windows(&self, start_offset: u64) -> u64 {
        self.effective_end.saturating_sub(start_offset)
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct BackendCandidate {
    pub backend: &'static str,
    pub status: &'static str,
    pub eligible: bool,
    pub reason: &'static str,
}

impl BackendCandidate {
    const fn selected(backend: &'static str) -> Self {
        Self {
            backend,
            status: "selected",
            eligible: true,
            reason: "",
        }
    }

    const fn skipped(backend: &'static str, reason: &'static str) -> Self {
        Self {
            backend,
            status: "skipped",
            eligible: false,
            reason,
        }
    }
}

#[derive(Clone, Debug)]
pub struct BackendResolution {
    pub status: &'static str,
    pub requested: &'static str,
    pub resolved: Option<&'static str>,
    pub gpu_mode: &'static str,
    pub fallback: bool,
    pub reason: String,
    pub auto_min_work_windows: u64,
    pub backend_candidates: Vec<BackendCandidate>,
}

#[derive(Clone, Copy, Debug)]
pub enum WgpuPreflight {
    NotProbed,
    Eligible,
    Unavailable(&'static str),
}

#[derive(Clone, Copy, Debug)]
pub enum CudaPreflight {
    NotProbed,
    Eligible,
    Unavailable(&'static str),
}

impl CudaPreflight {
    const fn unavailable_reason(self) -> Option<&'static str> {
        match self {
            Self::NotProbed => Some("cuda_preflight_not_performed"),
            Self::Eligible => None,
            Self::Unavailable(reason) => Some(reason),
        }
    }
}

pub fn cuda_preflight(capability: &GpuCapability) -> CudaPreflight {
    if capability.capability_state == "preflight_ok" {
        return CudaPreflight::Eligible;
    }
    CudaPreflight::Unavailable(match capability.reason.as_str() {
        "cuda_not_compiled" => "cuda_not_compiled",
        "artifact_handoff_missing" => "artifact_handoff_missing",
        "artifact_handoff_invalid" => "artifact_handoff_invalid",
        "driver_unavailable" => "driver_unavailable",
        "device_unavailable" => "device_unavailable",
        "unsupported_compute_capability" => "unsupported_compute_capability",
        "kernel_load_failed" => "kernel_load_failed",
        _ => "cuda_preflight_unavailable",
    })
}

impl WgpuPreflight {
    const fn unavailable_reason(self) -> Option<&'static str> {
        match self {
            Self::NotProbed => Some("wgpu_preflight_not_performed"),
            Self::Eligible => None,
            Self::Unavailable(reason) => Some(reason),
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct BackendPreflightRequest {
    pub backend: Option<SearchBackendChoice>,
    pub gpu: Option<GpuMode>,
    pub effective_work_windows: u64,
    pub cuda: CudaPreflight,
    pub wgpu: WgpuPreflight,
}

pub fn resolve_backend_preflight(request: BackendPreflightRequest) -> BackendResolution {
    let matrix = match (request.backend, request.gpu) {
        (None, None | Some(GpuMode::Auto))
        | (Some(SearchBackendChoice::Auto), None | Some(GpuMode::Auto)) => Ok(("auto", "auto")),
        (None, Some(GpuMode::Off))
        | (Some(SearchBackendChoice::Cpu), None | Some(GpuMode::Off)) => Ok(("cpu", "off")),
        (None, Some(GpuMode::On)) | (Some(SearchBackendChoice::Gpu), None | Some(GpuMode::On)) => {
            Ok(("wgpu", "on"))
        }
        (Some(SearchBackendChoice::Cuda), None | Some(GpuMode::On)) => Ok(("cuda", "on")),
        _ => Err("backend and gpu selections are inconsistent"),
    };
    let (requested, gpu_mode) = match matrix {
        Ok(value) => value,
        Err(reason) => return BackendResolution::error("selection_error", reason),
    };
    match requested {
        "cpu" => BackendResolution::explicit_cpu(gpu_mode),
        "auto" => {
            let (reason, backend_candidates) = if request.effective_work_windows
                < AUTO_MIN_WORK_WINDOWS
            {
                (
                    "auto_threshold_cpu",
                    vec![
                        BackendCandidate::skipped(
                            "cuda",
                            "below_auto_min_work_windows_before_capability_probe",
                        ),
                        BackendCandidate::skipped(
                            "wgpu",
                            "below_auto_min_work_windows_before_capability_probe",
                        ),
                        BackendCandidate::selected("cpu"),
                    ],
                )
            } else {
                let mut candidates = Vec::with_capacity(3);
                if let Some(reason) = request.cuda.unavailable_reason() {
                    candidates.push(BackendCandidate::skipped("cuda", reason));
                } else {
                    candidates.push(BackendCandidate::selected("cuda"));
                    return BackendResolution::auto_cuda(candidates);
                }
                if let Some(reason) = request.wgpu.unavailable_reason() {
                    candidates.push(BackendCandidate::skipped("wgpu", reason));
                    candidates.push(BackendCandidate::selected("cpu"));
                    return BackendResolution::auto_cpu("auto_cpu_capability_fallback", candidates);
                }
                candidates.push(BackendCandidate::selected("wgpu"));
                return BackendResolution::auto_wgpu("auto_wgpu_capability_fallback", candidates);
            };
            BackendResolution::auto_cpu(reason, backend_candidates)
        }
        "wgpu" => match request.wgpu.unavailable_reason() {
            Some(reason) => BackendResolution::unsupported(requested, gpu_mode, reason),
            None => BackendResolution::explicit_wgpu(requested, gpu_mode),
        },
        "cuda" => match request.cuda.unavailable_reason() {
            Some(reason) => BackendResolution::unsupported(requested, gpu_mode, reason),
            None => BackendResolution::explicit_cuda(requested, gpu_mode),
        },
        _ => BackendResolution::error("selection_error", "unknown backend selection"),
    }
}

impl BackendResolution {
    fn explicit_cpu(gpu_mode: &'static str) -> Self {
        Self {
            status: "ok",
            requested: "cpu",
            resolved: Some("cpu"),
            gpu_mode,
            fallback: false,
            reason: String::new(),
            auto_min_work_windows: AUTO_MIN_WORK_WINDOWS,
            backend_candidates: vec![BackendCandidate::selected("cpu")],
        }
    }

    fn auto_cpu(reason: &'static str, backend_candidates: Vec<BackendCandidate>) -> Self {
        Self {
            status: "ok",
            requested: "auto",
            resolved: Some("cpu"),
            gpu_mode: "auto",
            fallback: true,
            reason: reason.to_string(),
            auto_min_work_windows: AUTO_MIN_WORK_WINDOWS,
            backend_candidates,
        }
    }

    fn auto_wgpu(reason: &'static str, backend_candidates: Vec<BackendCandidate>) -> Self {
        Self {
            status: "ok",
            requested: "auto",
            resolved: Some("wgpu"),
            gpu_mode: "auto",
            fallback: true,
            reason: reason.to_string(),
            auto_min_work_windows: AUTO_MIN_WORK_WINDOWS,
            backend_candidates,
        }
    }

    fn auto_cuda(backend_candidates: Vec<BackendCandidate>) -> Self {
        Self {
            status: "ok",
            requested: "auto",
            resolved: Some("cuda"),
            gpu_mode: "auto",
            fallback: false,
            reason: String::new(),
            auto_min_work_windows: AUTO_MIN_WORK_WINDOWS,
            backend_candidates,
        }
    }

    fn explicit_wgpu(requested: &'static str, gpu_mode: &'static str) -> Self {
        Self {
            status: "ok",
            requested,
            resolved: Some("wgpu"),
            gpu_mode,
            fallback: false,
            reason: String::new(),
            auto_min_work_windows: AUTO_MIN_WORK_WINDOWS,
            backend_candidates: vec![BackendCandidate::selected("wgpu")],
        }
    }

    fn explicit_cuda(requested: &'static str, gpu_mode: &'static str) -> Self {
        Self {
            status: "ok",
            requested,
            resolved: Some("cuda"),
            gpu_mode,
            fallback: false,
            reason: String::new(),
            auto_min_work_windows: AUTO_MIN_WORK_WINDOWS,
            backend_candidates: vec![BackendCandidate::selected("cuda")],
        }
    }

    fn unsupported(requested: &'static str, gpu_mode: &'static str, reason: &'static str) -> Self {
        Self {
            status: "unsupported",
            requested,
            resolved: None,
            gpu_mode,
            fallback: false,
            reason: reason.to_string(),
            auto_min_work_windows: AUTO_MIN_WORK_WINDOWS,
            backend_candidates: vec![BackendCandidate::skipped(requested, reason)],
        }
    }

    fn error(status: &'static str, reason: &str) -> Self {
        Self {
            status,
            requested: "",
            resolved: None,
            gpu_mode: "",
            fallback: false,
            reason: reason.to_string(),
            auto_min_work_windows: AUTO_MIN_WORK_WINDOWS,
            backend_candidates: Vec::new(),
        }
    }
}

pub const fn resolved_generator(backend: GeneratorBackendChoice) -> &'static str {
    match backend {
        GeneratorBackendChoice::Cpu | GeneratorBackendChoice::Auto => "cpu",
        GeneratorBackendChoice::YCruncher => "y-cruncher-external",
    }
}

pub const fn source_mode(mode: BenchmarkSourceMode) -> &'static str {
    mode.as_str()
}

pub const fn cache_state(state: BenchmarkCacheState) -> &'static str {
    state.as_str()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_json_hash_vector_matches_contract() {
        let canonical = br#"{"a":1,"b":2}"#;
        assert_eq!(
            format!("{:x}", Sha256::digest(canonical)),
            "43258cff783fe7036d8a43033f830adfc60ec037382473548ac742b888292777"
        );
    }
}
