use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tempfile::TempDir;

fn script(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("scripts")
        .join(name)
}

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

fn script_command(name: &str) -> Command {
    #[cfg(windows)]
    {
        let mut command = Command::new(git_bash());
        command.arg(script(name));
        command
    }
    #[cfg(not(windows))]
    {
        Command::new(script(name))
    }
}

#[test]
fn ycruncher_error_redaction_and_attestation() {
    // Given: a missing y-cruncher fixture path with spaces and test-only variant selection.
    let root = TempDir::new().expect("temporary root");
    let missing = std::env::var_os("PI_CASSO_TEST_YCRUNCHER_PATH")
        .map(PathBuf::from)
        .unwrap_or_else(|| root.path().join("y cruncher missing"));
    let commands = root.path().join("commands.json");
    let log = root.path().join("command.log");
    let binary = assert_cmd::cargo::cargo_bin!("pi-casso");
    let mut command = script_command("run-evidence-command.sh");
    command
        .env_remove("PI_CASSO_DATA_DIR")
        .env_remove("PI_CASSO_CONFIG")
        .env_remove("XDG_DATA_HOME")
        .env_remove("XDG_CONFIG_HOME")
        .env_remove("TMPDIR")
        .env("PI_CASSO_TEST_MODE", "1")
        .env("PI_CASSO_TEST_GENERATOR_VARIANT", "y-cruncher-external")
        .env("PI_CASSO_TEST_YCRUNCHER_PATH", &missing)
        .args(["--commands-json"])
        .arg(&commands)
        .arg("--log")
        .arg(&log)
        .args(["--expected-exit", "2", "--"])
        .arg(binary)
        .args([
            "--json",
            "pi",
            "benchmark",
            "--targets",
            "100",
            "--repetitions",
            "1",
            "--warmup",
            "0",
            "--generator-backend",
            "y-cruncher",
        ]);

    // When: the evidence wrapper records the typed unavailable command.
    let output = command.output().expect("evidence command starts");

    // Then: command evidence attests test controls while redacting the raw path everywhere.
    assert_eq!(output.status.code(), Some(2));
    let raw_path = missing.to_string_lossy();
    let commands_bytes = fs::read(&commands).expect("commands evidence");
    let log_text = fs::read_to_string(&log).expect("command log");
    assert!(!String::from_utf8_lossy(&output.stdout).contains(raw_path.as_ref()));
    assert!(!String::from_utf8_lossy(&output.stderr).contains(raw_path.as_ref()));
    assert!(!String::from_utf8_lossy(&commands_bytes).contains(raw_path.as_ref()));
    assert!(!log_text.contains(raw_path.as_ref()));
    let recorded: Value = serde_json::from_slice(&commands_bytes).expect("commands JSON");
    assert_eq!(
        recorded[0]["env"]["PI_CASSO_TEST_GENERATOR_VARIANT"],
        "y-cruncher-external"
    );
    assert_eq!(
        recorded[0]["env"]["PI_CASSO_TEST_YCRUNCHER_PATH"],
        "<redacted-y-cruncher-path>"
    );
    assert_eq!(
        recorded[0]["env"]["PI_CASSO_TEST_YCRUNCHER_EXECUTABLE_SHA256"],
        ""
    );
}

fn identity(variant: &str) -> Value {
    json!({
        "template":"arch", "match_mode":"emergence", "canvas_width":24,
        "canvas_height":24, "target_width":12, "target_height":12,
        "target_bitmap_sha256":"a".repeat(64), "start_offset":0,
        "max_offset":-1, "work_windows":4096, "source_mode":"growing",
        "cache_state":"cold", "profile":"performance", "requested_backend":"cpu",
        "gpu_mode":"off", "gpu_device":"auto", "generator_backend":variant,
        "selected_variant":variant, "y_cruncher_path_present":false,
        "y_cruncher_executable_sha256":"", "cpu_workers":1,
        "cpu_utilization":100, "chunk_size":4096, "queue_depth":1,
        "memory_limit_mb":512
    })
}

#[test]
fn unavailable_envelope_is_complete_and_typed() {
    // Given: the external candidate has no executable.
    let root = TempDir::new().expect("temporary root");
    let output = root.path().join("unavailable.json");

    // When: the owned envelope helper records the candidate.
    let status = script_command("create-pi-unavailable-envelope.sh")
        .args([
            "--variant",
            "y-cruncher-external",
            "--reason",
            "external_ycruncher_path_missing",
            "--output",
            output.to_str().expect("output path"),
        ])
        .status()
        .expect("helper starts");

    // Then: all four normalized input modes have explicit unavailable sentinels.
    assert!(status.success());
    let value: Value =
        serde_json::from_slice(&fs::read(output).expect("envelope")).expect("envelope JSON");
    assert_eq!(value["schema_version"], 1);
    assert_eq!(value["status"], "unavailable");
    assert_eq!(value["selected_variant"], "y-cruncher-external");
    for mode in ["serial", "concurrent", "search_overlap", "end_to_end"] {
        assert_eq!(value[mode]["status"], "unavailable");
        assert_eq!(value[mode]["correctness"], false);
    }
    assert_eq!(value["end_to_end"]["repetitions_dir"], "");
}

#[test]
fn normalizer_maps_hyphenated_overlap_without_inventing_metrics() {
    // Given: a valid raw search-overlap benchmark artifact.
    let root = TempDir::new().expect("temporary root");
    let raw = root.path().join("raw.json");
    let normalized = root.path().join("search_overlap.normalized.json");
    let hash = "b".repeat(64);
    fs::write(
        &raw,
        serde_json::to_vec(&json!({
            "schema_version":1, "status":"ok", "reason":"",
            "demand_mode":"search-overlap", "selected_variant":"spigot-persistent",
            "generator_executable_sha256":hash, "workload_identity":identity("spigot-persistent"),
            "median":{"generated_source_digits_per_second":321.0,"generation_wait_ms":7,
                "scanned_windows_per_second":123.0},
            "p95":{"generated_source_digits_per_second":300.0,"generation_wait_ms":9,
                "scanned_windows_per_second":100.0},
            "search_work_windows":4096, "overlap_wait_ms":4, "correctness":true,
            "coalesced_request_count":0, "producer_epochs":1, "generation_batches":1
        }))
        .expect("raw JSON"),
    )
    .expect("raw artifact");

    // When: the normalizer consumes the declared raw artifact.
    let status = script_command("normalize-pi-variant.sh")
        .args([
            "--input",
            raw.to_str().expect("raw path"),
            "--variant",
            "spigot-persistent",
            "--mode",
            "search-overlap",
            "--artifact",
            raw.to_str().expect("raw path"),
            "--output",
            normalized.to_str().expect("normalized path"),
        ])
        .status()
        .expect("normalizer starts");

    // Then: the selector key and every score equal the raw values.
    assert!(status.success());
    let value: Value = serde_json::from_slice(&fs::read(normalized).expect("normalized artifact"))
        .expect("normalized JSON");
    assert_eq!(value["mode"], "search_overlap");
    assert_eq!(value["median_generated_source_digits_per_second"], 321.0);
    assert_eq!(value["median_scanned_windows_per_second"], 123.0);
    assert_eq!(value["median_overlap_wait_ms"], 4);
    assert_eq!(value["correctness"], true);
    assert_eq!(value["variant_executable_sha256"], "b".repeat(64));
}

#[test]
fn selector_applies_the_declared_deterministic_tie_break() {
    // Given: three correct candidates with an accepted end-to-end gate.
    let root = TempDir::new().expect("temporary root");
    let variants = root.path().join("variants");
    let baseline = root.path().join("baseline");
    fs::create_dir_all(&baseline).expect("baseline directory");
    write_candidate(&variants, "chudnovsky-rug-binary-split", 100.0, 100.0, 10);
    write_candidate(&variants, "spigot-persistent", 100.0, 200.0, 20);
    write_candidate(&variants, "y-cruncher-external", 90.0, 1000.0, 1);
    let comparison = root.path().join("comparison.json");
    fs::write(
        &comparison,
        serde_json::to_vec(&json!({
            "schema_version":1, "accepted":true, "rejection_reason":""
        }))
        .expect("comparison JSON"),
    )
    .expect("comparison artifact");
    let output = root.path().join("selection.json");

    // When: the selector ranks the normalized candidates.
    let status = script_command("select-pi-variant.sh")
        .args([
            "--input-dir",
            variants.to_str().expect("variants path"),
            "--baseline-dir",
            baseline.to_str().expect("baseline path"),
            "--comparison-json",
            comparison.to_str().expect("comparison path"),
            "--output",
            output.to_str().expect("output path"),
        ])
        .status()
        .expect("selector starts");

    // Then: equal overlap throughput is resolved by generation geometric mean.
    assert!(status.success());
    let value: Value =
        serde_json::from_slice(&fs::read(output).expect("selection")).expect("selection JSON");
    assert_eq!(value["selected_variant"], "spigot-persistent");
    assert_eq!(
        value["tie_break"],
        json!([
            "search_overlap_scanned_windows_per_second",
            "generation_geometric_mean",
            "generation_wait_ms",
            "selected_variant"
        ])
    );
}

fn write_candidate(
    root: &Path,
    variant: &str,
    overlap_rate: f64,
    generation_rate: f64,
    wait_ms: u64,
) {
    let directory = root.join(variant);
    let repetitions = directory.join("end-to-end");
    fs::create_dir_all(&repetitions).expect("candidate directory");
    let summary = directory.join("end-to-end-raw.json");
    fs::write(&summary, b"{}\n").expect("summary artifact");
    let executable_hash = match variant {
        "chudnovsky-rug-binary-split" => "1".repeat(64),
        "spigot-persistent" => "2".repeat(64),
        "y-cruncher-external" => "3".repeat(64),
        _ => panic!("unexpected variant fixture"),
    };
    for mode in ["serial", "concurrent", "search_overlap", "end_to_end"] {
        let raw = directory.join(format!("{mode}.raw.json"));
        fs::write(&raw, format!("{{\"mode\":\"{mode}\"}}\n")).expect("raw artifact");
        let raw_hash = format!("{:x}", Sha256::digest(fs::read(&raw).expect("raw bytes")));
        let normalized = json!({
            "schema_version":1, "mode":mode, "selected_variant":variant,
            "status":"ok", "reason":"", "artifact":raw,
            "summary_artifact":summary, "repetitions_dir":repetitions,
            "input_artifact":raw, "raw_input_sha256":raw_hash,
            "workload_identity":identity(variant),
            "variant_executable_sha256":executable_hash,
            "median_generated_source_digits_per_second":generation_rate,
            "p95_generated_source_digits_per_second":generation_rate,
            "median_generation_wait_ms":wait_ms, "p95_generation_wait_ms":wait_ms,
            "median_scanned_windows_per_second":if mode == "search_overlap" { overlap_rate } else { 1.0 },
            "p95_scanned_windows_per_second":overlap_rate,
            "median_source_digits_per_second":1.0, "p95_source_digits_per_second":1.0,
            "median_overlap_wait_ms":0, "coalesced_request_count":4,
            "producer_epochs":1, "generation_batches":1, "search_work_windows":4096,
            "correctness":true
        });
        let normalized_name = if mode == "end_to_end" {
            "end-to-end.normalized.json".to_string()
        } else {
            format!("{mode}.normalized.json")
        };
        fs::write(
            directory.join(normalized_name),
            serde_json::to_vec(&normalized).expect("normalized JSON"),
        )
        .expect("normalized artifact");
    }
}
