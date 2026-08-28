use std::fs;
use std::path::Path;
use std::process::{Command, Output};

#[cfg(windows)]
use std::path::PathBuf;

use serde_json::json;
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

#[test]
fn f4_audit_includes_staged_unstaged_and_new_untracked_paths() {
    // Given: allowed staged/unstaged changes, one unchanged pre-existing object, and one
    // new forbidden untracked object in an isolated repository.
    let root =
        TempDir::new_in(Path::new(env!("CARGO_MANIFEST_DIR"))).expect("temporary repository");
    prepare_repository(&root);
    fs::write(root.path().join("src/lib.rs"), "pub fn changed() {}\n").expect("unstaged file");
    fs::write(
        root.path().join("tests/staged.rs"),
        "#[test] fn staged() {}\n",
    )
    .expect("staged file");
    git(&root, &["add", "tests/staged.rs"]);
    fs::write(root.path().join("forbidden.tmp"), "new\n").expect("forbidden file");
    let base_sha = git_stdout(&root, &["rev-parse", "HEAD"]);
    write_base_state(&root, &base_sha);

    // When: F4 audits the complete current object state against the base commit/state.
    let result = run_audit(&root, &base_sha);

    // Then: the newly untracked forbidden path prevents a passing report.
    assert!(!result.status.success());
    let stderr = String::from_utf8_lossy(&result.stderr);
    assert!(
        stderr.contains("scope violation: forbidden.tmp"),
        "expected forbidden.tmp scope violation; status={:?}; stderr={stderr}",
        result.status
    );
    assert!(!root.path().join(".omo/evidence/report.json").exists());
}

fn project_script(name: &str) -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("scripts")
        .join(name)
}

fn git(root: &TempDir, args: &[&str]) {
    let output = Command::new("git")
        .current_dir(root.path())
        .args(args)
        .output()
        .expect("git starts");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn git_stdout(root: &TempDir, args: &[&str]) -> String {
    let output = Command::new("git")
        .current_dir(root.path())
        .args(args)
        .output()
        .expect("git starts");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout)
        .expect("git output is UTF-8")
        .trim()
        .to_string()
}

fn prepare_repository(root: &TempDir) {
    fs::create_dir_all(root.path().join("scripts")).expect("scripts directory");
    fs::create_dir_all(root.path().join("src")).expect("source directory");
    fs::create_dir_all(root.path().join("tests")).expect("tests directory");
    fs::create_dir_all(root.path().join(".codebase-memory")).expect("memory directory");
    fs::copy(
        project_script("run-f4-scope-audit.sh"),
        root.path().join("scripts/run-f4-scope-audit.sh"),
    )
    .expect("scope script");
    fs::copy(
        project_script("run-evidence-command.sh"),
        root.path().join("scripts/run-evidence-command.sh"),
    )
    .expect("evidence script");
    fs::write(root.path().join("src/lib.rs"), "pub fn original() {}\n").expect("source file");
    fs::write(
        root.path().join("tests/staged.rs"),
        "#[test] fn original() {}\n",
    )
    .expect("test file");
    git(root, &["init", "--quiet"]);
    git(root, &["config", "user.email", "task1@example.invalid"]);
    git(root, &["config", "user.name", "Task 1 Test"]);
    git(root, &["add", "scripts", "src", "tests"]);
    git(root, &["commit", "--quiet", "-m", "base"]);
    fs::write(root.path().join(".codebase-memory/artifact.json"), "{}\n")
        .expect("pre-existing object");
}

fn write_base_state(root: &TempDir, base_sha: &str) {
    let bytes =
        fs::read(root.path().join(".codebase-memory/artifact.json")).expect("pre-existing bytes");
    let state = json!({
        "base_sha": base_sha,
        "preexisting_untracked": [{
            "path": ".codebase-memory/artifact.json",
            "type": "regular",
            "bytes": bytes.len(),
            "sha256": format!("{:x}", Sha256::digest(&bytes))
        }]
    });
    fs::create_dir_all(root.path().join(".omo/evidence")).expect("base-state directory");
    fs::write(
        root.path().join(".omo/evidence/base-state.json"),
        serde_json::to_vec_pretty(&state).expect("base state JSON"),
    )
    .expect("base state");
}

fn run_audit(root: &TempDir, base_sha: &str) -> Output {
    #[cfg(windows)]
    let mut command = Command::new(git_bash());
    #[cfg(not(windows))]
    let mut command = Command::new("bash");
    command
        .current_dir(root.path())
        .env_remove("PI_CASSO_DATA_DIR")
        .env_remove("PI_CASSO_CONFIG")
        .args([
            "scripts/run-f4-scope-audit.sh",
            "--base-state",
            ".omo/evidence/base-state.json",
            "--base-sha",
            base_sha,
            "--final-git",
            base_sha,
            "--evidence-dir",
            ".omo/evidence",
            "--commands-json",
            ".omo/evidence/commands.json",
            "--log",
            ".omo/evidence/audit.log",
            "--report",
            ".omo/evidence/report.json",
        ]);
    command.output().expect("scope audit starts")
}
