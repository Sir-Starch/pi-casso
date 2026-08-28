use std::fs;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use serde_json::Value;
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

fn shell_path(path: &Path) -> String {
    #[cfg(windows)]
    {
        let value = path.to_string_lossy().replace('\\', "/");
        if value.as_bytes().get(1) == Some(&b':') {
            let drive = value
                .chars()
                .next()
                .expect("drive letter")
                .to_ascii_lowercase();
            return format!("/{drive}{}", &value[2..]);
        }
        value
    }
    #[cfg(not(windows))]
    {
        path.display().to_string()
    }
}

fn isolated_pi_casso(root: &TempDir) -> Command {
    let mut command = Command::new(assert_cmd::cargo::cargo_bin!("pi-casso"));
    command
        .env_remove("PI_CASSO_DATA_DIR")
        .env_remove("PI_CASSO_CONFIG")
        .env("XDG_DATA_HOME", root.path().join("data"))
        .env("XDG_CONFIG_HOME", root.path().join("config"))
        .env("TMPDIR", root.path().join("tmp"));
    command
}

fn benchmark_command(root: &TempDir) -> Command {
    let mut command = isolated_pi_casso(root);
    command.args([
        "--json",
        "benchmark",
        "--template",
        "arch",
        "--source-mode",
        "finite",
        "--work-windows",
        "2",
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
    command
}

fn parse_stdout(output: &Output) -> Value {
    serde_json::from_slice(&output.stdout).expect("stdout is JSON")
}

#[test]
fn negative_max_offset_returns_structured_selection_error() {
    // Given: an isolated JSON benchmark invocation.
    let root = TempDir::new().expect("temporary root");
    let mut command = benchmark_command(&root);
    command.args([
        "--cache-state",
        "cold",
        "--repetitions",
        "1",
        "--warmup",
        "0",
        "--max-offset",
        "-1",
    ]);

    // When: a negative maximum offset crosses the public CLI boundary.
    let output = command.output().expect("benchmark starts");

    // Then: the product returns the version-1 pre-work selection-error envelope.
    assert_eq!(output.status.code(), Some(2));
    let report = parse_stdout(&output);
    assert_eq!(report["schema_version"], 1);
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
fn warm_cache_without_warmups_reports_not_warmed() {
    // Given: a warm-cache benchmark with zero untimed warm-up repetitions.
    let root = TempDir::new().expect("temporary root");
    let mut command = benchmark_command(&root);
    command.args([
        "--cache-state",
        "warm",
        "--repetitions",
        "2",
        "--warmup",
        "0",
    ]);

    // When: both measured repetitions execute through the real CLI.
    let output = command.output().expect("benchmark starts");

    // Then: neither the summary nor either raw repetition claims a warm-up occurred.
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report = parse_stdout(&output);
    assert_eq!(report["warm_up_completed"], false);
    assert!(report["raw_runs"].as_array().is_some_and(
        |runs| runs.len() == 2 && runs.iter().all(|run| run["warm_up_completed"] == false)
    ));
}

#[test]
fn baseline_runner_passes_y_cruncher_identity_to_product() {
    // Given: an executable y-cruncher stand-in and a one-run finite baseline.
    let root = TempDir::new().expect("temporary root");
    let executable = root.path().join("fake-y-cruncher.exe");
    fs::write(&executable, b"#!/usr/bin/env bash\nexit 0\n").expect("fake executable");
    #[cfg(unix)]
    {
        let mut permissions = fs::metadata(&executable).expect("metadata").permissions();
        permissions.set_mode(0o700);
        fs::set_permissions(&executable, permissions).expect("executable permissions");
    }
    let output_dir = root.path().join("evidence");
    let xdg_root = root.path().join("xdg");
    let executable_path = shell_path(&executable);

    // When: the baseline runner receives the required optional interface value.
    let output = script_command("run-benchmark-baseline.sh")
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .env_remove("PI_CASSO_DATA_DIR")
        .env_remove("PI_CASSO_CONFIG")
        .args([
            "--output-dir",
            output_dir.to_str().expect("output path"),
            "--artifact-prefix",
            "task-1-y-cruncher-interface",
            "--scenario",
            "finite-cold",
            "--source-mode",
            "finite",
            "--cache-state",
            "cold",
            "--xdg-root",
            xdg_root.to_str().expect("xdg path"),
            "--work-windows",
            "2",
            "--repetitions",
            "1",
            "--warmup",
            "0",
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
        ])
        .arg("--y-cruncher-path")
        .arg(&executable_path)
        .output()
        .expect("baseline runner starts");

    // Then: the product report records presence and the executable-byte digest.
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let summary = output_dir.join("task-1-y-cruncher-interface-finite-cold-raw.json");
    let report: Value =
        serde_json::from_slice(&fs::read(summary).expect("summary")).expect("summary JSON");
    assert_eq!(report["workload_identity"]["y_cruncher_path_present"], true);
    assert_eq!(
        report["workload_identity"]["y_cruncher_executable_sha256"],
        format!(
            "{:x}",
            Sha256::digest(fs::read(executable).expect("executable bytes"))
        )
    );
}

fn script(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("scripts")
        .join(name)
}
