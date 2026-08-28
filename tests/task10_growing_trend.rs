use std::process::Command;

use serde_json::Value;
use tempfile::TempDir;

#[test]
fn growing_warm_cache_generator_wait_falls_while_resource_bounds_hold() {
    // Given: one isolated growing cache reused by four measured repetitions.
    let root = TempDir::new().expect("growing trend root");
    let mut command = Command::new(assert_cmd::cargo::cargo_bin!("pi-casso"));
    command
        .env_remove("PI_CASSO_DATA_DIR")
        .env_remove("PI_CASSO_CONFIG")
        .env("PI_CASSO_TEST_MODE", "1")
        .env("XDG_DATA_HOME", root.path().join("data"))
        .env("XDG_CONFIG_HOME", root.path().join("config"));
    #[cfg(all(windows, target_env = "msvc"))]
    command.env("PI_CASSO_TEST_GENERATOR_VARIANT", "spigot-persistent");
    let output = command
        .args([
            "--json",
            "benchmark",
            "--source-mode",
            "growing",
            "--cache-state",
            "warm",
            "--template",
            "arch",
            "--seconds",
            "10",
            "--work-windows",
            "8192",
            "--repetitions",
            "4",
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
            "1",
            "--chunk-size",
            "2048",
            "--queue-depth",
            "1",
            "--memory-limit-mb",
            "64",
            "--show-metrics",
        ])
        .output()
        .expect("growing trend benchmark starts");

    // When: the first run grows the cache and later runs search the published prefix.
    assert!(
        output.status.success(),
        "benchmark failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report: Value = serde_json::from_slice(&output.stdout).expect("benchmark JSON");
    let runs = report["raw_runs"].as_array().expect("raw repetitions");
    assert_eq!(runs.len(), 4);
    let waits = runs
        .iter()
        .map(|run| {
            run["waits"]["generator_ms"]
                .as_u64()
                .expect("generator wait")
        })
        .collect::<Vec<_>>();

    // Then: measured generator wait strictly falls and never rises, while every
    // repetition remains inside the same queue, memory, and CPU limits.
    assert!(
        waits[0] > *waits.last().expect("final wait"),
        "waits: {waits:?}"
    );
    assert!(
        waits.windows(2).all(|pair| pair[1] <= pair[0]),
        "waits: {waits:?}"
    );
    assert_eq!(runs[0]["first_published_digits"], 0);
    assert!(runs[0]["producer_epochs"].as_u64().unwrap_or_default() > 0);
    for run in runs {
        assert!(
            run["queue"]["max_occupancy"].as_u64().expect("queue peak")
                <= run["queue"]["global_limit"].as_u64().expect("queue limit")
        );
        assert!(
            run["memory"]["logical_peak_bytes"]
                .as_u64()
                .expect("memory peak")
                <= run["memory"]["logical_budget_bytes"]
                    .as_u64()
                    .expect("memory budget")
        );
        assert!(run["cpu_permits_peak"].as_u64().expect("CPU peak") > 0);
        assert!(
            run["cpu_permits_peak"].as_u64().expect("CPU peak")
                <= run["cpu_permits_max"].as_u64().expect("CPU limit")
        );
    }
}
