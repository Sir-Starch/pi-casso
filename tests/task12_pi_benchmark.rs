#[cfg(windows)]
use std::fs;
#[cfg(not(windows))]
use std::path::Path;
#[cfg(windows)]
use std::path::PathBuf;
use std::process::{Command, Output};

use serde_json::Value;
use tempfile::TempDir;

#[cfg(target_env = "msvc")]
const TEST_BUILTIN_VARIANT: &str = "spigot-persistent";

#[cfg(not(target_env = "msvc"))]
const TEST_BUILTIN_VARIANT: &str = "chudnovsky-rug-binary-split";

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
fn pi_benchmark_reports_serial_generation_contract() {
    // Given: an isolated one-repetition built-in benchmark.
    let root = TempDir::new().expect("temporary root");
    let mut command = benchmark(&root);
    command.args([
        "--targets",
        "100,1000",
        "--repetitions",
        "1",
        "--warmup",
        "0",
        "--generator-backend",
        "cpu",
        "--workers",
        "1",
    ]);

    // When: the public pi benchmark command runs.
    let output = command.output().expect("pi benchmark starts");

    // Then: its JSON exposes the stable version-1 metrics and identity contract.
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let value = report(&output);
    assert_eq!(value["schema_version"], 1);
    assert_eq!(value["status"], "ok");
    assert_eq!(value["targets"], serde_json::json!([100, 1000]));
    assert_eq!(value["demand_mode"], "serial");
    assert_eq!(value["selected_backend"], "cpu");
    assert_eq!(value["selected_variant"], TEST_BUILTIN_VARIANT);
    assert!(
        value["median"]["generated_source_digits_per_second"]
            .as_f64()
            .is_some_and(|rate| rate > 0.0)
    );
    assert!(value["generation_wait_ms"].is_number());
    assert!(value["memory"]["logical_peak_mb"].is_number());
    assert!(
        value["workload_id"]
            .as_str()
            .is_some_and(|id| id.starts_with("pi-v1-"))
    );
    assert_eq!(
        value["workload_identity"]["selected_variant"],
        value["selected_variant"]
    );
}

#[test]
fn concurrent_forced_variant_coalesces_four_requests() {
    // Given: four absolute targets and an allowlisted test-only forced variant.
    let root = TempDir::new().expect("temporary root");
    let mut command = benchmark(&root);
    command
        .env("PI_CASSO_TEST_MODE", "1")
        .env("PI_CASSO_TEST_GENERATOR_VARIANT", TEST_BUILTIN_VARIANT)
        .args([
            "--targets",
            "100,200,300,1000",
            "--demand-mode",
            "concurrent",
            "--repetitions",
            "1",
            "--warmup",
            "0",
            "--generator-backend",
            "cpu",
            "--workers",
            "2",
        ]);

    // When: all submissions cross the benchmark barrier.
    let output = command.output().expect("pi benchmark starts");

    // Then: one producer epoch coalesces all four requests.
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let value = report(&output);
    assert_eq!(value["demand_mode"], "concurrent");
    assert_eq!(value["concurrent_requests"], 4);
    assert_eq!(value["coalesced_request_count"], 4);
    assert_eq!(value["producer_epochs"], 1);
    assert!(
        value["generation_batches"]
            .as_u64()
            .is_some_and(|count| count >= 1)
    );
    assert_eq!(value["correctness"], true);
}

#[test]
fn explicit_missing_ycruncher_is_typed_redacted_unavailable() {
    // Given: an explicitly selected missing external executable.
    let root = TempDir::new().expect("temporary root");
    let missing = "/definitely/missing/y-cruncher-task12";
    let mut command = benchmark(&root);
    command.args([
        "--targets",
        "100",
        "--repetitions",
        "1",
        "--warmup",
        "0",
        "--generator-backend",
        "y-cruncher",
        "--y-cruncher-path",
        missing,
    ]);

    // When: backend preflight runs before generation.
    let output = command.output().expect("pi benchmark starts");

    // Then: exit 2 carries typed sentinels and neither output stream leaks the path.
    assert_eq!(output.status.code(), Some(2));
    assert!(!String::from_utf8_lossy(&output.stdout).contains(missing));
    assert!(!String::from_utf8_lossy(&output.stderr).contains(missing));
    let value = report(&output);
    assert_eq!(value["status"], "unavailable");
    assert_eq!(value["reason"], "executable_missing");
    assert_eq!(value["selected_backend"], "");
    assert_eq!(value["generator_executable_sha256"], "");
    assert_eq!(
        value["workload_identity"]["generator_backend"],
        "y-cruncher-external"
    );
    assert_eq!(value["workload_identity"]["y_cruncher_path_present"], false);
    assert_eq!(
        value["workload_identity"]["y_cruncher_executable_sha256"],
        ""
    );
    assert_eq!(
        value["unavailable_backends"][0]["reason"],
        "executable_missing"
    );
}

#[test]
fn ycruncher_missing_identity_is_deterministic() {
    // Given: repeated auto and explicit requests with the same invalid preferred candidate.
    let first_root = TempDir::new().expect("first root");
    let second_root = TempDir::new().expect("second root");
    let run = |root: &TempDir, backend: &str| {
        let mut command = benchmark(root);
        command.env("PATH", root.path().join("empty-path"));
        command.args([
            "--targets",
            "100",
            "--repetitions",
            "1",
            "--warmup",
            "0",
            "--generator-backend",
            backend,
            "--y-cruncher-path",
            "/definitely/missing/y-cruncher-task12",
        ]);
        command.output().expect("pi benchmark starts")
    };

    // When: both invocations resolve their generator.
    let first = run(&first_root, "auto");
    let second = run(&second_root, "auto");
    let explicit_first = run(&first_root, "y-cruncher");
    let explicit_second = run(&second_root, "y-cruncher");

    // Then: CPU fallback succeeds and volatile cache paths do not affect identity.
    assert!(first.status.success());
    assert!(second.status.success());
    let first = report(&first);
    let second = report(&second);
    assert_eq!(first["status"], "ok");
    assert_eq!(first["selected_backend"], "cpu");
    assert_eq!(first["fallback"], true);
    assert_eq!(first["fallback_reason"], "executable_missing");
    assert_eq!(first["generator_executable_sha256"], "");
    assert_eq!(first["workload_identity"]["generator_backend"], "cpu");
    assert_eq!(first["workload_id"], second["workload_id"]);
    assert_eq!(explicit_first.status.code(), Some(2));
    assert_eq!(explicit_second.status.code(), Some(2));
    let explicit_first = report(&explicit_first);
    let explicit_second = report(&explicit_second);
    assert_eq!(explicit_first["selected_variant"], "y-cruncher-external");
    assert_eq!(
        explicit_first["workload_id"],
        explicit_second["workload_id"]
    );
}

#[test]
fn builtin_variant_is_not_a_public_generator_backend() {
    // Given: a benchmark invocation trying to use an internal label publicly.
    let root = TempDir::new().expect("temporary root");
    let mut command = benchmark(&root);
    command.args([
        "--targets",
        "100",
        "--generator-backend",
        "spigot-persistent",
    ]);

    // When: clap parses the public boundary.
    let output = command.output().expect("pi benchmark starts");

    // Then: only the existing cpu/y-cruncher/auto enum remains accepted.
    assert_eq!(output.status.code(), Some(2));
}

#[test]
fn fixture_backed_ycruncher_imports_into_a_fresh_cache() {
    // Given: a contract-faithful external generator and no existing cache file.
    let root = TempDir::new().expect("temporary root");
    #[cfg(windows)]
    let executable = root.path().join("y-cruncher.cmd");
    #[cfg(not(windows))]
    let executable = root.path().join("y-cruncher");
    let fixture_status = {
        #[cfg(windows)]
        {
            let bash = git_bash();
            fs::copy(
                std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join("scripts/create-ycruncher-fixture.sh"),
                root.path().join("create-ycruncher-fixture.sh"),
            )
            .expect("fixture creator copies");
            let status = Command::new(&bash)
                .current_dir(root.path())
                .args([
                    "create-ycruncher-fixture.sh",
                    "--output",
                    "y-cruncher.sh",
                    "--digits",
                    "2000",
                ])
                .status()
                .expect("fixture creator starts");
            fs::write(
                &executable,
                format!(
                    "@echo off\r\n\"{}\" \"%~dp0y-cruncher.sh\" %*\r\n",
                    bash.display()
                ),
            )
            .expect("Windows fixture wrapper");
            status
        }
        #[cfg(not(windows))]
        {
            Command::new(
                Path::new(env!("CARGO_MANIFEST_DIR")).join("scripts/create-ycruncher-fixture.sh"),
            )
            .args(["--output"])
            .arg(&executable)
            .args(["--digits", "2000"])
            .status()
            .expect("fixture creator starts")
        }
    };
    assert!(fixture_status.success());
    let mut command = benchmark(&root);
    command
        .env("PI_CASSO_TEST_MODE", "1")
        .env("PI_CASSO_TEST_GENERATOR_VARIANT", "y-cruncher-external")
        .args([
            "--targets",
            "100,1000",
            "--repetitions",
            "1",
            "--warmup",
            "0",
            "--generator-backend",
            "cpu",
            "--workers",
            "1",
            "--y-cruncher-path",
        ])
        .arg(&executable);

    // When: the external output is imported into the benchmark's fresh cache.
    let output = command.output().expect("pi benchmark starts");

    // Then: publication creates the cache and reports a correct external run.
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let value = report(&output);
    assert_eq!(value["status"], "ok");
    assert_eq!(value["selected_variant"], "y-cruncher-external");
    assert_eq!(value["correctness"], true);
}
