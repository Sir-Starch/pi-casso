use std::fs;
use std::path::{Path, PathBuf};

use assert_cmd::Command;
use serde_json::Value;
use tempfile::tempdir;

fn pi_casso(data_dir: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_pi-casso"));
    command.env("PI_CASSO_DATA_DIR", data_dir);
    command
}

fn write_art(root: &Path) -> PathBuf {
    let path = root.join("target.txt");
    fs::write(&path, "#.\n.#\n").unwrap();
    path
}

#[test]
fn digit_source_rejects_short_or_non_digit_range() {
    // Given: one finite source shorter than the requested work and one malformed source.
    let root = tempdir().unwrap();
    let art = write_art(root.path());
    let short = root.path().join("short.txt");
    let malformed = root.path().join("malformed.txt");
    fs::write(&short, "31415").unwrap();
    fs::write(&malformed, "314x5").unwrap();

    // When: the short source is searched through the real CLI.
    let short_output = pi_casso(&root.path().join("short-data"))
        .args(["--json", "start", "--file"])
        .arg(&art)
        .args([
            "--width",
            "2",
            "--height",
            "2",
            "--empty",
            ".",
            "--filled",
            "#",
            "--name",
            "short",
            "--match-mode",
            "emergence",
            "--canvas-width",
            "2",
            "--canvas-height",
            "2",
            "--pi-file",
        ])
        .arg(&short)
        .args([
            "--limit",
            "4",
            "--no-tui",
            "--yes",
            "--keep-going-after-perfect",
            "--backend",
            "cpu",
            "--gpu",
            "off",
            "--chunk-size",
            "4",
            "--queue-depth",
            "1",
        ])
        .output()
        .unwrap();

    // Then: only the two complete windows are counted and exhaustion is explicit.
    assert!(short_output.status.success());
    let status_output = pi_casso(&root.path().join("short-data"))
        .args(["--json", "status", "short"])
        .output()
        .unwrap();
    assert!(status_output.status.success());
    let short_json: Value = serde_json::from_slice(&status_output.stdout).unwrap();
    assert_eq!(short_json["scanned_windows"], 2);
    assert_eq!(short_json["status"], "source_exhausted");

    // When/Then: malformed ASCII fails before scoring rather than shifting offsets.
    pi_casso(&root.path().join("malformed-data"))
        .args(["start", "--file"])
        .arg(&art)
        .args([
            "--width",
            "2",
            "--height",
            "2",
            "--empty",
            ".",
            "--filled",
            "#",
            "--name",
            "malformed",
            "--match-mode",
            "emergence",
            "--canvas-width",
            "2",
            "--canvas-height",
            "2",
            "--pi-file",
        ])
        .arg(&malformed)
        .args(["--no-tui", "--yes", "--backend", "cpu", "--gpu", "off"])
        .assert()
        .failure();
}
