use std::process::{Command, Output};

use serde_json::Value;
use tempfile::TempDir;

fn benchmark(root: &TempDir) -> Command {
    let mut command = Command::new(assert_cmd::cargo::cargo_bin!("pi-casso"));
    command
        .env_remove("PI_CASSO_DATA_DIR")
        .env_remove("PI_CASSO_CONFIG")
        .env("PI_CASSO_TEST_MODE", "1")
        .env("XDG_DATA_HOME", root.path().join("data"))
        .env("XDG_CONFIG_HOME", root.path().join("config"))
        .env("TMPDIR", root.path().join("tmp"))
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
            "performance",
            "--backend",
            "cpu",
            "--gpu",
            "off",
            "--generator-backend",
            "cpu",
            "--cpu-workers",
            "2",
            "--chunk-size",
            "1",
            "--queue-depth",
            "2",
            "--memory-limit-mb",
            "64",
            "--show-metrics",
        ]);
    command
}

fn json(output: &Output) -> Value {
    serde_json::from_slice(&output.stdout).expect("command stdout is JSON")
}

#[test]
fn benchmark_reports_process_rss_and_explicit_unavailable_vram() {
    // Given: a CPU-only benchmark on the real command surface.
    let root = TempDir::new().expect("temporary benchmark root");

    // When: one measured repetition completes.
    let output = benchmark(&root).output().expect("benchmark starts");

    // Then: process RSS and the absence of measurable VRAM are explicit.
    assert!(output.status.success());
    let report = json(&output);
    let memory = &report["memory"];
    let baseline = memory["rss_baseline_mb"].as_f64().unwrap_or_default();
    let peak = memory["rss_peak_mb"].as_f64().unwrap_or_default();
    let margin = memory["rss_margin_mb"].as_f64().unwrap_or_default();
    assert!(baseline > 0.0);
    assert!(peak >= baseline);
    assert!(margin >= 0.0);
    assert_eq!(memory["gpu_vram_status"], "unavailable");
    assert_eq!(memory["gpu_vram_baseline_mb"], 0.0);
    assert_eq!(memory["gpu_vram_peak_mb"], 0.0);
    assert_eq!(memory["gpu_vram_margin_mb"], 0.0);
}

#[test]
fn stress_cpu_width_is_globally_leased() {
    // Given: one shared two-permit budget serving the CPU and optional GPU lanes.
    let root = TempDir::new().expect("temporary stress root");
    let output = Command::new(assert_cmd::cargo::cargo_bin!("pi-casso"))
        .env_remove("PI_CASSO_DATA_DIR")
        .env_remove("PI_CASSO_CONFIG")
        .env("PI_CASSO_TEST_MODE", "1")
        .env("XDG_DATA_HOME", root.path().join("data"))
        .env("XDG_CONFIG_HOME", root.path().join("config"))
        .env("TMPDIR", root.path().join("tmp"))
        .args([
            "--json",
            "stress-test",
            "--stress-target",
            "both",
            "--stress-duration",
            "1",
            "--backend",
            "auto",
            "--gpu",
            "auto",
            "--cpu-workers",
            "2",
            "--queue-depth",
            "2",
            "--memory-limit-mb",
            "64",
            "--yes",
        ])
        .output()
        .expect("stress starts");

    // When: the lanes finish against that command-owned budget.
    // Then: the peak lease equals the actual two-thread Rayon width.
    assert!(output.status.success());
    let report = json(&output);
    assert_eq!(report["aggregate"]["cpu_permits_max"], 2);
    assert_eq!(report["aggregate"]["cpu_permits_peak"], 2);
}

#[test]
fn source_open_failpoint_is_active_in_test_mode() {
    // Given: enough memory to reach source construction and an enabled test failpoint.
    let root = TempDir::new().expect("failpoint benchmark root");

    // When: the benchmark reaches the source-open boundary.
    let output = benchmark(&root)
        .env("PI_CASSO_TEST_FAIL_IF_SOURCE_OPEN", "1")
        .output()
        .expect("benchmark starts");

    // Then: the failpoint proves that boundary was reached.
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("PI_CASSO_TEST_FAIL_IF_SOURCE_OPEN"));
}

#[test]
fn source_open_failpoint_is_ignored_outside_test_mode() {
    // Given: the failpoint variable without the production test-mode gate.
    let root = TempDir::new().expect("production-mode benchmark root");

    // When: the same benchmark runs outside test mode.
    let output = benchmark(&root)
        .env_remove("PI_CASSO_TEST_MODE")
        .env("PI_CASSO_TEST_FAIL_IF_SOURCE_OPEN", "1")
        .output()
        .expect("benchmark starts");

    // Then: production ignores the test-only variable.
    assert!(
        output.status.success(),
        "benchmark failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}
