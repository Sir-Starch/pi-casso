use std::fs;
use std::path::Path;

use assert_cmd::Command;
use rusqlite::{Connection, params};
use serde_json::Value;
use tempfile::TempDir;

fn command(data_dir: &Path) -> Command {
    let mut command = Command::cargo_bin("pi-casso").expect("pi-casso binary");
    command.env("PI_CASSO_DATA_DIR", data_dir);
    command
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
fn resumed_checkpoint_preserves_snapshot_bounds_and_unknown_fields() {
    // Given: a paused run whose configured bound is seven, with two windows already scanned.
    let root = TempDir::new().expect("temporary root");
    let art = root.path().join("checkpoint-snapshot.art");
    let digits = root.path().join("checkpoint-snapshot.digits");
    let data_dir = root.path().join("checkpoint-snapshot-data");
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
            "checkpoint-snapshot",
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
            "2",
            "--work-windows",
            "7",
            "--keep-going-after-perfect",
            "--backend",
            "cpu",
            "--gpu",
            "off",
        ])
        .output()
        .expect("start command runs");
    assert!(
        start.status.success(),
        "start failed: {}",
        String::from_utf8_lossy(&start.stderr)
    );
    let connection = Connection::open(data_dir.join("pi-casso.db")).expect("database opens");
    let run_id: String = connection
        .query_row(
            "SELECT id FROM runs WHERE name = 'checkpoint-snapshot'",
            [],
            |row| row.get(0),
        )
        .expect("run id");
    drop(connection);
    let (_, scanned_windows, params_json) = persisted_state(&data_dir, &run_id);
    assert_eq!(scanned_windows, 2);

    let mut params: Value = serde_json::from_str(&params_json).expect("params JSON");
    params["performance_snapshot"]["future_field"] = serde_json::json!({"keep": true});
    let connection = Connection::open(data_dir.join("pi-casso.db")).expect("database opens");
    connection
        .execute(
            "UPDATE runs SET params_json = ?2 WHERE id = ?1",
            params![run_id, params.to_string()],
        )
        .expect("inject unknown snapshot field");
    drop(connection);

    // When: resume performs a real session checkpoint after enough delayed work to make the
    // one-second checkpoint interval due.
    let resume = command(&data_dir)
        .env("PI_CASSO_TEST_MODE", "1")
        .env("PI_CASSO_TEST_CONSUMER_DELAY_MS", "600")
        .args([
            "resume",
            &run_id,
            "--no-tui",
            "--checkpoint-every",
            "1",
            "--chunk-size",
            "1",
        ])
        .output()
        .expect("resume command runs");

    // Then: the session checkpoint keeps the configured bound, unknown data, and booleans.
    assert!(
        resume.status.success(),
        "resume failed: {}",
        String::from_utf8_lossy(&resume.stderr)
    );
    let (current_offset, scanned_windows, params_json) = persisted_state(&data_dir, &run_id);
    assert!(current_offset > 2);
    assert!(scanned_windows > 2);
    let params: Value = serde_json::from_str(&params_json).expect("persisted params JSON");
    assert_eq!(params["performance_snapshot"]["work_windows"], 7);
    assert_eq!(params["performance_snapshot"]["future_field"]["keep"], true);
    assert_eq!(
        params["performance_snapshot"]["keep_going_after_perfect"],
        true
    );
    assert_eq!(params["performance_snapshot"]["no_tui"], true);
}
