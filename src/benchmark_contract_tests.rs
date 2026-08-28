use std::collections::HashSet;

use crate::benchmark_contract::{
    BackendPreflightRequest, CudaPreflight, WgpuPreflight, WorkloadIdentity,
    resolve_backend_preflight,
};
use crate::performance::{GpuMode, SearchBackendChoice};

fn baseline_identity() -> WorkloadIdentity {
    WorkloadIdentity {
        template: "arch".to_string(),
        match_mode: "emergence".to_string(),
        canvas_width: 24,
        canvas_height: 24,
        target_width: 12,
        target_height: 12,
        target_bitmap_sha256: "79e5a3df8a1b624af9fc288b62f692962368b8ce4cec34a2e900811597b3f2ca"
            .to_string(),
        start_offset: 0,
        work_windows: 2,
        max_offset: -1,
        chunk_size: 2,
        source_mode: "finite".to_string(),
        cache_state: "cold".to_string(),
        profile: "eco".to_string(),
        requested_backend: "cpu".to_string(),
        gpu_mode: "off".to_string(),
        gpu_device: "auto".to_string(),
        generator_backend: "cpu".to_string(),
        selected_variant: "chudnovsky-rug-binary-split".to_string(),
        y_cruncher_path_present: false,
        y_cruncher_executable_sha256: String::new(),
        cpu_workers: 1,
        cpu_utilization: 25,
        queue_depth: 1,
        memory_limit_mb: 64,
    }
}

#[test]
fn benchmark_workload_hash_contract() {
    // Given: the canonical Task 1 workload identity and its fixed digest.
    let baseline = baseline_identity();
    let baseline_id = baseline.workload_id().expect("baseline identity hashes");
    assert_eq!(
        baseline_id,
        "bench-v1-49dba4c267f587b825f571b0b44355ae9a8114f027705a024c4e05d1588e240e"
    );

    macro_rules! changed {
        ($field:ident, $value:expr) => {{
            let mut identity = baseline.clone();
            identity.$field = $value;
            identity
        }};
    }
    let mutations = vec![
        changed!(template, "pi".to_string()),
        changed!(match_mode, "threshold".to_string()),
        changed!(canvas_width, 25),
        changed!(canvas_height, 25),
        changed!(target_width, 13),
        changed!(target_height, 13),
        changed!(target_bitmap_sha256, "a".repeat(64)),
        changed!(start_offset, 1),
        changed!(work_windows, 3),
        changed!(max_offset, 10),
        changed!(chunk_size, 3),
        changed!(source_mode, "growing".to_string()),
        changed!(cache_state, "warm".to_string()),
        changed!(profile, "performance".to_string()),
        changed!(requested_backend, "auto".to_string()),
        changed!(gpu_mode, "auto".to_string()),
        changed!(gpu_device, "device-1".to_string()),
        changed!(generator_backend, "spigot-persistent".to_string()),
        changed!(selected_variant, "spigot-persistent".to_string()),
        changed!(y_cruncher_path_present, true),
        changed!(y_cruncher_executable_sha256, "b".repeat(64)),
        changed!(cpu_workers, 2),
        changed!(cpu_utilization, 26),
        changed!(queue_depth, 2),
        changed!(memory_limit_mb, 65),
    ];

    // When: every canonical field is independently mutated.
    let mutated_ids: HashSet<_> = mutations
        .iter()
        .map(|identity| identity.workload_id().expect("mutated identity hashes"))
        .collect();

    // Then: each mutation has a distinct bench-v1 identity.
    assert_eq!(mutated_ids.len(), mutations.len());
    assert!(mutated_ids.iter().all(|identity| identity != &baseline_id));
}

#[test]
fn backend_preflight_preserves_presence_and_status_discrimination() {
    // Given: omitted selection, an explicit CPU selection, and an inconsistent pair.
    let omitted = BackendPreflightRequest {
        backend: None,
        gpu: None,
        effective_work_windows: 1,
        cuda: CudaPreflight::Eligible,
        wgpu: WgpuPreflight::Eligible,
    };
    let explicit_cpu = BackendPreflightRequest {
        backend: Some(SearchBackendChoice::Cpu),
        gpu: Some(GpuMode::Off),
        effective_work_windows: 4_096,
        cuda: CudaPreflight::NotProbed,
        wgpu: WgpuPreflight::NotProbed,
    };
    let inconsistent = BackendPreflightRequest {
        backend: Some(SearchBackendChoice::Gpu),
        gpu: Some(GpuMode::Off),
        effective_work_windows: 4_096,
        cuda: CudaPreflight::NotProbed,
        wgpu: WgpuPreflight::NotProbed,
    };

    // When: all requests cross the shared preflight boundary.
    let omitted_result = resolve_backend_preflight(omitted);
    let cpu_result = resolve_backend_preflight(explicit_cpu);
    let error_result = resolve_backend_preflight(inconsistent);

    // Then: omission is resolved only at the boundary and errors have no resolved backend.
    assert_eq!(omitted_result.status, "ok");
    assert_eq!(omitted_result.requested, "auto");
    assert_eq!(omitted_result.resolved, Some("cpu"));
    assert_eq!(omitted_result.reason, "auto_threshold_cpu");
    assert_eq!(omitted_result.backend_candidates.len(), 3);
    assert!(
        omitted_result.backend_candidates[..2]
            .iter()
            .all(|candidate| {
                candidate.status == "skipped"
                    && !candidate.eligible
                    && candidate.reason == "below_auto_min_work_windows_before_capability_probe"
            })
    );
    assert_eq!(omitted_result.backend_candidates[2].backend, "cpu");
    assert_eq!(omitted_result.backend_candidates[2].status, "selected");
    assert_eq!(cpu_result.requested, "cpu");
    assert_eq!(cpu_result.resolved, Some("cpu"));
    assert!(!cpu_result.fallback);
    assert!(cpu_result.reason.is_empty());
    assert_eq!(cpu_result.auto_min_work_windows, 4_096);
    assert_eq!(cpu_result.backend_candidates.len(), 1);
    assert_eq!(cpu_result.backend_candidates[0].backend, "cpu");
    assert_eq!(cpu_result.backend_candidates[0].status, "selected");
    assert!(cpu_result.backend_candidates[0].eligible);
    assert_eq!(error_result.status, "selection_error");
    assert_eq!(error_result.resolved, None);
    assert!(!error_result.fallback);
    assert!(!error_result.reason.is_empty());
    assert_eq!(error_result.auto_min_work_windows, 4_096);
    assert!(error_result.backend_candidates.is_empty());
}

#[test]
fn auto_backend_at_threshold_reports_unavailable_accelerators_before_cpu() {
    // Given: an auto-selected workload at the accelerator threshold without an eligible device.
    let request = BackendPreflightRequest {
        backend: Some(SearchBackendChoice::Auto),
        gpu: Some(GpuMode::Auto),
        effective_work_windows: 4_096,
        cuda: CudaPreflight::Unavailable("cuda_not_compiled"),
        wgpu: WgpuPreflight::Unavailable("pipeline_preflight_unavailable"),
    };

    // When: the shared preflight resolves the ordered candidates.
    let result = resolve_backend_preflight(request);

    // Then: unavailable accelerators are skipped and CPU is the explained fallback.
    assert_eq!(result.status, "ok");
    assert_eq!(result.resolved, Some("cpu"));
    assert!(result.fallback);
    assert_eq!(result.reason, "auto_cpu_capability_fallback");
    assert_eq!(result.auto_min_work_windows, 4_096);
    assert_eq!(result.backend_candidates.len(), 3);
    assert_eq!(result.backend_candidates[0].backend, "cuda");
    assert_eq!(result.backend_candidates[0].status, "skipped");
    assert!(!result.backend_candidates[0].eligible);
    assert_eq!(result.backend_candidates[0].reason, "cuda_not_compiled");
    assert_eq!(result.backend_candidates[1].backend, "wgpu");
    assert_eq!(result.backend_candidates[1].status, "skipped");
    assert!(!result.backend_candidates[1].eligible);
    assert_eq!(
        result.backend_candidates[1].reason,
        "pipeline_preflight_unavailable"
    );
    assert_eq!(result.backend_candidates[2].backend, "cpu");
    assert_eq!(result.backend_candidates[2].status, "selected");
    assert!(result.backend_candidates[2].eligible);
}

#[test]
fn eligible_accelerator_preflight_preserves_cuda_first_auto_order() {
    // Given: explicit wgpu plus auto requests with eligible CUDA or wgpu capability.
    let explicit = BackendPreflightRequest {
        backend: Some(SearchBackendChoice::Gpu),
        gpu: Some(GpuMode::On),
        effective_work_windows: 4_096,
        cuda: CudaPreflight::NotProbed,
        wgpu: WgpuPreflight::Eligible,
    };
    let automatic_cuda = BackendPreflightRequest {
        backend: Some(SearchBackendChoice::Auto),
        gpu: Some(GpuMode::Auto),
        effective_work_windows: 4_096,
        cuda: CudaPreflight::Eligible,
        wgpu: WgpuPreflight::Eligible,
    };
    let automatic_wgpu = BackendPreflightRequest {
        cuda: CudaPreflight::Unavailable("cuda_not_compiled"),
        ..automatic_cuda
    };

    // When: the shared preflight resolves each request.
    let explicit_result = resolve_backend_preflight(explicit);
    let cuda_result = resolve_backend_preflight(automatic_cuda);
    let wgpu_result = resolve_backend_preflight(automatic_wgpu);

    // Then: explicit wgpu is unchanged and auto selects the first eligible candidate.
    assert_eq!(explicit_result.status, "ok");
    assert_eq!(explicit_result.resolved, Some("wgpu"));
    assert!(!explicit_result.fallback);
    assert_eq!(explicit_result.backend_candidates[0].status, "selected");
    assert_eq!(cuda_result.resolved, Some("cuda"));
    assert!(!cuda_result.fallback);
    assert_eq!(cuda_result.backend_candidates.len(), 1);
    assert_eq!(cuda_result.backend_candidates[0].backend, "cuda");
    assert_eq!(cuda_result.backend_candidates[0].status, "selected");
    assert_eq!(wgpu_result.resolved, Some("wgpu"));
    assert!(wgpu_result.fallback);
    assert_eq!(wgpu_result.reason, "auto_wgpu_capability_fallback");
    assert_eq!(wgpu_result.backend_candidates[0].backend, "cuda");
    assert_eq!(
        wgpu_result.backend_candidates[0].reason,
        "cuda_not_compiled"
    );
    assert_eq!(wgpu_result.backend_candidates[1].backend, "wgpu");
    assert_eq!(wgpu_result.backend_candidates[1].status, "selected");
}

#[test]
fn explicit_cuda_preflight_remains_strict() {
    // Given: explicit CUDA requests with eligible and unavailable capability states.
    let request = BackendPreflightRequest {
        backend: Some(SearchBackendChoice::Cuda),
        gpu: Some(GpuMode::On),
        effective_work_windows: 1,
        cuda: CudaPreflight::Eligible,
        wgpu: WgpuPreflight::Eligible,
    };

    // When: both requests cross the same preflight boundary.
    let eligible = resolve_backend_preflight(request);
    let unavailable = resolve_backend_preflight(BackendPreflightRequest {
        cuda: CudaPreflight::Unavailable("cuda_not_compiled"),
        ..request
    });

    // Then: CUDA is selected strictly or rejected without falling through to wgpu/CPU.
    assert_eq!(eligible.resolved, Some("cuda"));
    assert!(!eligible.fallback);
    assert_eq!(unavailable.status, "unsupported");
    assert_eq!(unavailable.resolved, None);
    assert_eq!(unavailable.reason, "cuda_not_compiled");
    assert_eq!(unavailable.backend_candidates.len(), 1);
    assert_eq!(unavailable.backend_candidates[0].backend, "cuda");
}

#[test]
fn explicit_wgpu_without_preflight_is_structured_unsupported() {
    // Given: an explicit GPU request whose capability was not probed.
    let request = BackendPreflightRequest {
        backend: Some(SearchBackendChoice::Gpu),
        gpu: Some(GpuMode::On),
        effective_work_windows: 4_096,
        cuda: CudaPreflight::NotProbed,
        wgpu: WgpuPreflight::NotProbed,
    };

    // When: selection reaches the capability-aware contract.
    let result = resolve_backend_preflight(request);

    // Then: it cannot infer capability from configuration alone.
    assert_eq!(result.status, "unsupported");
    assert_eq!(result.resolved, None);
    assert_eq!(result.reason, "wgpu_preflight_not_performed");
}
