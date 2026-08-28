use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use assert_cmd::Command as AssertCommand;
use rusqlite::Connection;
use serde_json::Value;
use sha2::{Digest, Sha256};
use tempfile::TempDir;

struct TestRoot {
    data_home: PathBuf,
    config_home: PathBuf,
    tmp: PathBuf,
    _owned: Option<TempDir>,
}

#[cfg(windows)]
static TUI_RUNNER_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

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

fn shell_path(path: &Path) -> String {
    #[cfg(windows)]
    {
        let value = path.to_string_lossy().replace('\\', "/");
        if value.as_bytes().get(1) == Some(&b':') {
            format!("/{}{}", value[..1].to_ascii_lowercase(), &value[2..])
        } else {
            value
        }
    }
    #[cfg(not(windows))]
    {
        path.display().to_string()
    }
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

impl TestRoot {
    fn new(shared_harness_root: bool) -> Self {
        if shared_harness_root && std::env::var("PI_CASSO_TEST_MODE").as_deref() == Ok("1") {
            if let (Ok(data_home), Ok(config_home), Ok(tmp)) = (
                std::env::var("XDG_DATA_HOME"),
                std::env::var("XDG_CONFIG_HOME"),
                std::env::var("TMPDIR"),
            ) {
                return Self {
                    data_home: data_home.into(),
                    config_home: config_home.into(),
                    tmp: tmp.into(),
                    _owned: None,
                };
            }
        }
        let owned = TempDir::new().expect("temporary Task 13 root");
        let data_home = owned.path().join("data");
        let config_home = owned.path().join("config");
        let tmp = owned.path().join("tmp");
        fs::create_dir_all(&data_home).expect("data home");
        fs::create_dir_all(&config_home).expect("config home");
        fs::create_dir_all(&tmp).expect("temporary directory");
        Self {
            data_home,
            config_home,
            tmp,
            _owned: Some(owned),
        }
    }

    fn command(&self) -> AssertCommand {
        let mut command = AssertCommand::cargo_bin("pi-casso").expect("pi-casso binary");
        command
            .env("XDG_DATA_HOME", &self.data_home)
            .env("XDG_CONFIG_HOME", &self.config_home)
            .env("TMPDIR", &self.tmp)
            .env("PI_CASSO_TEST_MODE", "1")
            .env_remove("PI_CASSO_DATA_DIR")
            .env_remove("PI_CASSO_CONFIG");
        #[cfg(windows)]
        command.env("PI_CASSO_DATA_DIR", self.data_home.join("pi-casso"));
        command
    }

    fn database(&self) -> PathBuf {
        self.data_home.join("pi-casso/pi-casso.db")
    }
}

fn seed_snapshot(root: &TestRoot, name: &str) -> String {
    let fixture = root.data_home.join("pi-casso/task13-fixtures");
    fs::create_dir_all(&fixture).expect("fixture directory");
    let art = fixture.join(format!("{name}.art"));
    let digits = fixture.join(format!("{name}.digits"));
    fs::write(&art, "##\n##\n").expect("art fixture");
    fs::write(
        &digits,
        "314159265358979323846264338327950288419716939937510",
    )
    .expect("digit fixture");

    let output = root
        .command()
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
            "128",
            "--max-offset",
            "4096",
            "--keep-going-after-perfect",
            "--profile",
            "performance",
            "--backend",
            "cpu",
            "--gpu",
            "off",
            "--generator-backend",
            "cpu",
            "--cpu-workers",
            "2",
            "--cpu-utilization",
            "73",
            "--gpu-utilization",
            "0",
            "--chunk-size",
            "64",
            "--queue-depth",
            "3",
            "--memory-limit-mb",
            "128",
            "--ui-refresh-ms",
            "1000",
            "--checkpoint-every",
            "2",
            "--background-yield-ms",
            "7",
            "--pause-when-on-battery",
            "--max-fps",
            "60",
            "--show-metrics",
        ])
        .output()
        .expect("seed command runs");
    assert!(
        output.status.success(),
        "seed failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let run: Value = serde_json::from_slice(&output.stdout).expect("seed JSON");
    let run_id = run["id"].as_str().expect("seed run id").to_string();
    let checkpoint = root
        .command()
        .args(["--json", "resume", &run_id, "--no-tui", "--limit", "0"])
        .output()
        .expect("checkpointing resume runs");
    assert!(
        checkpoint.status.success(),
        "checkpointing resume failed: {}",
        String::from_utf8_lossy(&checkpoint.stderr)
    );
    run_id
}

fn snapshot(root: &TestRoot, run_id: &str) -> Value {
    let connection = Connection::open(root.database()).expect("database opens");
    let params: String = connection
        .query_row(
            "SELECT params_json FROM runs WHERE id = ?1",
            [run_id],
            |row| row.get(0),
        )
        .expect("persisted run");
    serde_json::from_str::<Value>(&params).expect("persisted params JSON")["performance_snapshot"]
        .clone()
}

fn assert_full_performance_snapshot(value: &Value) {
    let settings = &value["settings"];
    let limits = &settings["limits"];
    assert_eq!(settings["profile"], "performance");
    assert_eq!(settings["backend"], "cpu");
    assert_eq!(settings["generator_backend"], "cpu");
    assert_eq!(settings["gpu"], "off");
    assert_eq!(settings["thermal_mode"], "normal");
    assert_eq!(settings["show_metrics"], true);
    assert_eq!(limits["cpu_workers"], 2);
    assert_eq!(limits["cpu_utilization"], 73);
    assert_eq!(limits["gpu_utilization"], 0);
    assert_eq!(limits["chunk_size"], 64);
    assert_eq!(limits["queue_depth"], 3);
    assert_eq!(limits["memory_limit_mb"], 128);
    assert_eq!(limits["ui_refresh_ms"], 1000);
    assert_eq!(limits["checkpoint_every_secs"], 2);
    assert_eq!(limits["background_yield_ms"], 7);
    assert_eq!(limits["max_fps"], 60);
    assert_eq!(limits["pause_when_on_battery"], true);
    assert_eq!(value["work_windows"], 128);
    assert_eq!(value["limit"], 0);
    assert_eq!(value["max_offset"], 4096);
    assert_eq!(value["keep_going_after_perfect"], true);
}

fn run_tui(
    root: &TestRoot,
    run_id: &str,
    in_app: bool,
    worker_snapshot_sha256: Option<&str>,
) -> String {
    #[cfg(windows)]
    let _runner_lock = TUI_RUNNER_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);

    let transcript = root.tmp.join(format!(
        "task13-{}-{run_id}.transcript",
        if in_app { "in-app" } else { "cli" }
    ));
    let binary = shell_quote(&shell_path(Path::new(env!("CARGO_BIN_EXE_pi-casso"))));
    let app_invocation = if in_app {
        binary
    } else {
        format!("{binary} resume {} --tui", shell_quote(run_id))
    };
    #[cfg(windows)]
    let invocation = format!(
        "cmd.exe //c mode con: cols=120 lines=40 >/dev/null 2>&1 || true; stty rows 40 cols 120 2>/dev/null || true; timeout --signal=TERM --kill-after=2s 15s {app_invocation}"
    );
    #[cfg(not(windows))]
    let invocation = format!("stty rows 40 cols 120 2>/dev/null || true; exec {app_invocation}");
    #[cfg(windows)]
    let runner_command = format!(
        "set -o pipefail; winpty -Xallow-non-tty -Xplain bash -lc {} 2>&1 | tee {}",
        shell_quote(&invocation),
        shell_quote(&shell_path(&transcript))
    );
    #[cfg(windows)]
    let mut command = {
        let mut command = Command::new(git_bash());
        command.args(["-lc", &runner_command]);
        command
    };
    #[cfg(not(windows))]
    let mut command = {
        let mut command = Command::new("timeout");
        command.args([
            "--signal=TERM",
            "--kill-after=2s",
            "15s",
            "script",
            "--quiet",
            "--return",
            "--flush",
            "--command",
            &invocation,
            transcript.to_str().expect("UTF-8 transcript path"),
        ]);
        command
    };
    command
        .env("XDG_DATA_HOME", &root.data_home)
        .env("XDG_CONFIG_HOME", &root.config_home)
        .env("TMPDIR", &root.tmp)
        .env("PI_CASSO_TEST_MODE", "1")
        .env("TERM", "xterm-256color")
        .env_remove("PI_CASSO_DATA_DIR")
        .env_remove("PI_CASSO_CONFIG")
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    #[cfg(windows)]
    command.env("PI_CASSO_DATA_DIR", root.data_home.join("pi-casso"));
    let mut child = command.spawn().expect("PTY runner starts");
    let mut stdin = child.stdin.take().expect("PTY stdin");
    if in_app {
        assert!(
            wait_for_transcript_markers(&transcript, &["START SEARCH"]),
            "TUI did not render the initial Hunt screen within 10 seconds"
        );
        stdin.write_all(b"2r").expect("in-app resume input");
        stdin.flush().expect("flush in-app resume input");
    }
    let marker = format!("resume_run_id={run_id}");
    let worker_marker = worker_snapshot_sha256.map(worker_snapshot_marker);
    let required_markers = if let Some(worker_marker) = worker_marker.as_deref() {
        vec![
            marker.as_str(),
            "worker_handoff_received=true",
            worker_marker,
        ]
    } else {
        vec![marker.as_str()]
    };
    let marker_seen = wait_for_transcript_markers(&transcript, &required_markers);
    let quit_input = if marker_seen { b"q" } else { b"\x03" };
    if let Err(error) = stdin.write_all(quit_input).and_then(|_| stdin.flush()) {
        drop(stdin);
        let output = child.wait_with_output().expect("PTY runner completes");
        let transcript_contents = fs::read_to_string(&transcript).unwrap_or_default();
        panic!(
            "TUI quit input: {error}; runner status: {}; stderr: {}; transcript: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr),
            transcript_contents
        );
    }
    drop(stdin);
    let output = child.wait_with_output().expect("PTY runner completes");
    assert!(
        output.status.success(),
        "TUI failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let transcript_contents = fs::read_to_string(&transcript).unwrap_or_default();
    assert!(
        marker_seen,
        "TUI transcript did not emit {marker:?} within 10 seconds; transcript: {transcript_contents}"
    );
    transcript_contents
}

fn wait_for_transcript_markers(transcript: &Path, markers: &[&str]) -> bool {
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        if let Ok(contents) = fs::read_to_string(transcript) {
            if markers.iter().all(|marker| contents.contains(marker)) {
                return true;
            }
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    false
}

fn worker_snapshot_marker(expected_snapshot_sha256: &str) -> String {
    #[cfg(windows)]
    let expected_snapshot_sha256 = expected_snapshot_sha256
        .get(..16)
        .unwrap_or(expected_snapshot_sha256);
    format!("worker_snapshot_sha256={expected_snapshot_sha256}")
}

fn assert_handoff_markers(transcript: &str, run_id: &str) {
    for marker in [
        "resume_state_restored=true",
        "event_loop_handoff=true",
        &format!("resume_run_id={run_id}"),
        "max_fps=60",
        "ui_refresh_ms=1000",
        "backend=cpu",
        "queue=3",
        "wait=ready",
        "stop_reason=",
    ] {
        assert!(transcript.contains(marker), "missing TUI marker {marker:?}");
    }
}

fn snapshot_sha256(value: &Value) -> String {
    format!("{:x}", Sha256::digest(value.to_string().as_bytes()))
}

fn assert_worker_receipt_markers(transcript: &str, expected_snapshot_sha256: &str) {
    let worker_marker = worker_snapshot_marker(expected_snapshot_sha256);
    #[cfg(windows)]
    let resolved_marker = "worker_capability_re";
    #[cfg(not(windows))]
    let resolved_marker = "worker_capability_resolved=cpu";
    for marker in [
        "worker_handoff_received=true",
        worker_marker.as_str(),
        "worker_capability_sha256=",
        "worker_capability_status=ok",
        "worker_capability_requested=cpu",
        resolved_marker,
    ] {
        assert!(
            transcript.contains(marker),
            "missing worker receipt marker {marker:?}"
        );
    }
}

#[test]
fn resume_restores_full_performance_snapshot() {
    // Given: a persisted run checkpoint with every performance field distinct.
    let root = TestRoot::new(false);
    let run_id = seed_snapshot(&root, "task13-full-resume");

    // When: regular resume crosses the shared preparation boundary without overrides.
    let output = root
        .command()
        .args(["--json", "resume", &run_id, "--no-tui", "--limit", "0"])
        .output()
        .expect("resume command runs");

    // Then: the complete checkpointed performance snapshot remains restored.
    assert!(output.status.success());
    assert_full_performance_snapshot(&snapshot(&root, &run_id));
}

#[test]
fn tui_resume_snapshot_handoff_restores_max_fps_and_refresh() {
    // Given: a real checkpoint with UI values distinct from application defaults.
    let root = TestRoot::new(false);
    let run_id = seed_snapshot(&root, "task13-cli-tui-handoff");

    // When: CLI resume hands its prepared value to the TUI event loop.
    let transcript = run_tui(&root, &run_id, false, None);

    // Then: the visible dashboard carries snapshot timing and handoff provenance.
    assert_handoff_markers(&transcript, &run_id);
}

#[test]
fn tui_in_app_resume_snapshot_handoff() {
    // Given: the Runs tab can select a persisted checkpoint with distinct UI values.
    let root = TestRoot::new(false);
    let run_id = seed_snapshot(&root, "task13-in-app-handoff");
    let expected_snapshot_sha256 = snapshot_sha256(&snapshot(&root, &run_id));

    // When: the user opens Runs and resumes without leaving the active event loop.
    let transcript = run_tui(&root, &run_id, true, Some(&expected_snapshot_sha256));

    // Then: the worker receipt matches the selected prepared snapshot/capability,
    // and the existing real-PTY handoff markers remain visible.
    assert_handoff_markers(&transcript, &run_id);
    assert_worker_receipt_markers(&transcript, &expected_snapshot_sha256);
}

#[test]
fn tui_resume_restores_full_performance_snapshot() {
    // Given: a complete persisted snapshot prepared for CLI-to-TUI resume.
    let root = TestRoot::new(false);
    let run_id = seed_snapshot(&root, "task13-full-tui-resume");

    // When: the TUI consumes and runs the prepared resume.
    let transcript = run_tui(&root, &run_id, false, None);

    // Then: both visible handoff state and all persisted performance fields survive.
    assert_handoff_markers(&transcript, &run_id);
    assert_full_performance_snapshot(&snapshot(&root, &run_id));
}

#[test]
fn seed_snapshot_ui_values_60_1000() {
    // Given: the named-test harness supplies a root shared with later PTY QA.
    let root = TestRoot::new(true);

    // When: start creates and checkpoints a run through the real application path.
    let run_id = seed_snapshot(&root, "task13-seeded-ui-values");

    // Then: the checkpoint is queryable and the harness can extract its run id.
    let persisted = snapshot(&root, &run_id);
    assert_eq!(persisted["settings"]["limits"]["max_fps"], 60);
    assert_eq!(persisted["settings"]["limits"]["ui_refresh_ms"], 1000);
    println!("{}", serde_json::json!({ "run_id": run_id }));
}
