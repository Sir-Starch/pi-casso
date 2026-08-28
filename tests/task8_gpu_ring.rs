use std::process::{Command, Output};

use serde_json::Value;
use tempfile::TempDir;

fn isolated_command(root: &TempDir) -> Command {
    let mut command = Command::new(assert_cmd::cargo::cargo_bin!("pi-casso"));
    command
        .env_remove("PI_CASSO_DATA_DIR")
        .env_remove("PI_CASSO_CONFIG")
        .env("PI_CASSO_TEST_MODE", "1")
        .env("PI_CASSO_TEST_FAKE_WGPU_PREFLIGHT", "1")
        .env("PI_CASSO_TEST_FAKE_WGPU_EXECUTION", "1")
        .env("XDG_DATA_HOME", root.path().join("data"))
        .env("XDG_CONFIG_HOME", root.path().join("config"))
        .env("TMPDIR", root.path().join("tmp"));
    command
}

fn benchmark(root: &TempDir, work_windows: &str, chunk_size: &str, depth: &str) -> Output {
    isolated_command(root)
        .env("PI_CASSO_TEST_GPU_COMPLETION_DELAY_MS", "2")
        .args([
            "--json",
            "benchmark",
            "--template",
            "arch",
            "--source-mode",
            "finite",
            "--cache-state",
            "cold",
            "--seconds",
            "10",
            "--work-windows",
            work_windows,
            "--repetitions",
            "1",
            "--warmup",
            "0",
            "--profile",
            "max",
            "--backend",
            "gpu",
            "--gpu",
            "on",
            "--generator-backend",
            "cpu",
            "--cpu-workers",
            "2",
            "--chunk-size",
            chunk_size,
            "--queue-depth",
            depth,
            "--memory-limit-mb",
            "512",
            "--show-metrics",
        ])
        .output()
        .expect("mock GPU benchmark starts")
}

fn report(output: &Output) -> Value {
    assert!(
        output.status.success(),
        "command failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).expect("command stdout is JSON")
}

#[test]
fn gpu_ring_overlap_scheduler_mock() {
    // Given: the plan's host-independent 64-chunk workload at ring depths 1, 2, and 4.
    let reports = ["1", "2", "4"].map(|depth| {
        let root = TempDir::new().expect("temporary benchmark root");
        report(&benchmark(&root, "262144", "4096", depth))
    });

    // When: the mock completion scheduler executes every chunk through the bounded ring.
    // Then: depth one stays serial while depths two and four expose real delayed overlap.
    for (index, depth) in [1_u64, 2, 4].into_iter().enumerate() {
        let gpu = &reports[index]["gpu"];
        assert_eq!(gpu["test_only_mock"], true);
        assert!(gpu["submissions"].as_u64().is_some_and(|value| value >= 8));
        assert_eq!(gpu["submissions"], gpu["completions"]);
        assert!(
            gpu["max_in_flight"]
                .as_u64()
                .is_some_and(|value| value >= 1 && value <= depth)
        );
        if depth == 1 {
            assert_eq!(gpu["max_in_flight"], 1);
            assert_eq!(gpu["overlap_events"], 0);
        } else {
            assert!(gpu["max_in_flight"].as_u64().is_some_and(|value| value > 1));
            assert!(
                gpu["overlap_events"]
                    .as_u64()
                    .is_some_and(|value| value > 0)
            );
            assert!(gpu["overlap_ms"].as_u64().is_some_and(|value| value > 0));
        }
        assert!(
            reports[index]["queue"]["max_occupancy"]
                .as_u64()
                .is_some_and(|value| value <= depth)
        );
    }
}

#[test]
fn gpu_async_completion_failure_falls_back_once() {
    // Given: one successful mock-wgpu chunk followed by the post-preflight failure.
    let root = TempDir::new().expect("temporary fallback root");
    let output = isolated_command(&root)
        .env("PI_CASSO_TEST_BACKEND_FAIL_AFTER_PREFLIGHT", "wgpu")
        .args([
            "--json",
            "benchmark",
            "--template",
            "arch",
            "--source-mode",
            "finite",
            "--cache-state",
            "cold",
            "--work-windows",
            "8192",
            "--repetitions",
            "1",
            "--warmup",
            "0",
            "--profile",
            "max",
            "--backend",
            "gpu",
            "--gpu",
            "on",
            "--generator-backend",
            "cpu",
            "--cpu-workers",
            "2",
            "--chunk-size",
            "4096",
            "--queue-depth",
            "2",
            "--memory-limit-mb",
            "512",
            "--show-metrics",
        ])
        .output()
        .expect("fallback benchmark starts");
    let report = report(&output);

    // When: the outstanding mock work drains and the affected chunk is retried on CPU.
    // Then: fallback occurs exactly once and ordered reduction advances through both chunks.
    assert_eq!(report["gpu"]["test_only_mock"], true);
    assert_eq!(report["fallback"], true);
    assert_eq!(report["fallback_count"], 1);
    assert_eq!(report["gpu"]["fallback_count"], 1);
    assert!(
        report["gpu"]["completions"]
            .as_u64()
            .is_some_and(|value| value > 0)
    );
    assert_eq!(report["scanned_windows"], 8192);
    assert_eq!(report["reducer"]["ordered"], true);
    assert_eq!(
        report["reducer"]["contiguous_completed_offsets"],
        report["scanned_windows"]
    );
    assert_eq!(
        report["gpu"]["capability"]["capability_state"],
        "runtime_fault"
    );
    assert_eq!(report["gpu"]["capability"]["available"], true);
    assert_eq!(
        report["gpu"]["capability"]["kernel_load_status"],
        "not_attempted"
    );
}

#[test]
fn stress_both_gpu_runtime_fault_is_lane_error() {
    // Given: a shared-budget CPU/GPU stress run with a mock GPU runtime fault.
    let root = TempDir::new().expect("temporary stress root");
    let output = isolated_command(&root)
        .env("PI_CASSO_TEST_STRESS_RUNTIME_FAULT", "wgpu")
        .args([
            "--json",
            "stress-test",
            "--stress-target",
            "both",
            "--stress-duration",
            "2",
            "--backend",
            "auto",
            "--gpu",
            "auto",
            "--cpu-workers",
            "2",
            "--queue-depth",
            "2",
            "--memory-limit-mb",
            "512",
            "--yes",
        ])
        .output()
        .expect("stress command starts");

    // When: the GPU lane faults after capability preflight.
    // Then: aggregate exit is one, CPU remains one independent lane, and no mixed fallback appears.
    assert_eq!(output.status.code(), Some(1));
    let report: Value = serde_json::from_slice(&output.stdout).expect("stress stdout is JSON");
    let lanes = report["lanes"].as_array().expect("lane array");
    assert_eq!(lanes.iter().filter(|lane| lane["lane"] == "cpu").count(), 1);
    let gpu = lanes
        .iter()
        .find(|lane| lane["lane"] == "gpu")
        .expect("GPU lane");
    assert_eq!(report["aggregate"]["status"], "error");
    assert_eq!(gpu["status"], "runtime_fault");
    assert_eq!(gpu["capability"]["capability_state"], "runtime_fault");
    assert_eq!(gpu["capability"]["available"], true);
    assert_eq!(gpu["capability"]["kernel_load_status"], "not_attempted");
    assert_eq!(gpu["fallback"], false);
    assert_eq!(gpu["test_only_mock"], true);
    assert_ne!(gpu["resolved_backend"], "mixed");
    assert!(
        gpu["reason"]
            .as_str()
            .is_some_and(|reason| reason.contains("post-preflight execution failure"))
    );
}
