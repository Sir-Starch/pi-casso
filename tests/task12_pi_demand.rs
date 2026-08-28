use std::process::{Command, Output};

use serde_json::Value;
use tempfile::TempDir;

fn benchmark(root: &TempDir) -> Command {
    let mut command = Command::new(assert_cmd::cargo::cargo_bin!("pi-casso"));
    command
        .env_remove("PI_CASSO_DATA_DIR")
        .env_remove("PI_CASSO_CONFIG")
        .env_remove("PI_CASSO_TEST_MODE")
        .env_remove("PI_CASSO_TEST_GENERATOR_VARIANT")
        .env("XDG_DATA_HOME", root.path().join("data"))
        .env("XDG_CONFIG_HOME", root.path().join("config"))
        .env("TMPDIR", root.path().join("tmp"))
        .args(["--json", "pi", "benchmark"]);
    command
}

fn report(output: &Output) -> Value {
    serde_json::from_slice(&output.stdout).expect("benchmark stdout is JSON")
}

#[test]
fn search_overlap_reports_real_search_throughput() {
    // Given: a cold growing-cache benchmark with a bounded search work window count.
    let root = TempDir::new().expect("temporary root");
    let mut command = benchmark(&root);
    command.args([
        "--targets",
        "100",
        "--demand-mode",
        "search-overlap",
        "--search-work-windows",
        "16",
        "--repetitions",
        "1",
        "--warmup",
        "0",
        "--generator-backend",
        "cpu",
        "--workers",
        "1",
    ]);

    // When: generation and search execute through the public benchmark command.
    let output = command.output().expect("pi benchmark starts");

    // Then: the report contains measured overlap search throughput and correctness.
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let value = report(&output);
    assert_eq!(value["demand_mode"], "search-overlap");
    assert_eq!(value["search_work_windows"], 16);
    assert!(
        value["median"]["scanned_windows_per_second"]
            .as_f64()
            .is_some_and(|rate| rate > 0.0)
    );
    assert_eq!(value["raw_runs"][0]["search_work_windows"], 16);
    assert_eq!(value["correctness"], true);
}

#[test]
fn test_mode_can_force_the_persistent_spigot_variant() {
    // Given: the allowlisted persistent spigot label in internal test mode.
    let root = TempDir::new().expect("temporary root");
    let mut command = benchmark(&root);
    command
        .env("PI_CASSO_TEST_MODE", "1")
        .env("PI_CASSO_TEST_GENERATOR_VARIANT", "spigot-persistent")
        .args([
            "--targets",
            "100",
            "--repetitions",
            "1",
            "--warmup",
            "0",
            "--generator-backend",
            "cpu",
            "--workers",
            "1",
        ]);

    // When: the benchmark resolves the internal variant.
    let output = command.output().expect("pi benchmark starts");

    // Then: the forced implementation is explicit in report and workload identity.
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let value = report(&output);
    assert_eq!(value["selected_variant"], "spigot-persistent");
    assert_eq!(
        value["workload_identity"]["generator_backend"],
        "spigot-persistent"
    );
    assert_eq!(value["correctness"], true);
}
