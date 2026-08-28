use std::fs;
use std::path::{Path, PathBuf};

use assert_cmd::Command;
use serde_json::Value;

pub const KNOWN_PI_PREFIX: &str = "3141592653589793238462643383279502884197169399375105820974944592307816406286208998628034825342117067";

pub fn pi_casso(data_dir: &Path) -> Command {
    let mut command = Command::cargo_bin("pi-casso").unwrap();
    command.env("PI_CASSO_DATA_DIR", data_dir);
    command
}

pub fn publish_cache(data_dir: &Path, digits: &[u8]) {
    let source = data_dir.join("task3-cache-seed.txt");
    fs::write(&source, digits).unwrap();
    pi_casso(data_dir)
        .args(["pi", "import"])
        .arg(&source)
        .assert()
        .success();
    fs::remove_file(source).unwrap();
}

pub fn write_art(root: &Path) -> PathBuf {
    let path = root.join("diagonal.txt");
    fs::write(&path, "#.\n.#\n").unwrap();
    path
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
