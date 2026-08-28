use std::path::Path;
use std::process::{Command, Output};

use serde_json::Value;
use tempfile::TempDir;

#[cfg(windows)]
fn git_bash() -> std::path::PathBuf {
    ["ProgramFiles", "ProgramFiles(x86)"]
        .into_iter()
        .filter_map(|name| std::env::var_os(name))
        .map(std::path::PathBuf::from)
        .map(|path| path.join("Git").join("bin").join("bash.exe"))
        .find(|path| path.is_file())
        .unwrap_or_else(|| std::path::PathBuf::from(r"C:\Program Files\Git\bin\bash.exe"))
}

fn named_test_command(script: &Path, target_dir: &Path) -> Command {
    #[cfg(windows)]
    {
        let mut command = Command::new(git_bash());
        let script_path = script.to_string_lossy().replace('\\', "/");
        let quoted_script = format!("'{}'", script_path.replace('\'', "'\\''"));
        let command_line = format!("exec {quoted_script} \"$@\"");
        command
            .args(["--noprofile", "--norc", "-lc"])
            .arg(&command_line)
            .arg("pi-casso-named-test");
        command.env("PI_CASSO_NAMED_TEST_TARGET_DIR", target_dir);
        command.env("CARGO_NET_OFFLINE", "true");
        command
    }
    #[cfg(not(windows))]
    {
        let mut command = Command::new(script);
        command.env("PI_CASSO_NAMED_TEST_TARGET_DIR", target_dir);
        command.env("CARGO_NET_OFFLINE", "true");
        command
    }
}

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

fn benchmark(root: &TempDir, extra: &[&str]) -> Output {
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
        "--profile",
        "eco",
        "--generator-backend",
        "cpu",
        "--cpu-workers",
        "1",
        "--chunk-size",
        "2",
        "--queue-depth",
        "1",
        "--memory-limit-mb",
        "64",
    ]);
    for (flag, value) in [
        ("--work-windows", "2"),
        ("--repetitions", "1"),
        ("--warmup", "0"),
        ("--backend", "cpu"),
        ("--gpu", "off"),
    ] {
        if !extra.contains(&flag) {
            command.args([flag, value]);
        }
    }
    command.args(extra).output().expect("benchmark starts")
}

fn parse_stdout(output: &Output) -> Value {
    serde_json::from_slice(&output.stdout).expect("stdout is JSON")
}

#[test]
fn benchmark_args_enforce_contract_bounds() {
    // Given: an isolated benchmark command with the new fixed-work flags.
    let root = TempDir::new().expect("temporary root");

    // When: the command uses valid lower-bound values.
    let valid = benchmark(&root, &[]);

    // Then: the fixed-work benchmark is accepted.
    assert!(
        valid.status.success(),
        "{}",
        String::from_utf8_lossy(&valid.stderr)
    );

    // Given: a fresh isolated command.
    let root = TempDir::new().expect("temporary root");

    // When: repetitions is below its public lower bound.
    let invalid = benchmark(&root, &["--repetitions", "0"]);

    // Then: clap rejects the malformed boundary before work.
    assert_eq!(invalid.status.code(), Some(2));
}

#[test]
fn benchmark_json_exposes_schema_and_distinct_rates() {
    // Given: a two-window finite benchmark.
    let root = TempDir::new().expect("temporary root");

    // When: the real CLI emits JSON.
    let output = benchmark(&root, &[]);

    // Then: the schema is versioned and source/logical rates are distinct concepts.
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report = parse_stdout(&output);
    assert_eq!(report["schema_version"], 1);
    assert_eq!(report["status"], "ok");
    assert_eq!(report["scanned_windows"], 2);
    assert!(report["source_digits_per_second"].is_number());
    assert!(report["logical_window_digits_per_second"].is_number());
    assert_ne!(
        report["source_digits_per_second"],
        report["scanned_windows_per_second"]
    );
    assert!(report["stage_timings"].is_object());
    assert!(report["waits"].is_object());
    assert!(report["source"].is_object());
}

#[test]
fn auto_backend_below_threshold_reports_ordered_cpu_fallback_metadata() {
    // Given: an auto-selected workload below the accelerator setup threshold.
    let root = TempDir::new().expect("temporary root");

    // When: the real CLI resolves the backend before running the finite benchmark.
    let output = benchmark(
        &root,
        &["--work-windows", "16", "--backend", "auto", "--gpu", "auto"],
    );

    // Then: CPU selection is an explained fallback with deterministic candidates.
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report = parse_stdout(&output);
    assert_eq!(report["schema_version"], 1);
    assert_eq!(report["requested_backend"], "auto");
    assert_eq!(report["resolved_backend"], "cpu");
    assert_eq!(report["fallback"], true);
    assert!(
        report["fallback_reason"]
            .as_str()
            .is_some_and(|reason| !reason.is_empty())
    );
    assert_eq!(report["auto_min_work_windows"], 4_096);

    let candidates = report["backend_candidates"]
        .as_array()
        .expect("backend candidates are an array");
    assert_eq!(candidates.len(), 3);
    assert_eq!(candidates[0]["backend"], "cuda");
    assert_eq!(candidates[1]["backend"], "wgpu");
    assert_eq!(candidates[2]["backend"], "cpu");
    assert_eq!(candidates[0]["status"], "skipped");
    assert_eq!(candidates[1]["status"], "skipped");
    assert_eq!(candidates[2]["status"], "selected");
    assert_eq!(candidates[0]["eligible"], false);
    assert_eq!(candidates[1]["eligible"], false);
    assert_eq!(candidates[2]["eligible"], true);
    assert!(
        candidates[0]["reason"]
            .as_str()
            .is_some_and(|reason| !reason.is_empty())
    );
    assert!(
        candidates[1]["reason"]
            .as_str()
            .is_some_and(|reason| !reason.is_empty())
    );
}

#[test]
fn finite_warm_completes_after_untimed_warmup() {
    // Given: a finite warm benchmark that shares one cache across warm-up and measured runs.
    let root = TempDir::new().expect("temporary root");
    let mut command = isolated_command(&root);
    command.args([
        "--json",
        "benchmark",
        "--template",
        "arch",
        "--source-mode",
        "finite",
        "--cache-state",
        "warm",
        "--work-windows",
        "2",
        "--repetitions",
        "2",
        "--warmup",
        "1",
        "--profile",
        "eco",
        "--backend",
        "cpu",
        "--gpu",
        "off",
        "--generator-backend",
        "cpu",
        "--cpu-workers",
        "1",
        "--chunk-size",
        "2",
        "--queue-depth",
        "1",
        "--memory-limit-mb",
        "64",
    ]);

    // When: the real CLI executes the untimed warm-up before both repetitions.
    let output = command.output().expect("warm benchmark starts");

    // Then: all measured runs complete against one warmed cache instance.
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report = parse_stdout(&output);
    assert_eq!(report["status"], "ok");
    assert_eq!(report["raw_runs"].as_array().map(Vec::len), Some(2));
    assert_eq!(report["warm_up_completed"], true);
}

#[test]
fn benchmark_workload_hash_contract() {
    // Given: identical canonical workload fields in independent cache roots.
    let first_root = TempDir::new().expect("first root");
    let second_root = TempDir::new().expect("second root");

    // When: runner-only repetition metadata changes.
    let first = benchmark(&first_root, &[]);
    let second = benchmark(&second_root, &["--repetitions", "2"]);

    // Then: both commands succeed and preserve the bench-v1 identity.
    assert!(
        first.status.success(),
        "{}",
        String::from_utf8_lossy(&first.stderr)
    );
    assert!(
        second.status.success(),
        "{}",
        String::from_utf8_lossy(&second.stderr)
    );
    let first_report = parse_stdout(&first);
    let second_report = parse_stdout(&second);
    let first_id = first_report["workload_id"].as_str().expect("workload id");
    assert!(first_id.starts_with("bench-v1-"));
    assert_eq!(first_report["workload_id"], second_report["workload_id"]);

    // Given: one changed canonical field.
    let changed_root = TempDir::new().expect("changed root");

    // When: the exclusive work-window budget changes.
    let changed = benchmark(&changed_root, &["--work-windows", "3"]);

    // Then: the stable workload identity changes.
    assert!(
        changed.status.success(),
        "{}",
        String::from_utf8_lossy(&changed.stderr)
    );
    assert_ne!(
        first_report["workload_id"],
        parse_stdout(&changed)["workload_id"]
    );
}

#[test]
fn selection_error_uses_status_discriminated_envelope() {
    // Given: an inconsistent explicit backend/gpu pair.
    let root = TempDir::new().expect("temporary root");

    // When: selection is attempted through the real JSON CLI.
    let output = benchmark(&root, &["--backend", "gpu", "--gpu", "off"]);

    // Then: it fails before work with the structured preflight envelope.
    assert_eq!(output.status.code(), Some(2));
    let report = parse_stdout(&output);
    assert_eq!(report["status"], "selection_error");
    assert!(report["resolved_backend"].is_null());
    assert_eq!(report["fallback"], false);
    assert_eq!(report["scanned_windows"], 0);
    assert!(
        report["reason"]
            .as_str()
            .is_some_and(|reason| !reason.is_empty())
    );
}

#[test]
fn gpu_and_pi_cache_info_honor_global_json() {
    // Given: isolated global JSON commands.
    let root = TempDir::new().expect("temporary root");

    // When: gpu info and pi cache-info are invoked.
    let gpu = isolated_command(&root)
        .args(["--json", "gpu", "info"])
        .output()
        .expect("gpu info starts");
    let cache = isolated_command(&root)
        .args(["--json", "pi", "cache-info"])
        .output()
        .expect("cache info starts");

    // Then: both successful outputs are versioned JSON schemas.
    assert!(gpu.status.success());
    assert!(cache.status.success());
    let gpu_report = parse_stdout(&gpu);
    let cache_report = parse_stdout(&cache);
    assert_eq!(gpu_report["schema_version"], 1);
    assert!(matches!(
        gpu_report["capability_state"].as_str(),
        Some("unavailable" | "preflight_ok")
    ));
    assert_eq!(cache_report["schema_version"], 1);
    assert!(cache_report["valid_ascii"].is_boolean());
    assert!(cache_report["published_prefix_sha256"].is_string());
}

#[test]
fn named_test_artifact_prefix_isolated_and_manifested() {
    // Given: two distinct path-safe artifact prefixes.
    let root = TempDir::new().expect("temporary root");
    let evidence = root.path().join("evidence");
    let target_dir = root.path().join("named-test-target");
    let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("scripts/run-named-test.sh");

    // When: the same named test runs under both prefixes.
    let first = named_test_command(&script, &target_dir)
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .args([
            "--evidence-dir",
            evidence.to_str().expect("utf8 path"),
            "--artifact-prefix",
            "task-1-prefix-a",
            "--timeout-seconds",
            "300",
            "--test-target",
            "cli",
            "test_version",
        ])
        .output()
        .expect("first named test starts");
    let second = named_test_command(&script, &target_dir)
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .args([
            "--evidence-dir",
            evidence.to_str().expect("utf8 path"),
            "--artifact-prefix",
            "task-1-prefix-b",
            "--timeout-seconds",
            "300",
            "--test-target",
            "cli",
            "test_version",
        ])
        .output()
        .expect("second named test starts");

    // Then: each run owns a non-overlapping immutable path manifest.
    assert!(
        first.status.success(),
        "{}",
        String::from_utf8_lossy(&first.stderr)
    );
    assert!(
        second.status.success(),
        "{}",
        String::from_utf8_lossy(&second.stderr)
    );
    for prefix in ["task-1-prefix-a", "task-1-prefix-b"] {
        let manifest = evidence
            .join("named-tests")
            .join(prefix)
            .join("test_version.paths.json");
        let value: Value = serde_json::from_slice(&std::fs::read(manifest).expect("path manifest"))
            .expect("path manifest JSON");
        assert_eq!(value["artifact_prefix"], prefix);
        assert!(
            value["raw"]
                .as_str()
                .is_some_and(|path| path.contains(prefix))
        );
    }
}
