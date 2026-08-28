#![cfg(not(debug_assertions))]

use std::process::Command;

use serde_json::Value;
use tempfile::TempDir;

#[test]
fn release_binary_ignores_production_test_environment_seams() {
    // Given: every production seam is armed against a small valid CPU benchmark.
    let root = TempDir::new().expect("release seam root");
    let output = Command::new(assert_cmd::cargo::cargo_bin!("pi-casso"))
        .env_remove("PI_CASSO_DATA_DIR")
        .env_remove("PI_CASSO_CONFIG")
        .env("XDG_DATA_HOME", root.path().join("data"))
        .env("XDG_CONFIG_HOME", root.path().join("config"))
        .env("TMPDIR", root.path().join("tmp"))
        .env("PI_CASSO_TEST_MODE", "1")
        .env("PI_CASSO_TEST_GENERATOR_VARIANT", "invalid-release-variant")
        .env(
            "PI_CASSO_TEST_YCRUNCHER_PATH",
            "/missing/release/y-cruncher",
        )
        .env("PI_CASSO_TEST_FAIL_IF_SOURCE_OPEN", "1")
        .env("PI_CASSO_TEST_MIN_RESERVATION_BYTES", u64::MAX.to_string())
        .env("PI_CASSO_TEST_CONSUMER_DELAY_MS", "60000")
        .env("PI_CASSO_TEST_CACHE_CRASH_PHASE", "after_raw_sync")
        .env("PI_CASSO_TEST_STORAGE_FAIL_PHASE", "before_commit")
        .env("PI_CASSO_TEST_FAKE_WGPU_PREFLIGHT", "1")
        .env("PI_CASSO_TEST_FAKE_WGPU_EXECUTION", "1")
        .env("PI_CASSO_TEST_BACKEND_FAIL_AFTER_PREFLIGHT", "wgpu")
        .env("PI_CASSO_TEST_STRESS_RUNTIME_FAULT", "wgpu")
        .env("PI_CASSO_TEST_GPU_COMPLETION_DELAY_MS", "60000")
        .env("PI_CASSO_TEST_FORCE_CAPABILITY", "wgpu-unavailable")
        .env("PI_CASSO_TEST_CUDA_ARTIFACT_ROOT", "/missing/release/cuda")
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
            "1",
            "--repetitions",
            "1",
            "--warmup",
            "0",
            "--profile",
            "eco",
            "--backend",
            "cpu",
            "--gpu",
            "off",
            "--generator-backend",
            "cpu",
            "--cpu-workers",
            "1",
            "--chunk-size",
            "1",
            "--queue-depth",
            "1",
            "--memory-limit-mb",
            "64",
        ])
        .output()
        .expect("release benchmark starts");

    // When/Then: release behavior stays on the ordinary CPU path and succeeds promptly.
    assert!(
        output.status.success(),
        "release benchmark failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report: Value = serde_json::from_slice(&output.stdout).expect("benchmark JSON");
    assert_eq!(report["status"], "ok");
    assert_eq!(report["resolved_backend"], "cpu");
    assert_eq!(report["fallback"], false);
    assert_eq!(report["gpu"]["test_only_mock"], false);
}
