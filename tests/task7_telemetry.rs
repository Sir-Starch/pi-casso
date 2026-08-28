use std::process::Command;

use serde_json::Value;
use tempfile::TempDir;

fn isolated_command(root: &TempDir) -> Command {
    let mut command = Command::new(assert_cmd::cargo::cargo_bin!("pi-casso"));
    command
        .env_remove("PI_CASSO_DATA_DIR")
        .env_remove("PI_CASSO_CONFIG")
        .env("XDG_DATA_HOME", root.path().join("data"))
        .env("XDG_CONFIG_HOME", root.path().join("config"))
        .env("TMPDIR", root.path().join("tmp"));
    command
}

fn benchmark(root: &TempDir, work_windows: &str, chunk_size: &str) -> Command {
    let mut command = isolated_command(root);
    command.args([
        "--json",
        "benchmark",
        "--template",
        "arch",
        "--source-mode",
        "finite",
        "--cache-state",
        "cold",
        "--work-windows",
        work_windows,
        "--repetitions",
        "1",
        "--warmup",
        "0",
        "--profile",
        "performance",
        "--backend",
        "gpu",
        "--gpu",
        "on",
        "--generator-backend",
        "cpu",
        "--cpu-workers",
        "1",
        "--chunk-size",
        chunk_size,
        "--queue-depth",
        "1",
        "--memory-limit-mb",
        "128",
        "--show-metrics",
    ]);
    command
}

fn gpu_is_available(root: &TempDir) -> bool {
    let output = isolated_command(root)
        .args(["--json", "gpu", "info"])
        .output()
        .expect("gpu info starts");
    assert!(
        output.status.success(),
        "gpu info failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report: Value = serde_json::from_slice(&output.stdout).expect("gpu info is JSON");
    report["capability_state"] == "preflight_ok"
}

#[test]
fn benchmark_snapshot_reports_driver_resource_reuse_and_zero_without_reuse() {
    let root = TempDir::new().expect("temporary benchmark root");
    if !gpu_is_available(&root) {
        eprintln!("SKIP: wgpu adapter/device/pipeline preflight unavailable");
        return;
    }

    let control = benchmark(&root, "256", "256")
        .output()
        .expect("control benchmark starts");
    let reused = benchmark(&root, "512", "256")
        .output()
        .expect("reuse benchmark starts");
    assert!(
        control.status.success(),
        "control benchmark failed: {}",
        String::from_utf8_lossy(&control.stderr)
    );
    assert!(
        reused.status.success(),
        "reuse benchmark failed: {}",
        String::from_utf8_lossy(&reused.stderr)
    );
    let control_report: Value = serde_json::from_slice(&control.stdout).expect("control is JSON");
    let reused_report: Value = serde_json::from_slice(&reused.stdout).expect("reuse is JSON");

    assert_eq!(control_report["gpu"]["resource_reuses"], 0);
    assert_eq!(control_report["gpu"]["buffer_creations"], 6);
    assert_eq!(control_report["gpu"]["bind_group_creations"], 1);
    assert_eq!(reused_report["gpu"]["resource_reuses"], 1);
    assert_eq!(reused_report["gpu"]["buffer_creations"], 6);
    assert_eq!(reused_report["gpu"]["bind_group_creations"], 1);
    assert_eq!(reused_report["raw_runs"][0]["gpu_resource_reuses"], 1);
}
