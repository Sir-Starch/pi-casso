#![cfg(feature = "cuda-native")]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use serde_json::Value;
use sha2::{Digest, Sha256};
use tempfile::TempDir;

struct CudaFixture {
    root: TempDir,
    artifact_root: PathBuf,
}

impl CudaFixture {
    fn create(mode: &str) -> Self {
        let root = TempDir::new().expect("temporary CUDA fixture root");
        let artifact_root = root.path().join(mode);
        let output = Command::new("scripts/create-cuda-handoff-fixture.sh")
            .args(["--mode", mode, "--output"])
            .arg(&artifact_root)
            .output()
            .expect("CUDA fixture script starts");
        assert!(
            output.status.success(),
            "fixture creation failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        Self {
            root,
            artifact_root,
        }
    }

    fn command(&self) -> Command {
        let runtime_root = self.root.path().join("runtime");
        let mut command = Command::new(assert_cmd::cargo::cargo_bin!("pi-casso"));
        command
            .env_remove("PI_CASSO_DATA_DIR")
            .env_remove("PI_CASSO_CONFIG")
            .env("PI_CASSO_TEST_MODE", "1")
            .env("PI_CASSO_TEST_FAKE_CUDA_PREFLIGHT", "1")
            .env("PI_CASSO_TEST_CUDA_ARTIFACT_ROOT", &self.artifact_root)
            .env("XDG_DATA_HOME", runtime_root.join("data"))
            .env("XDG_CONFIG_HOME", runtime_root.join("config"))
            .env("TMPDIR", runtime_root.join("tmp"));
        command
    }
}

fn json(output: &Output) -> Value {
    serde_json::from_slice(&output.stdout).expect("command stdout is JSON")
}

fn benchmark_args(work_windows: &'static str) -> [&'static str; 30] {
    [
        "--json",
        "benchmark",
        "--source-mode",
        "finite",
        "--cache-state",
        "cold",
        "--template",
        "arch",
        "--work-windows",
        work_windows,
        "--repetitions",
        "1",
        "--warmup",
        "0",
        "--profile",
        "performance",
        "--backend",
        "cuda",
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
    ]
}

fn sha256(path: &Path) -> String {
    let bytes = fs::read(path).expect("fixture file is readable");
    format!("{:x}", Sha256::digest(bytes))
}

#[test]
fn cuda_artifact_fixture_matrix() {
    // Given: isolated missing and corrupt artifact roots.
    let missing = CudaFixture::create("missing");
    let corrupt = CudaFixture::create("corrupt");

    // When: gpu info reaches the artifact decision before source/cache work.
    let missing_output = missing
        .command()
        .env("PI_CASSO_TEST_FAIL_IF_SOURCE_OPEN", "1")
        .args(["--json", "gpu", "info"])
        .output()
        .expect("missing handoff probe starts");
    let corrupt_output = corrupt
        .command()
        .env("PI_CASSO_TEST_FAIL_IF_SOURCE_OPEN", "1")
        .args(["--json", "gpu", "info"])
        .output()
        .expect("corrupt handoff probe starts");

    // Then: absence is an exit-zero skip while present corruption is an integrity failure.
    assert_eq!(missing_output.status.code(), Some(0));
    let missing_report = json(&missing_output);
    assert_eq!(missing_report["capability_state"], "unavailable");
    assert_eq!(missing_report["reason"], "artifact_handoff_missing");
    assert_eq!(missing_report["kernel_load_status"], "not_attempted");
    assert_eq!(corrupt_output.status.code(), Some(1));
    let corrupt_report = json(&corrupt_output);
    assert_eq!(corrupt_report["capability_state"], "unavailable");
    assert_eq!(corrupt_report["reason"], "artifact_handoff_invalid");
    assert_eq!(corrupt_report["kernel_load_status"], "failed");
}

#[test]
fn cuda_valid_fixture_reports_hashes_and_mock_backend() {
    // Given: a hash-consistent isolated artifact handoff and fake CUDA execution.
    let fixture = CudaFixture::create("valid");
    let source = fixture.artifact_root.join("kernels/cuda/emergence.cu");
    let artifact = fixture.artifact_root.join("kernels/cuda/emergence.ptx");

    // When: the explicit CUDA benchmark runs through the real CLI schema.
    let output = fixture
        .command()
        .env("PI_CASSO_TEST_FAKE_CUDA_EXECUTION", "1")
        .args(benchmark_args("4096"))
        .output()
        .expect("valid fake CUDA benchmark starts");

    // Then: the report carries only fixture hashes and is unambiguously test-only.
    assert!(
        output.status.success(),
        "benchmark failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report = json(&output);
    assert_eq!(report["resolved_backend"], "cuda");
    assert_eq!(report["backend_feature_available"], true);
    assert_eq!(report["gpu"]["kernel_arch"], "compute_89");
    assert_eq!(report["gpu"]["kernel_sha256"], sha256(&artifact));
    assert_eq!(report["gpu"]["kernel_source_sha256"], sha256(&source));
    assert_eq!(report["gpu"]["test_only_mock"], true);
    assert!(
        report["scanned_windows"]
            .as_u64()
            .is_some_and(|value| value > 0)
    );
}

#[test]
fn cuda_explicit_unavailable_fails_before_work() {
    // Given: explicit CUDA selection with a missing handoff and a source-open tripwire.
    let fixture = CudaFixture::create("missing");

    // When: benchmark selection performs strict CUDA preflight.
    let output = fixture
        .command()
        .env("PI_CASSO_TEST_FAIL_IF_SOURCE_OPEN", "1")
        .args(benchmark_args("4096"))
        .output()
        .expect("strict CUDA benchmark starts");

    // Then: it exits two as unsupported without scanning or falling back.
    assert_eq!(output.status.code(), Some(2));
    let report = json(&output);
    assert_eq!(report["status"], "unsupported");
    assert_eq!(report["requested_backend"], "cuda");
    assert_eq!(report["resolved_backend"], Value::Null);
    assert_eq!(report["scanned_windows"], 0);
    assert_eq!(
        report["gpu"]["capability"]["reason"],
        "artifact_handoff_missing"
    );
}

#[test]
fn cuda_search_runtime_fault_records_mixed() {
    // Given: two chunks with one completed mock CUDA chunk before an injected runtime fault.
    let fixture = CudaFixture::create("valid");
    let output = fixture
        .command()
        .env("PI_CASSO_TEST_FAKE_CUDA_EXECUTION", "1")
        .env("PI_CASSO_TEST_BACKEND_FAIL_AFTER_PREFLIGHT", "cuda")
        .args(benchmark_args("8192"))
        .output()
        .expect("CUDA fallback benchmark starts");
    assert!(
        output.status.success(),
        "fallback benchmark failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report = json(&output);

    // When: the second chunk is retried on CPU.
    // Then: fallback is exactly once, resolution is mixed, and shared budgets remain observable.
    assert_eq!(report["resolved_backend"], "mixed");
    assert_eq!(report["fallback_count"], 1);
    assert_eq!(report["gpu"]["fallback_count"], 1);
    assert_eq!(report["gpu"]["test_only_mock"], true);
    assert!(
        report["gpu"]["completions"]
            .as_u64()
            .is_some_and(|value| value > 0)
    );
    assert_eq!(report["scanned_windows"], 8192);
    assert!(report["queue"]["permits"].as_u64().is_some());
    assert!(report["queue"]["max_occupancy"].as_u64().is_some());
    assert!(report["memory"]["logical_peak_mb"].as_f64().is_some());
    assert!(report["cpu_permits_max"].as_u64().is_some());
    assert_eq!(
        report["gpu"]["capability"]["capability_state"],
        "runtime_fault"
    );
    assert_eq!(report["gpu"]["capability"]["available"], true);
    assert_eq!(report["gpu"]["capability"]["kernel_load_status"], "ok");
}

#[test]
fn cuda_stress_runtime_fault_is_lane_error() {
    // Given: the plan's host-independent explicit CUDA stress invocation.
    let fixture = CudaFixture::create("valid");
    let output = fixture
        .command()
        .env("PI_CASSO_TEST_FAKE_CUDA_EXECUTION", "1")
        .env("PI_CASSO_TEST_STRESS_RUNTIME_FAULT", "cuda")
        .args([
            "--json",
            "stress-test",
            "--stress-target",
            "gpu",
            "--stress-duration",
            "2",
            "--backend",
            "cuda",
            "--gpu",
            "on",
            "--cpu-workers",
            "2",
            "--queue-depth",
            "2",
            "--memory-limit-mb",
            "512",
            "--yes",
        ])
        .output()
        .expect("CUDA stress command starts");

    // When: execution faults after successful preflight.
    // Then: it is a typed lane failure with no ordinary-search fallback.
    assert_eq!(output.status.code(), Some(1));
    let report = json(&output);
    assert_eq!(report["status"], "error");
    assert_eq!(report["aggregate"]["status"], "error");
    let lane = &report["lanes"][0];
    assert_eq!(lane["status"], "runtime_fault");
    assert_eq!(lane["capability"]["capability_state"], "runtime_fault");
    assert_eq!(lane["capability"]["available"], true);
    assert_eq!(lane["capability"]["kernel_load_status"], "ok");
    assert_eq!(lane["fallback"], false);
    assert_eq!(lane["test_only_mock"], true);
    assert_ne!(lane["resolved_backend"], "mixed");
}
