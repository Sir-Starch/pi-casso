use std::fs;

use assert_cmd::Command;
use serde_json::Value;
use tempfile::TempDir;

#[test]
fn strict_start_preflight_rejects_before_digit_source_open() {
    // Given: a finite file source and a device name that cannot exist on the current host.
    let root = TempDir::new().expect("temporary root");
    let art = root.path().join("strict-start.art");
    let digits = root.path().join("strict-start.digits");
    let data_dir = root.path().join("strict-start-data");
    fs::write(&art, "##\n##\n").expect("art fixture");
    fs::write(
        &digits,
        "314159265358979323846264338327950288419716939937510",
    )
    .expect("digit fixture");

    // When: explicit wgpu selection is preflighted with the source-open failpoint armed.
    let output = Command::cargo_bin("pi-casso")
        .expect("pi-casso binary")
        .env("PI_CASSO_DATA_DIR", &data_dir)
        .env("PI_CASSO_TEST_MODE", "1")
        .env("PI_CASSO_TEST_FAIL_IF_SOURCE_OPEN", "1")
        .args([
            "--json",
            "start",
            "--file",
            art.to_str().expect("UTF-8 art path"),
            "--name",
            "strict-start",
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
            "4096",
            "--backend",
            "gpu",
            "--gpu",
            "on",
            "--gpu-device",
            "task13-device-that-does-not-exist",
        ])
        .output()
        .expect("start command runs");

    // Then: typed preflight failure occurs before source opening or run creation.
    assert_eq!(output.status.code(), Some(2));
    let error: Value = serde_json::from_slice(&output.stdout).expect("typed error JSON");
    assert_eq!(error["status"], "unsupported");
    assert!(!String::from_utf8_lossy(&output.stderr).contains("FAIL_IF_SOURCE_OPEN reached"));
    assert!(!data_dir.join("pi-casso.db").exists());
}
