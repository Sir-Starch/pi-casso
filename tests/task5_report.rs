use std::process::{Command, Output};

use serde_json::Value;
use tempfile::TempDir;

fn benchmark(root: &TempDir) -> Output {
    Command::new(assert_cmd::cargo::cargo_bin!("pi-casso"))
        .env_remove("PI_CASSO_DATA_DIR")
        .env_remove("PI_CASSO_CONFIG")
        .env("XDG_DATA_HOME", root.path().join("data"))
        .env("XDG_CONFIG_HOME", root.path().join("config"))
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
            "3",
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
            "4",
            "--memory-limit-mb",
            "64",
            "--show-metrics",
        ])
        .output()
        .expect("benchmark starts")
}

#[test]
fn one_repetition_reports_actual_reader_pool_telemetry() {
    // Given: one benchmark repetition with a two-reader pool and three range reads.
    let root = TempDir::new().expect("temporary benchmark root");

    // When: the real CLI emits its flat version-1 report.
    let output = benchmark(&root);
    assert!(
        output.status.success(),
        "benchmark failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report: Value = serde_json::from_slice(&output.stdout).expect("benchmark stdout is JSON");

    // Then: source counters come from that search session, not repetition count.
    assert_eq!(report["repetitions"], 1);
    assert_eq!(report["config"]["cpu_workers"], 2);
    assert_eq!(report["config"]["queue_depth"], 4);
    assert_eq!(report["source"]["reader_pool_size"], 2);
    assert_eq!(report["source"]["reader_open_count"], 2);
    assert_eq!(report["source"]["reader_reuse_count"], 2);
    assert!(report["stage_timings"]["read_ms"].as_u64().is_some());
    assert!(report["stage_timings"]["parse_ms"].as_u64().is_some());
    assert!(report["source"]["cache_hit_ms"].as_u64().is_some());
}
