use std::process::{Command, Output};

use serde_json::Value;
use tempfile::TempDir;

const STAGE_FIELDS: [&str; 12] = [
    "read_ms",
    "parse_ms",
    "queue_wait_ms",
    "backend_compute_ms",
    "gpu_allocation_ms",
    "gpu_upload_ms",
    "gpu_dispatch_ms",
    "gpu_readback_map_ms",
    "reduction_ms",
    "persistence_ms",
    "generation_wait_ms",
    "throttle_wait_ms",
];
const WAIT_FIELDS: [&str; 4] = ["source_ms", "queue_ms", "generator_ms", "throttle_ms"];

fn benchmark(
    root: &TempDir,
    work_windows: u64,
    chunk_size: usize,
    backend: &str,
    gpu: &str,
) -> Command {
    benchmark_with_source_mode(root, work_windows, chunk_size, backend, gpu, "finite")
}

fn benchmark_with_source_mode(
    root: &TempDir,
    work_windows: u64,
    chunk_size: usize,
    backend: &str,
    gpu: &str,
    source_mode: &str,
) -> Command {
    let mut command = Command::new(assert_cmd::cargo::cargo_bin!("pi-casso"));
    command
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
            source_mode,
            "--cache-state",
            "cold",
            "--work-windows",
            &work_windows.to_string(),
            "--repetitions",
            "1",
            "--warmup",
            "0",
            "--profile",
            "performance",
            "--backend",
            backend,
            "--gpu",
            gpu,
            "--generator-backend",
            "cpu",
            "--cpu-workers",
            "1",
            "--chunk-size",
            &chunk_size.to_string(),
            "--queue-depth",
            "1",
            "--memory-limit-mb",
            "64",
            "--show-metrics",
        ]);
    command
}

fn output_json(output: &Output) -> Value {
    assert!(
        output.status.success(),
        "benchmark failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).expect("benchmark stdout is JSON")
}

fn assert_nonnegative_fields(value: &Value, parent: &str, fields: &[&str]) {
    let object = value[parent]
        .as_object()
        .unwrap_or_else(|| panic!("{parent} is an object"));
    assert_eq!(object.len(), fields.len(), "{parent} remains flat");
    for field in fields {
        assert!(
            object[*field].as_u64().is_some(),
            "{parent}.{field} is a nonnegative integer in milliseconds"
        );
    }
}

#[test]
fn benchmark_reports_flat_stage_and_wait_accounting_when_metrics_enabled() {
    // Given: a real finite CPU benchmark with detailed metrics enabled.
    let root = TempDir::new().expect("temporary benchmark root");

    // When: one measured chunk completes through the CLI report surface.
    let report = output_json(
        &benchmark(&root, 2, 2, "cpu", "off")
            .output()
            .expect("benchmark starts"),
    );

    // Then: every version-1 millisecond field and final runtime identity is explicit.
    assert_nonnegative_fields(&report, "stage_timings", &STAGE_FIELDS);
    assert_nonnegative_fields(&report, "waits", &WAIT_FIELDS);
    assert_eq!(report["resolved_backend"], "cpu");
    assert_eq!(report["backend_device"], "cpu");
    assert_eq!(report["backend_feature_available"], true);
    assert_eq!(report["fallback"], false);
    assert_eq!(report["fallback_count"], 0);
    assert!(report["source_lag_digits"].as_u64().is_some());
    assert!(report["generator_digits_per_second"].as_f64().is_some());
    assert_eq!(report["telemetry_enabled"], true);
}

#[test]
fn empty_range_keeps_complete_zero_telemetry() {
    // Given: a benchmark whose exclusive range contains no windows.
    let root = TempDir::new().expect("temporary benchmark root");

    // When: the CLI resolves the range without entering the search loop.
    let report = output_json(
        &benchmark(&root, 0, 1, "cpu", "off")
            .output()
            .expect("benchmark starts"),
    );

    // Then: telemetry is structurally complete and no wait is fabricated.
    assert_eq!(report["stop_reason"], "empty_range");
    assert_eq!(report["scanned_windows"], 0);
    assert_nonnegative_fields(&report, "stage_timings", &STAGE_FIELDS);
    assert_nonnegative_fields(&report, "waits", &WAIT_FIELDS);
    assert!(
        STAGE_FIELDS
            .iter()
            .all(|field| report["stage_timings"][*field] == 0)
    );
    assert!(WAIT_FIELDS.iter().all(|field| report["waits"][*field] == 0));
}

#[test]
fn telemetry_is_aggregated_per_run_without_window_events() {
    // Given: three windows forced through three one-window chunks.
    let root = TempDir::new().expect("temporary benchmark root");

    // When: the benchmark emits its one final structured report.
    let output = benchmark(&root, 3, 1, "cpu", "off")
        .output()
        .expect("benchmark starts");
    let report = output_json(&output);

    // Then: telemetry is bounded to run/chunk counters and emits no per-window stream.
    assert_eq!(report["raw_runs"].as_array().map(Vec::len), Some(1));
    assert!(report.get("window_events").is_none());
    assert!(report.get("window_timings").is_none());
    assert_eq!(report["gpu"]["submissions"], 0);
    assert!(
        output.stderr.is_empty(),
        "CPU telemetry writes no hot-path log"
    );
}

#[test]
fn growing_source_records_source_and_queue_waits() {
    // Given: a growing cache source with enough one-window chunks to exercise its readiness path.
    let root = TempDir::new().expect("temporary benchmark root");

    // When: the benchmark runs through the real growing-source CLI path.
    let report = output_json(
        &benchmark_with_source_mode(&root, 128, 1, "cpu", "off", "growing")
            .output()
            .expect("benchmark starts"),
    );

    // Then: source readiness and queue submission are measured independently of generator waits.
    assert!(
        report["waits"]["source_ms"]
            .as_u64()
            .is_some_and(|duration| duration > 0),
        "growing source wait must be recorded"
    );
    assert!(
        report["waits"]["queue_ms"]
            .as_u64()
            .is_some_and(|duration| duration > 0),
        "growing queue wait must be recorded"
    );
    assert_eq!(
        report["waits"]["generator_ms"],
        report["stage_timings"]["generation_wait_ms"]
    );
    assert_eq!(report["producer_epochs"], 1);
    assert_eq!(report["coalesced_request_count"], 1);
    assert_eq!(report["generation_batches"], 1);
    assert!(
        report["lead_digits"]
            .as_u64()
            .is_some_and(|digits| digits > 0)
    );
    assert!(
        report["source_lag_digits"]
            .as_u64()
            .is_some_and(|digits| digits > 0),
        "final telemetry refreshes the growing cache's published length"
    );
    assert!(
        report["high_water_digits"]
            .as_u64()
            .is_some_and(|digits| digits >= 704)
    );
    assert!(
        report["event_wake_latency_ms"]
            .as_u64()
            .is_some_and(|latency| latency < 20)
    );
    assert!(
        report["waits"]["generator_ms"]
            .as_u64()
            .is_some_and(|duration| duration < 5_000),
        "event-driven generator wait stays bounded"
    );
    assert!(
        report["memory"]["logical_peak_bytes"]
            .as_u64()
            .zip(report["memory"]["logical_budget_bytes"].as_u64())
            .is_some_and(|(peak, budget)| peak <= budget)
    );
    assert!(
        report["cpu_permits_in_use"]
            .as_u64()
            .zip(report["cpu_permits_max"].as_u64())
            .is_some_and(|(used, maximum)| used <= maximum)
    );
}

#[test]
fn synthetic_backend_error_records_one_mixed_fallback() {
    // Given: the host-independent test seam completes one fake accelerator chunk,
    // then injects one backend error before a second chunk.
    let root = TempDir::new().expect("temporary benchmark root");
    let mut command = benchmark(&root, 2, 1, "auto", "auto");
    command
        .env("PI_CASSO_TEST_MODE", "1")
        .env("PI_CASSO_TEST_FAKE_WGPU_PREFLIGHT", "1")
        .env("PI_CASSO_TEST_FAKE_WGPU_EXECUTION", "1")
        .env("PI_CASSO_TEST_BACKEND_FAIL_AFTER_PREFLIGHT", "wgpu");

    // When: both chunks finish through the normal benchmark report surface.
    let report = output_json(&command.output().expect("benchmark starts"));

    // Then: accelerator work is not relabeled CPU and fallback is counted once.
    assert_eq!(report["scanned_windows"], 2);
    assert_eq!(report["requested_backend"], "auto");
    assert_eq!(report["resolved_backend"], "mixed");
    assert_eq!(report["fallback"], true);
    assert_eq!(report["fallback_count"], 1);
    assert_eq!(report["gpu"]["completions"], 1);
    assert_eq!(report["gpu"]["fallback_count"], 1);
    assert!(
        report["fallback_reason"]
            .as_str()
            .is_some_and(|reason| reason.contains("synthetic wgpu backend failure"))
    );
}
