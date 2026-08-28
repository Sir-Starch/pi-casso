use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};
use tempfile::TempDir;

#[cfg(windows)]
fn git_bash() -> PathBuf {
    ["ProgramFiles", "ProgramFiles(x86)"]
        .into_iter()
        .filter_map(|name| std::env::var_os(name))
        .map(PathBuf::from)
        .map(|path| path.join("Git").join("bin").join("bash.exe"))
        .find(|path| path.is_file())
        .unwrap_or_else(|| PathBuf::from(r"C:\Program Files\Git\bin\bash.exe"))
}

fn shell_path(path: &Path) -> String {
    #[cfg(windows)]
    {
        let canonical = fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
        let value = canonical.to_string_lossy().replace('\\', "/");
        let value = value.strip_prefix("//?/").unwrap_or(&value);
        if value.as_bytes().get(1) == Some(&b':') {
            let drive = value
                .chars()
                .next()
                .expect("drive letter")
                .to_ascii_lowercase();
            return format!("/{drive}{}", &value[2..]);
        }
        value.to_string()
    }
    #[cfg(not(windows))]
    {
        path.display().to_string()
    }
}

fn script_command(name: &str) -> Command {
    let path = script(name);
    #[cfg(windows)]
    {
        let mut command = Command::new(git_bash());
        let script_path = path.to_string_lossy().replace('\\', "/");
        let quoted_script = format!("'{}'", script_path.replace('\'', "'\\''"));
        let command_line = format!("exec {quoted_script} \"$@\"");
        command
            .args(["--noprofile", "--norc", "-lc"])
            .arg(&command_line)
            .arg("pi-casso-script");
        command
    }
    #[cfg(not(windows))]
    {
        Command::new(path)
    }
}

fn write_fixture(root: &Path, name: &str, rate: f64, reuse_cold_cache: bool) -> PathBuf {
    let repetitions_dir = root.join(name);
    fs::create_dir_all(&repetitions_dir).expect("repetition directory");
    let mut repetition_paths = Vec::new();
    let mut digests = Map::new();
    for repetition in 0..5 {
        let path = repetitions_dir.join(format!("repetition-{repetition}.json"));
        let cache_instance_id = if reuse_cold_cache {
            "reused-cold-cache".to_string()
        } else {
            format!("{name}-cold-cache-{repetition}")
        };
        let run = json!({
            "schema_version": 1,
            "status": "ok",
            "repetition": repetition,
            "cache_instance_id": cache_instance_id,
            "cache_reset": true,
            "warm_up_completed": false,
            "first_published_digits": 0,
            "scanned_windows": 100,
            "source_digits_read": 675,
            "logical_window_digits": 57_600,
            "scanned_windows_per_second": rate,
            "source_digits_per_second": rate * 6.75,
            "logical_window_digits_per_second": rate * 576.0,
            "elapsed_seconds": 1.0,
            "stop_reason": "work_windows",
            "stage_timings": {
                "read_ms": 0, "parse_ms": 0, "queue_wait_ms": 0,
                "backend_compute_ms": 1, "gpu_allocation_ms": 0,
                "gpu_upload_ms": 0, "gpu_dispatch_ms": 0,
                "gpu_readback_map_ms": 0, "reduction_ms": 0,
                "persistence_ms": 0, "generation_wait_ms": 0,
                "throttle_wait_ms": 0
            },
            "waits": {"source_ms": 0, "queue_ms": 0, "generator_ms": 0, "throttle_ms": 0},
            "overlap_wait_ms": 0,
            "cache_write_ms": 0,
            "producer_epochs": 0,
            "coalesced_request_count": 0,
            "generation_batches": 0,
            "best_score": 0.5
        });
        let bytes = serde_json::to_vec_pretty(&run).expect("serialize repetition");
        fs::write(&path, &bytes).expect("write repetition");
        let path_string = shell_path(&path);
        digests.insert(
            path_string.clone(),
            json!({"bytes": bytes.len(), "sha256": format!("{:x}", Sha256::digest(&bytes))}),
        );
        repetition_paths.push(path_string);
    }

    let summary_path = root.join(format!("{name}-raw.json"));
    let aggregate = json!({
        "scanned_windows_per_second": rate,
        "source_digits_per_second": rate * 6.75,
        "logical_window_digits_per_second": rate * 576.0,
        "elapsed_seconds": 1.0,
        "overlap_wait_ms": 0,
        "cache_write_ms": 0,
        "generation_wait_ms": 0
    });
    let summary = json!({
        "schema_version": 1,
        "status": "ok",
        "workload_id": "bench-v1-contract-fixture",
        "workload_identity": {"cache_state":"cold", "cpu_workers":1, "work_windows":100},
        "source_mode": "finite",
        "cache_state": "cold",
        "warmup": 0,
        "warm_up_completed": false,
        "config": {"cpu_workers":1},
        "machine": {
            "os":"fixture-os", "cpu":"fixture-cpu", "gpu":"unavailable",
            "driver":"unavailable", "rustc":"fixture-rustc",
            "power_policy":"fixture-power", "thermal_policy":"fixture-thermal"
        },
        "repetitions": 5,
        "median": aggregate,
        "p95": aggregate,
        "overlap_wait_ms": 0,
        "cache_write_ms": 0,
        "producer_epochs": 0
    });
    fs::write(
        &summary_path,
        serde_json::to_vec_pretty(&summary).expect("serialize summary"),
    )
    .expect("write summary");
    let manifest = json!({
        "schema_version": 1,
        "summary_artifact": shell_path(&summary_path),
        "cache_state": "cold",
        "expected_count": 5,
        "repetitions": repetition_paths,
        "raw_file_digests": Value::Object(digests)
    });
    fs::write(
        repetitions_dir.join("manifest.json"),
        serde_json::to_vec_pretty(&manifest).expect("serialize manifest"),
    )
    .expect("write manifest");
    repetitions_dir
}

fn script(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("scripts")
        .join(name)
}

#[test]
fn benchmark_repetition_audit_accepts_canonical_cold_runs() {
    // Given: five schema-valid cold repetitions with distinct cache instances and digests.
    let root = TempDir::new().expect("temporary root");
    let repetitions = write_fixture(root.path(), "baseline", 100.0, false);
    let output = root.path().join("audit.json");
    let repetitions_path = shell_path(&repetitions);

    // When: the plan-owned repetition verifier audits the directory.
    let result = script_command("verify-benchmark-repetitions.sh")
        .args([
            "--dir",
            repetitions_path.as_str(),
            "--cache-state",
            "cold",
            "--expected-count",
            "5",
            "--output",
            output.to_str().expect("utf8 path"),
        ])
        .output()
        .expect("repetition verifier starts");

    // Then: the verifier writes a typed passing audit.
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let audit: Value =
        serde_json::from_slice(&fs::read(output).expect("audit output")).expect("audit JSON");
    assert_eq!(audit["status"], "pass");
    assert_eq!(audit["verified_count"], 5);
}

#[test]
fn benchmark_comparison_accepts_an_identity_matched_improvement() {
    // Given: identity-matched five-run baseline and candidate fixtures above the noise floor.
    let root = TempDir::new().expect("temporary root");
    let baseline = write_fixture(root.path(), "baseline", 100.0, false);
    let candidate = write_fixture(root.path(), "candidate", 110.0, false);
    let output = root.path().join("comparison.json");
    let baseline_path = shell_path(&baseline);
    let candidate_path = shell_path(&candidate);

    // When: the plan-owned exact comparison evaluates throughput and p95.
    let result = script_command("compare-benchmark-runs.sh")
        .args([
            "--baseline-dir",
            baseline_path.as_str(),
            "--candidate-dir",
            candidate_path.as_str(),
            "--metrics",
            "scanned_windows_per_second",
            "--max-p95-regression",
            "0.10",
            "--output",
            output.to_str().expect("utf8 path"),
        ])
        .output()
        .expect("benchmark comparison starts");

    // Then: the typed result records acceptance and the computed noise floor.
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let comparison: Value = serde_json::from_slice(&fs::read(output).expect("comparison output"))
        .expect("comparison JSON");
    assert_eq!(comparison["status"], "pass");
    assert_eq!(comparison["accepted"], true);
    assert!(
        comparison["metrics"][0]["noise_floor"]
            .as_f64()
            .is_some_and(|value| value >= 0.05)
    );
}

#[test]
fn benchmark_comparison_accepts_equal_optimal_zero_lower_metric() {
    // Given: identity-matched benchmark runs whose lower-is-better wait is optimally zero.
    let root = TempDir::new().expect("temporary root");
    let baseline = write_fixture(root.path(), "baseline", 100.0, false);
    let candidate = write_fixture(root.path(), "candidate", 100.0, false);
    let output = root.path().join("comparison.json");
    let baseline_path = shell_path(&baseline);
    let candidate_path = shell_path(&candidate);

    // When: the exact comparator evaluates equal zero overlap wait.
    let result = script_command("compare-benchmark-runs.sh")
        .args([
            "--baseline-dir",
            baseline_path.as_str(),
            "--candidate-dir",
            candidate_path.as_str(),
            "--metrics",
            "overlap_wait_ms",
            "--max-p95-regression",
            "0.10",
            "--output",
            output.to_str().expect("utf8 path"),
        ])
        .output()
        .expect("benchmark comparison starts");

    // Then: exact equality at the optimum is accepted without changing the noise threshold.
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let comparison: Value = serde_json::from_slice(&fs::read(output).expect("comparison output"))
        .expect("comparison JSON");
    assert_eq!(comparison["accepted"], true);
    assert_eq!(comparison["metrics"][0]["equal_optimal"], true);
    assert_eq!(comparison["metrics"][0]["noise_floor"], 0.05);
}

#[test]
fn benchmark_repetition_audit_rejects_reused_cold_cache() {
    // Given: five cold repetitions that incorrectly reuse one cache instance.
    let root = TempDir::new().expect("temporary root");
    let repetitions = write_fixture(root.path(), "reused", 100.0, true);
    let output = root.path().join("audit.json");
    let repetitions_path = shell_path(&repetitions);

    // When: the repetition verifier audits cold-cache isolation.
    let result = script_command("verify-benchmark-repetitions.sh")
        .args([
            "--dir",
            repetitions_path.as_str(),
            "--cache-state",
            "cold",
            "--expected-count",
            "5",
            "--output",
            output.to_str().expect("utf8 path"),
        ])
        .output()
        .expect("repetition verifier starts");

    // Then: the invalid cold-cache fixture is rejected and no pass artifact is emitted.
    assert!(!result.status.success());
    assert!(!output.exists());
}
