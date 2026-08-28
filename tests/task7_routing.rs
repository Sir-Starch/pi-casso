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

fn json(output: &std::process::Output) -> Value {
    serde_json::from_slice(&output.stdout).expect("command stdout is JSON")
}

#[test]
fn explicit_wgpu_benchmark_runs_after_successful_preflight_or_skips_with_capability_reason() {
    // Given: the capability probe and a nonzero benchmark with one warm-up and two measured runs.
    let root = TempDir::new().expect("temporary benchmark root");
    let capability_output = isolated_command(&root)
        .args(["--json", "gpu", "info"])
        .output()
        .expect("gpu info starts");
    assert!(
        capability_output.status.success(),
        "gpu info failed: {}",
        String::from_utf8_lossy(&capability_output.stderr)
    );
    let capability = json(&capability_output);
    let benchmark = isolated_command(&root)
        .args([
            "--json",
            "benchmark",
            "--template",
            "arch",
            "--source-mode",
            "finite",
            "--cache-state",
            "warm",
            "--work-windows",
            "4096",
            "--repetitions",
            "2",
            "--warmup",
            "1",
            "--profile",
            "balanced",
            "--backend",
            "gpu",
            "--gpu",
            "on",
            "--generator-backend",
            "cpu",
            "--cpu-workers",
            "1",
            "--chunk-size",
            "256",
            "--queue-depth",
            "1",
            "--memory-limit-mb",
            "128",
            "--show-metrics",
        ])
        .output()
        .expect("benchmark starts");
    let report = json(&benchmark);

    // When: the real CLI routes the request through the capability-aware contract.
    // Then: a usable device produces nonzero measured GPU work; otherwise the result is an
    // explicit, capability-derived unsupported response with no fake comparison data.
    if capability["capability_state"] == "preflight_ok" {
        assert!(
            benchmark.status.success(),
            "GPU benchmark failed: {}",
            String::from_utf8_lossy(&benchmark.stderr)
        );
        assert_eq!(report["status"], "ok");
        assert_eq!(report["requested_backend"], "wgpu");
        assert_eq!(report["resolved_backend"], "wgpu");
        assert_eq!(
            report["gpu"]["capability"]["capability_state"],
            "preflight_ok"
        );
        assert_eq!(report["scanned_windows"], 4096);
        assert_eq!(report["warm_up_completed"], true);
        assert_eq!(report["raw_runs"].as_array().map(Vec::len), Some(2));
        assert!(
            report["p95"]["scanned_windows_per_second"]
                .as_f64()
                .is_some_and(|rate| rate > 0.0)
        );
        assert!(
            report["gpu"]["submissions"]
                .as_u64()
                .is_some_and(|count| count > 0)
        );
        assert!(
            report["gpu"]["completions"]
                .as_u64()
                .is_some_and(|count| count > 0)
        );
    } else {
        assert_eq!(benchmark.status.code(), Some(2));
        assert_eq!(report["status"], "unsupported");
        assert_eq!(report["requested_backend"], "wgpu");
        assert!(report["resolved_backend"].is_null());
        assert_eq!(report["reason"], capability["reason"]);
        assert_ne!(report["reason"], "accelerator_execution_deferred");
        assert_eq!(report["scanned_windows"], 0);
        assert!(report["raw_runs"].as_array().is_some_and(Vec::is_empty));
    }
}
