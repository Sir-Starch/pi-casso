use std::fs;
use std::path::{Path, PathBuf};

use assert_cmd::Command;
use rusqlite::{Connection, params};
use serde_json::Value;
use tempfile::TempDir;

fn command(data_dir: &Path) -> Command {
    let mut command = Command::cargo_bin("pi-casso").expect("pi-casso binary");
    command.env("PI_CASSO_DATA_DIR", data_dir);
    command
}

fn seed_paused_run(root: &TempDir, name: &str) -> (PathBuf, String) {
    // Given: a finite run whose zero limit creates a resumable checkpoint without search work.
    let art = root.path().join(format!("{name}.art"));
    let digits = root.path().join(format!("{name}.digits"));
    fs::write(&art, "##\n##\n").expect("art fixture");
    fs::write(
        &digits,
        "314159265358979323846264338327950288419716939937510",
    )
    .expect("digit fixture");
    let data_dir = root.path().join(format!("{name}-data"));

    let output = command(&data_dir)
        .args([
            "--json",
            "start",
            "--file",
            art.to_str().expect("UTF-8 art path"),
            "--name",
            name,
            "--width",
            "2",
            "--height",
            "2",
            "--match-mode",
            "threshold",
            "--pi-file",
            digits.to_str().expect("UTF-8 digit path"),
            "--no-tui",
            "--limit",
            "0",
            "--work-windows",
            "7",
            "--backend",
            "cpu",
            "--gpu",
            "off",
        ])
        .output()
        .expect("seed command runs");
    assert!(
        output.status.success(),
        "seed failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let run: Value = serde_json::from_slice(&output.stdout).expect("seed JSON");
    let run_id = run["id"].as_str().expect("run id").to_string();
    (data_dir, run_id)
}

fn persisted_state(data_dir: &Path, run_id: &str) -> (i64, i64, String) {
    let connection = Connection::open(data_dir.join("pi-casso.db")).expect("database opens");
    connection
        .query_row(
            "SELECT current_offset, scanned_windows, params_json FROM runs WHERE id = ?1",
            [run_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .expect("persisted run")
}

#[test]
fn snapshot_incompatible_preserves_progress_and_params() {
    let root = TempDir::new().expect("temporary root");
    let (data_dir, run_id) = seed_paused_run(&root, "snapshot-incompatible");
    let connection = Connection::open(data_dir.join("pi-casso.db")).expect("database opens");
    let incompatible = serde_json::json!({
        "performance_snapshot": {"schema_version": 99},
        "checkpoint": {"stop_reason": "seed", "checkpoint_sequence": 4}
    })
    .to_string();
    connection
        .execute(
            "UPDATE runs SET params_json = ?2 WHERE id = ?1",
            params![run_id, incompatible],
        )
        .expect("inject incompatible snapshot");
    drop(connection);
    let before = persisted_state(&data_dir, &run_id);

    // When: JSON resume crosses the shared snapshot decoder.
    let output = command(&data_dir)
        .args(["--json", "resume", &run_id, "--no-tui"])
        .output()
        .expect("resume command runs");

    // Then: it is a typed exit-2 response and neither progress nor params changed.
    assert_eq!(output.status.code(), Some(2));
    let error: Value = serde_json::from_slice(&output.stdout).expect("typed error JSON");
    assert_eq!(error["status"], "snapshot_incompatible");
    assert_eq!(error["snapshot_schema_version"], 99);
    assert!(!error["reason"].as_str().unwrap_or_default().is_empty());
    assert_eq!(persisted_state(&data_dir, &run_id), before);
}

#[test]
fn resume_cli_tui_atomic_round_trip() {
    let root = TempDir::new().expect("temporary root");
    let (data_dir, run_id) = seed_paused_run(&root, "atomic-round-trip");

    // When: CLI resume prepares distinct count bounds but performs no search work.
    let output = command(&data_dir)
        .args([
            "--json",
            "resume",
            &run_id,
            "--no-tui",
            "--work-windows",
            "3",
            "--limit",
            "0",
            "--backend",
            "cpu",
            "--gpu",
            "off",
        ])
        .output()
        .expect("resume command runs");
    assert!(
        output.status.success(),
        "resume failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    // Then: the one persisted snapshot consumed by either CLI or TUI retains both bounds.
    let (_, _, params) = persisted_state(&data_dir, &run_id);
    let params: Value = serde_json::from_str(&params).expect("persisted params JSON");
    assert_eq!(params["performance_snapshot"]["work_windows"], 3);
    assert_eq!(params["performance_snapshot"]["limit"], 0);
    assert_eq!(params["performance_snapshot"]["current_offset"], 0);
}

#[test]
fn strict_resume_preflight_rejects_before_digit_source_open() {
    let root = TempDir::new().expect("temporary root");
    let (data_dir, run_id) = seed_paused_run(&root, "strict-preflight");
    let before = persisted_state(&data_dir, &run_id);

    // When: an explicit current-host adapter cannot pass strict wgpu preflight.
    let output = command(&data_dir)
        .env("PI_CASSO_TEST_MODE", "1")
        .env("PI_CASSO_TEST_FAIL_IF_SOURCE_OPEN", "1")
        .args([
            "--json",
            "resume",
            &run_id,
            "--no-tui",
            "--backend",
            "gpu",
            "--gpu",
            "on",
            "--gpu-device",
            "task13-device-that-does-not-exist",
            "--work-windows",
            "4096",
        ])
        .output()
        .expect("resume command runs");

    // Then: preflight exits 2, the source failpoint is not reached, and persistence is untouched.
    assert_eq!(output.status.code(), Some(2));
    let error: Value = serde_json::from_slice(&output.stdout).expect("typed error JSON");
    assert_eq!(error["status"], "unsupported");
    assert!(!String::from_utf8_lossy(&output.stderr).contains("FAIL_IF_SOURCE_OPEN reached"));
    assert_eq!(persisted_state(&data_dir, &run_id), before);
}

#[test]
fn resumed_work_windows_is_measured_from_checkpoint() {
    // Given: a run that stopped after exactly two windows.
    let root = TempDir::new().expect("temporary root");
    let art = root.path().join("resume-bounds.art");
    let digits = root.path().join("resume-bounds.digits");
    let data_dir = root.path().join("resume-bounds-data");
    fs::write(&art, "##\n##\n").expect("art fixture");
    fs::write(
        &digits,
        "314159265358979323846264338327950288419716939937510",
    )
    .expect("digit fixture");
    let start = command(&data_dir)
        .args([
            "start",
            "--file",
            art.to_str().expect("UTF-8 art path"),
            "--name",
            "resume-bounds",
            "--width",
            "2",
            "--height",
            "2",
            "--match-mode",
            "threshold",
            "--pi-file",
            digits.to_str().expect("UTF-8 digit path"),
            "--no-tui",
            "--work-windows",
            "2",
            "--keep-going-after-perfect",
            "--backend",
            "cpu",
            "--gpu",
            "off",
        ])
        .output()
        .expect("start command runs");
    assert!(start.status.success());
    let connection = Connection::open(data_dir.join("pi-casso.db")).expect("database opens");
    let run_id: String = connection
        .query_row(
            "SELECT id FROM runs WHERE name = 'resume-bounds'",
            [],
            |row| row.get(0),
        )
        .expect("run id");
    drop(connection);
    assert_eq!(persisted_state(&data_dir, &run_id).1, 2);

    // When: resume omits the bound and restores the persisted two-window budget.
    let resume = command(&data_dir)
        .args(["resume", &run_id, "--no-tui", "--keep-going-after-perfect"])
        .output()
        .expect("resume command runs");

    // Then: two additional windows are scanned from the checkpoint, for four total.
    assert!(
        resume.status.success(),
        "resume failed: {}",
        String::from_utf8_lossy(&resume.stderr)
    );
    let (current_offset, scanned_windows, _) = persisted_state(&data_dir, &run_id);
    assert_eq!(scanned_windows, 4);
    assert_eq!(current_offset, 4);
}
