use std::fs;
use std::path::{Path, PathBuf};

use assert_cmd::Command;
use serde_json::Value;

pub fn pi_casso(data_dir: &Path) -> Command {
    let mut command = Command::cargo_bin("pi-casso").unwrap();
    command.env("PI_CASSO_DATA_DIR", data_dir);
    command
}

pub fn write_art(root: &Path) -> PathBuf {
    let path = root.join("diagonal.txt");
    fs::write(&path, "#.\n.#\n").unwrap();
    path
}

pub fn write_digits(root: &Path, name: &str, digits: &str) -> PathBuf {
    let path = root.join(name);
    fs::write(&path, digits).unwrap();
    path
}

pub fn start_finite(data_dir: &Path, art: &Path, digits: &Path, name: &str, bounds: &[&str]) {
    pi_casso(data_dir)
        .args(["start", "--file"])
        .arg(art)
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
            name,
            "--match-mode",
            "emergence",
            "--canvas-width",
            "2",
            "--canvas-height",
            "2",
            "--pi-file",
        ])
        .arg(digits)
        .args([
            "--no-tui",
            "--keep-going-after-perfect",
            "--backend",
            "cpu",
            "--gpu",
            "off",
            "--cpu-workers",
            "1",
            "--chunk-size",
            "2",
            "--top",
            "8",
        ])
        .args(bounds)
        .assert()
        .success();
}

pub fn status(data_dir: &Path, name: &str) -> Value {
    let output = pi_casso(data_dir)
        .args(["--json", "status", name])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}

pub fn diagnostic_score_q(score: f64) -> u32 {
    ((score.max(0.0) * 1_000_000.0 + 0.5)
        .floor()
        .min(1_000_000.0)) as u32
}
