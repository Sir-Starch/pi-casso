mod task3_cache_support;

use std::fs;
#[cfg(unix)]
use std::process::Stdio;
#[cfg(unix)]
use std::thread;
#[cfg(unix)]
use std::time::{Duration, Instant};

#[cfg(unix)]
use serde_json::Value;
use tempfile::tempdir;

use task3_cache_support::{KNOWN_PI_PREFIX, pi_casso, publish_cache, status, write_art};

#[test]
fn pi_continuation_from_nonzero_prefix_matches_reference() {
    // Given: one independently generated reference and cache prefixes ending at
    // lengths 0, 1, 31, and a non-aligned large offset.
    let root = tempdir().unwrap();
    let reference_data = root.path().join("reference");
    let target_len = 4_161usize;
    pi_casso(&reference_data)
        .args([
            "pi",
            "generate",
            "--digits",
            &target_len.to_string(),
            "--generator-backend",
            "cpu",
        ])
        .assert()
        .success();
    let reference = fs::read(reference_data.join("pi-cache.txt")).unwrap();
    assert!(reference.starts_with(KNOWN_PI_PREFIX.as_bytes()));

    // When: each prefix is continued to the same absolute digit count.
    for length in [0usize, 1, 31, 4_097] {
        let data_dir = root.path().join(format!("prefix-{length}"));
        fs::create_dir_all(&data_dir).unwrap();
        publish_cache(&data_dir, &reference[..length]);
        pi_casso(&data_dir)
            .args([
                "pi",
                "generate",
                "--digits",
                &(target_len - length).to_string(),
                "--generator-backend",
                "cpu",
            ])
            .assert()
            .success();

        // Then: valid truncated ASCII is accepted as a prefix and continuation is exact.
        assert_eq!(fs::read(data_dir.join("pi-cache.txt")).unwrap(), reference);
    }
}

#[test]
fn finite_and_growing_termination_preserves_offsets() {
    // Given: a valid growing cache and a search shape that cannot terminate on perfection.
    let root = tempdir().unwrap();
    let art = write_art(root.path());
    let bounded_data = root.path().join("bounded-data");
    fs::create_dir_all(&bounded_data).unwrap();
    publish_cache(&bounded_data, KNOWN_PI_PREFIX.as_bytes());

    // When: one growing run uses an explicit work window and another receives SIGINT.
    pi_casso(&bounded_data)
        .args(["start", "--file"])
        .arg(&art)
        .args([
            "--width",
            "2",
            "--height",
            "2",
            "--name",
            "bounded",
            "--match-mode",
            "emergence",
            "--canvas-width",
            "2",
            "--canvas-height",
            "2",
            "--infinite",
            "--stress-test",
            "--limit",
            "3",
            "--no-tui",
            "--yes",
            "--keep-going-after-perfect",
            "--backend",
            "cpu",
            "--gpu",
            "off",
            "--cpu-workers",
            "1",
            "--chunk-size",
            "2",
        ])
        .assert()
        .success();

    #[cfg(unix)]
    {
        let cancelled_data = root.path().join("cancelled-data");
        fs::create_dir_all(&cancelled_data).unwrap();
        publish_cache(&cancelled_data, KNOWN_PI_PREFIX.as_bytes());
        let mut cancelled_command = std::process::Command::new(env!("CARGO_BIN_EXE_pi-casso"));
        cancelled_command
            .env("PI_CASSO_DATA_DIR", &cancelled_data)
            .args(["start", "--file"])
            .arg(&art)
            .args([
                "--width",
                "2",
                "--height",
                "2",
                "--name",
                "cancelled",
                "--match-mode",
                "emergence",
                "--canvas-width",
                "2",
                "--canvas-height",
                "2",
                "--infinite",
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
            ])
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        let mut child = cancelled_command.spawn().unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            assert!(
                child.try_wait().unwrap().is_none(),
                "growing search exited before cancellation"
            );
            let probe = pi_casso(&cancelled_data)
                .args(["--json", "status", "cancelled"])
                .output()
                .unwrap();
            if probe.status.success()
                && serde_json::from_slice::<Value>(&probe.stdout)
                    .is_ok_and(|report| report["status"] == "running")
            {
                break;
            }
            if Instant::now() >= deadline {
                let _ = std::process::Command::new("kill")
                    .args(["-INT", &child.id().to_string()])
                    .status();
                let _ = child.wait();
                panic!("growing search did not reach running state");
            }
            thread::sleep(Duration::from_millis(10));
        }
        let signal_status = std::process::Command::new("kill")
            .args(["-INT", &child.id().to_string()])
            .status()
            .unwrap();
        assert!(signal_status.success());
        assert!(child.wait().unwrap().success());
        assert_eq!(status(&cancelled_data, "cancelled")["status"], "paused");
    }

    // Then: work-window termination is exclusive and cancellation is resumable.
    let bounded = status(&bounded_data, "bounded");
    assert_eq!(bounded["current_offset"], 3);
    assert_eq!(bounded["scanned_windows"], 3);
    assert_eq!(bounded["status"], "paused");
}

#[test]
fn non_digit_cache_is_rejected_at_the_digit_source_boundary() {
    // Given: a cache with a non-digit inside its advertised prefix.
    let root = tempdir().unwrap();
    let data_dir = root.path().join("data");
    fs::create_dir_all(&data_dir).unwrap();
    let source = data_dir.join("non-digit.txt");
    fs::write(&source, "31415x9265").unwrap();
    let art = write_art(root.path());

    // When: the digit-source boundary opens the malformed cache file.
    let assertion = pi_casso(&data_dir)
        .args(["start", "--file"])
        .arg(&art)
        .args([
            "--width",
            "2",
            "--height",
            "2",
            "--name",
            "invalid-cache",
            "--match-mode",
            "emergence",
            "--canvas-width",
            "2",
            "--canvas-height",
            "2",
            "--pi-file",
        ])
        .arg(&source)
        .args(["--no-tui", "--backend", "cpu", "--gpu", "off"])
        .assert();

    // Then: invalid cache bytes are rejected before any scoring work.
    assertion.failure();
}

#[test]
fn pi_partial_write_recovery_preserves_prefix() {
    // Given: an independently generated 64-digit prefix truncated at byte 31.
    let root = tempdir().unwrap();
    let reference_data = root.path().join("reference-partial");
    pi_casso(&reference_data)
        .args([
            "pi",
            "generate",
            "--digits",
            "64",
            "--generator-backend",
            "cpu",
        ])
        .assert()
        .success();
    let reference = fs::read(reference_data.join("pi-cache.txt")).unwrap();
    let recovered_data = root.path().join("recovered-partial");
    fs::create_dir_all(&recovered_data).unwrap();
    publish_cache(&recovered_data, &reference[..31]);

    // When: generation resumes from the complete valid ASCII prefix.
    pi_casso(&recovered_data)
        .args([
            "pi",
            "generate",
            "--digits",
            "33",
            "--generator-backend",
            "cpu",
        ])
        .assert()
        .success();

    // Then: continuation preserves every existing byte and reconstructs the reference.
    assert_eq!(
        fs::read(recovered_data.join("pi-cache.txt")).unwrap(),
        reference
    );
}
