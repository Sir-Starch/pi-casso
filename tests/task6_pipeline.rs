use std::process::{Command, Output};

use serde_json::Value;
use tempfile::TempDir;

fn benchmark(
    root: &TempDir,
    profile: &str,
    work_windows: &str,
    chunk_size: &str,
    queue_depth: &str,
    memory_limit_mb: &str,
) -> Command {
    benchmark_with_workers(
        root,
        profile,
        work_windows,
        chunk_size,
        queue_depth,
        memory_limit_mb,
        "2",
    )
}

fn benchmark_with_workers(
    root: &TempDir,
    profile: &str,
    work_windows: &str,
    chunk_size: &str,
    queue_depth: &str,
    memory_limit_mb: &str,
    cpu_workers: &str,
) -> Command {
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
            work_windows,
            "--repetitions",
            "1",
            "--warmup",
            "0",
            "--profile",
            profile,
            "--backend",
            "cpu",
            "--gpu",
            "off",
            "--generator-backend",
            "cpu",
            "--cpu-workers",
            cpu_workers,
            "--chunk-size",
            chunk_size,
            "--queue-depth",
            queue_depth,
            "--memory-limit-mb",
            memory_limit_mb,
            "--show-metrics",
        ]);
    command
}

#[test]
fn huge_worker_and_chunk_request_is_rejected_before_pool_or_reader_creation() {
    // Given: a request whose worker pool and reader capacity are intentionally infeasible.
    let root = TempDir::new().expect("resource preflight root");
    let output = benchmark_with_workers(
        &root,
        "performance",
        "1",
        "16777216",
        "1000000",
        "1",
        "1000000",
    )
    .env("PI_CASSO_TEST_FAIL_IF_SOURCE_OPEN", "1")
    .output()
    .expect("infeasible benchmark returns");

    // When/Then: typed preflight wins before Rayon or a path reader can be constructed.
    assert_eq!(output.status.code(), Some(3));
    assert!(!String::from_utf8_lossy(&output.stderr).contains("source-open boundary"));
    let report = json(&output);
    assert_eq!(report["status"], "resource_error");
    assert_eq!(report["scanned_windows"], 0);
    assert_eq!(report["source"]["reader_open_count"], 0);
}

fn json(output: &Output) -> Value {
    serde_json::from_slice(&output.stdout).expect("command stdout is JSON")
}

#[test]
fn bounded_pipeline_backpressure() {
    // Given: a one-slot pipeline and a deliberately delayed ordered consumer.
    let root = TempDir::new().expect("temporary benchmark root");
    let output = benchmark(&root, "performance", "8", "1", "1", "64")
        .env("PI_CASSO_TEST_CONSUMER_DELAY_MS", "50")
        .output()
        .expect("benchmark starts");

    // When: all chunks complete through the real CLI pipeline.
    // Then: the producer is backpressured by the shared slot budget and the run terminates.
    assert!(
        output.status.success(),
        "benchmark failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report = json(&output);
    assert_eq!(report["queue"]["global_limit"], 1);
    assert!(
        report["queue"]["max_occupancy"]
            .as_u64()
            .is_some_and(|v| v <= 1)
    );
    assert!(report["waits"]["queue_ms"].as_u64().is_some_and(|v| v > 0));
    assert_eq!(report["reducer"]["ordered"], true);
}

#[test]
fn stress_both_reports_independent_lanes_and_optional_gpu_skip() {
    // Given: one stress command requesting independent CPU and optional GPU lanes.
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
            "1",
            "--queue-depth",
            "2",
            "--memory-limit-mb",
            "64",
            "--yes",
        ])
        .output()
        .expect("stress starts");

    // When: both lanes finish or the optional GPU capability is skipped.
    // Then: the report proves one command-scoped budget and no fake second CPU lane.
    assert!(
        output.status.success(),
        "stress failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report = json(&output);
    let lanes = report["lanes"].as_array().expect("lane array");
    assert_eq!(lanes.len(), 2);
    assert_eq!(lanes[0]["lane"], "cpu");
    assert_eq!(lanes[1]["lane"], "gpu");
    assert_eq!(lanes[1]["requested_backend"], "auto");
    assert_eq!(
        lanes[1]["auto_candidate_order"],
        serde_json::json!(["cuda", "wgpu", "cpu"])
    );
    assert_eq!(lanes[1]["auto_min_work_windows"], 4_096);
    assert!(lanes[1]["backend_candidates"].is_array());
    assert_eq!(report["aggregate"]["status"], "ok");
    assert_eq!(report["aggregate"]["resource_budget"]["shared"], true);
    assert!(
        report["aggregate"]["cpu_permits_in_use"]
            .as_u64()
            .is_some_and(
                |used| used <= report["aggregate"]["cpu_permits_max"].as_u64().unwrap_or(0)
            )
    );
    assert!(
        report["aggregate"]["queue"]["max_occupancy"]
            .as_u64()
            .is_some_and(|peak| peak
                <= report["aggregate"]["queue"]["global_limit"]
                    .as_u64()
                    .unwrap_or(0))
    );
    if report["lanes"][1]["status"] == "skip" {
        assert_eq!(
            report["lanes"][1]["resource_budget"]["gpu_permits_acquired"],
            0
        );
    }
}

#[test]
fn oversized_reader_capacity_is_rejected_before_source_open() {
    // Given: a configured reader buffer larger than the complete logical budget.
    let root = TempDir::new().expect("reader-capacity root");
    let output = benchmark(&root, "performance", "1", "16777216", "1", "1")
        .env("PI_CASSO_TEST_FAIL_IF_SOURCE_OPEN", "1")
        .output()
        .expect("oversized benchmark starts");

    // When/Then: resource preflight rejects it without constructing a path reader.
    assert_eq!(output.status.code(), Some(3));
    assert!(
        !String::from_utf8_lossy(&output.stderr)
            .contains("PI_CASSO_TEST_FAIL_IF_SOURCE_OPEN reached")
    );
    let report = json(&output);
    assert_eq!(report["status"], "resource_error");
    assert_eq!(report["scanned_windows"], 0);
    assert_eq!(report["source"]["reader_open_count"], 0);
}

#[test]
fn queue_depth_one_and_four_report_bounded_slots() {
    // Given: identical finite workloads with two different queue limits.
    let one_root = TempDir::new().expect("one-slot root");
    let four_root = TempDir::new().expect("four-slot root");
    let one = benchmark(&one_root, "performance", "8", "1", "1", "64")
        .output()
        .expect("one-slot benchmark starts");
    let four = benchmark(&four_root, "performance", "8", "1", "4", "64")
        .output()
        .expect("four-slot benchmark starts");

    // When: both runs complete through the bounded pipeline.
    // Then: occupancy is bounded by the configured limit and the larger queue can overlap.
    assert!(one.status.success());
    assert!(four.status.success());
    let one_report = json(&one);
    let four_report = json(&four);
    assert!(
        one_report["queue"]["max_occupancy"]
            .as_u64()
            .is_some_and(|v| v <= 1)
    );
    assert!(
        four_report["queue"]["max_occupancy"]
            .as_u64()
            .is_some_and(|v| v <= 4)
    );
    assert!(
        four_report["queue"]["max_occupancy"]
            .as_u64()
            .is_some_and(|v| v >= 2)
    );
}

#[test]
fn logical_memory_ceiling_is_enforced() {
    // Given: a finite run with a small but sufficient logical memory budget.
    let root = TempDir::new().expect("memory root");

    // When: several chunks flow through the shared reservation ledger.
    let output = benchmark(&root, "performance", "8", "1", "4", "2")
        .output()
        .expect("memory benchmark starts");

    // Then: current, peak, and budget reservations are independently visible and bounded.
    assert!(output.status.success());
    let report = json(&output);
    assert!(report["memory"]["logical_reserved_mb"].as_f64().is_some());
    assert!(
        report["memory"]["logical_peak_mb"]
            .as_f64()
            .is_some_and(|v| v <= 2.0)
    );
    assert_eq!(report["memory"]["logical_budget_mb"], 2.0);
}

#[test]
fn resource_budget_rejects_before_source_open() {
    // Given: the real source-open failpoint is enabled for both a rejected and
    // an otherwise valid benchmark.
    let root = TempDir::new().expect("resource-error root");
    let rejected = benchmark(&root, "performance", "1", "1", "4", "1")
        .env("PI_CASSO_TEST_MIN_RESERVATION_BYTES", "1048577")
        .env("PI_CASSO_TEST_FAIL_IF_SOURCE_OPEN", "1")
        .output()
        .expect("resource-error benchmark starts");

    // When: the command validates resources before constructing the digit source.
    // Then: it returns the typed resource error without reaching the failpoint.
    assert_eq!(rejected.status.code(), Some(3));
    assert!(
        !String::from_utf8_lossy(&rejected.stderr)
            .contains("PI_CASSO_TEST_FAIL_IF_SOURCE_OPEN reached")
    );
    let report = json(&rejected);
    assert_eq!(report["status"], "resource_error");
    assert_eq!(report["scanned_windows"], 0);
    assert_eq!(report["source"]["reader_open_count"], 0);

    // Then: removing only the oversized reservation reaches the actual
    // source-open boundary, proving the failpoint was active in the first run.
    let opened = benchmark(&root, "performance", "1", "1", "4", "1")
        .env("PI_CASSO_TEST_FAIL_IF_SOURCE_OPEN", "1")
        .output()
        .expect("source-open control benchmark starts");
    assert!(!opened.status.success());
    assert!(
        String::from_utf8_lossy(&opened.stderr)
            .contains("PI_CASSO_TEST_FAIL_IF_SOURCE_OPEN reached")
    );
}

#[test]
fn ordered_reducer_matches_serial_reference() {
    // Given: the same deterministic workload under serial and overlapped queue settings.
    let serial_root = TempDir::new().expect("serial root");
    let overlapped_root = TempDir::new().expect("overlapped root");
    let serial = benchmark(&serial_root, "performance", "8", "1", "1", "64")
        .output()
        .expect("serial benchmark starts");
    let overlapped = benchmark(&overlapped_root, "performance", "8", "1", "4", "64")
        .output()
        .expect("overlapped benchmark starts");

    // When: both reducers persist their completed offsets.
    // Then: ordered reduction preserves the serial score/tie result and only advances contiguously.
    assert!(serial.status.success());
    assert!(overlapped.status.success());
    let serial_report = json(&serial);
    let overlapped_report = json(&overlapped);
    assert_eq!(serial_report["best_score"], overlapped_report["best_score"]);
    assert_eq!(
        serial_report["scanned_windows"],
        overlapped_report["scanned_windows"]
    );
    assert_eq!(overlapped_report["reducer"]["ordered"], true);
    assert_eq!(
        overlapped_report["reducer"]["contiguous_completed_offsets"],
        overlapped_report["scanned_windows"]
    );
}

#[test]
fn max_profile_has_no_fixed_yield() {
    // Given: max profile, full CPU utilization, and no battery/thermal pause.
    let root = TempDir::new().expect("max root");

    // When: chunks complete through the reducer.
    let output = benchmark(&root, "max", "3", "1", "1", "64")
        .output()
        .expect("max benchmark starts");

    // Then: no fixed post-chunk throttle is injected.
    assert!(output.status.success());
    let report = json(&output);
    assert_eq!(report["stage_timings"]["throttle_wait_ms"], 0);
}

#[test]
fn final_chunk_skips_throttle_while_continuing_chunk_still_throttles() {
    // Given: identical balanced-profile requests with one-window chunks.
    let final_root = TempDir::new().expect("final root");
    let continuing_root = TempDir::new().expect("continuing root");

    // When: one request ends after its first chunk and the other continues.
    let final_chunk = benchmark(&final_root, "balanced", "1", "1", "1", "64")
        .output()
        .expect("final-chunk benchmark starts");
    let continuing_chunk = benchmark(&continuing_root, "balanced", "2", "1", "1", "64")
        .output()
        .expect("continuing-chunk benchmark starts");

    // Then: only the chunk followed by more work incurs the configured policy wait.
    assert!(final_chunk.status.success());
    assert!(continuing_chunk.status.success());
    let final_report = json(&final_chunk);
    let continuing_report = json(&continuing_chunk);
    assert_eq!(final_report["stage_timings"]["throttle_wait_ms"], 0);
    assert!(
        continuing_report["stage_timings"]["throttle_wait_ms"]
            .as_u64()
            .is_some_and(|wait_ms| wait_ms > 0)
    );
}

#[test]
fn balanced_and_eco_keep_policy_yields() {
    // Given: the profiles whose configured policy intentionally yields between chunks.
    let balanced_root = TempDir::new().expect("balanced root");
    let eco_root = TempDir::new().expect("eco root");

    // When: each profile completes a bounded multi-chunk run.
    let balanced = benchmark(&balanced_root, "balanced", "3", "1", "1", "64")
        .output()
        .expect("balanced benchmark starts");
    let eco = benchmark(&eco_root, "eco", "3", "1", "1", "64")
        .output()
        .expect("eco benchmark starts");

    // Then: configured policy waits remain observable and distinct from queue/generator waits.
    assert!(balanced.status.success());
    assert!(eco.status.success());
    let balanced_report = json(&balanced);
    let eco_report = json(&eco);
    assert!(
        balanced_report["stage_timings"]["throttle_wait_ms"]
            .as_u64()
            .is_some_and(|v| v > 0)
    );
    assert!(
        eco_report["stage_timings"]["throttle_wait_ms"]
            .as_u64()
            .is_some_and(|v| v > 0)
    );
    assert_ne!(
        balanced_report["waits"]["queue_ms"],
        balanced_report["waits"]["generator_ms"]
    );
}
