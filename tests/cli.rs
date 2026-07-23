use assert_cmd::Command;
use predicates::prelude::*;
use std::fs;
use tempfile::tempdir;

fn pi_casso_cmd() -> Command {
    Command::cargo_bin("pi-casso").unwrap()
}

#[test]
fn test_help() {
    pi_casso_cmd()
        .arg("--help")
        .assert()
        .success()
        .stdout(predicate::str::contains("Find ASCII art"));
}

#[test]
fn test_version() {
    pi_casso_cmd()
        .arg("--version")
        .assert()
        .success()
        .stdout(predicate::str::contains("pi-casso"));
}

#[test]
fn test_templates_list() {
    pi_casso_cmd()
        .arg("templates")
        .assert()
        .success()
        .stdout(predicate::str::contains("arch").and(predicate::str::contains("pi")));
}

#[test]
fn test_template_preview() {
    pi_casso_cmd()
        .args(["preview", "--template", "arch"])
        .assert()
        .success()
        .stdout(predicate::str::contains("#"));
}

#[test]
fn test_invalid_digit_file() {
    let dir = tempdir().unwrap();
    let bad_file = dir.path().join("bad_digits.txt");
    fs::write(&bad_file, "3.14159 abcdef").unwrap();

    let data_dir = dir.path().join("data");

    pi_casso_cmd()
        .env("XDG_DATA_HOME", data_dir.to_str().unwrap())
        .args([
            "start",
            "--template",
            "pi",
            "--name",
            "test-bad",
            "--pi-file",
            bad_file.to_str().unwrap(),
            "--no-tui",
            "--limit",
            "100",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains("invalid"));
}

#[test]
fn test_finite_search_and_json_export() {
    let dir = tempdir().unwrap();
    let digits_file = dir.path().join("digits.txt");
    fs::write(&digits_file, "31415926535897932384626433832795028841971693993751058209749445923078164062862089986280348253421170679").unwrap();
    let data_dir = dir.path().join("data");

    // 1. Start search
    pi_casso_cmd()
        .env("XDG_DATA_HOME", data_dir.to_str().unwrap())
        .args([
            "start",
            "--template",
            "pi",
            "--name",
            "test-search",
            "--pi-file",
            digits_file.to_str().unwrap(),
            "--no-tui",
            "--limit",
            "10",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("finished"));

    // 2. Status
    pi_casso_cmd()
        .env("XDG_DATA_HOME", data_dir.to_str().unwrap())
        .args(["status", "test-search"])
        .assert()
        .success()
        .stdout(predicate::str::contains("test-search"));

    // 3. JSON Export
    pi_casso_cmd()
        .env("XDG_DATA_HOME", data_dir.to_str().unwrap())
        .args(["export", "test-search", "--format", "json"])
        .assert()
        .success()
        .stdout(predicate::str::starts_with("{"));
}

#[test]
fn test_resume_search() {
    let dir = tempdir().unwrap();
    let digits_file = dir.path().join("digits.txt");
    fs::write(&digits_file, "31415926535897932384626433832795028841971693993751058209749445923078164062862089986280348253421170679".repeat(10)).unwrap();
    let data_dir = dir.path().join("data");

    pi_casso_cmd()
        .env("XDG_DATA_HOME", data_dir.to_str().unwrap())
        .args([
            "start",
            "--template",
            "pi",
            "--name",
            "test-resume",
            "--mode",
            "8x8",
            "--pi-file",
            digits_file.to_str().unwrap(),
            "--no-tui",
            "--max-offset",
            "5",
        ])
        .assert()
        .success();

    pi_casso_cmd()
        .env("XDG_DATA_HOME", data_dir.to_str().unwrap())
        .args(["resume", "test-resume", "--no-tui", "--max-offset", "10"])
        .assert()
        .success()
        .stdout(predicate::str::contains("finished"));
}
