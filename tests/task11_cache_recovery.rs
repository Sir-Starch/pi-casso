pub mod task11_cache_support;

use std::fs::{self, OpenOptions};

use serde_json::json;

use task11_cache_support::{CacheFixture, assert_failed};

#[test]
fn pi_cache_rejects_non_digit_and_recovers_truncated_prefix() {
    // Given: a published cache and an import source containing a non-digit.
    let invalid = CacheFixture::new();
    assert!(invalid.generate(64).status.success());
    let before_raw = invalid.raw();
    let before_sidecar = fs::read(invalid.sidecar_path()).expect("valid sidecar");
    let source = invalid.path("invalid.txt");
    fs::write(&source, b"31415x9265").expect("invalid import source");

    // When: the import validates the complete source before publication.
    let rejected = invalid.import_file(&source);

    // Then: corruption is rejected without changing the recoverable publication.
    assert_failed(&rejected, "non-digit import");
    assert_eq!(invalid.raw(), before_raw);
    assert_eq!(
        fs::read(invalid.sidecar_path()).expect("unchanged sidecar"),
        before_sidecar
    );

    // Given: a valid published prefix is truncated at a digit boundary.
    let reference = CacheFixture::new();
    assert!(reference.generate(64).status.success());
    let expected = reference.raw();
    let truncated = CacheFixture::new();
    assert!(truncated.generate(64).status.success());
    let file = OpenOptions::new()
        .write(true)
        .open(truncated.cache_path())
        .expect("truncated cache");
    file.set_len(31).expect("truncate valid prefix");

    // When: repair rebuilds the snapshot and generation continues the prefix.
    assert!(truncated.repair().status.success());
    assert!(truncated.generate(33).status.success());

    // Then: continuation reconstructs the reference without duplicate digits.
    assert_eq!(truncated.raw(), expected);
    truncated.assert_published(&truncated.info(), &expected);
}

#[test]
fn pi_cache_rejects_malformed_sidecar_and_inconsistent_lengths() {
    // Given: one valid raw publication whose sidecar can be damaged in place.
    let fixture = CacheFixture::new();
    assert!(fixture.generate(64).status.success());
    let sidecar = fixture.sidecar_path();
    let raw = fixture.raw();

    // When: the sidecar is malformed, then advertises a longer raw snapshot.
    fs::write(&sidecar, b"not-json").expect("malformed sidecar");
    let malformed = fixture.info();
    assert_eq!(malformed["sidecar_status"], "invalid");
    assert_failed(
        &fixture.output(&[
            "pi",
            "generate",
            "--digits",
            "1",
            "--generator-backend",
            "cpu",
        ]),
        "malformed sidecar write",
    );
    assert!(fixture.repair().status.success());
    let mut inconsistent = fixture.read_sidecar();
    inconsistent["published_digits"] = json!(raw.len() + 1);
    inconsistent["raw_file_size"] = json!(raw.len() + 1);
    fs::write(
        &sidecar,
        serde_json::to_vec(&inconsistent).expect("inconsistent sidecar"),
    )
    .expect("inconsistent sidecar bytes");

    // Then: inconsistent lengths block writers until repair restores a valid snapshot.
    let report = fixture.info();
    assert_eq!(report["sidecar_status"], "inconsistent");
    assert_failed(
        &fixture.output(&[
            "pi",
            "generate",
            "--digits",
            "1",
            "--generator-backend",
            "cpu",
        ]),
        "inconsistent sidecar write",
    );
    assert!(fixture.repair().status.success());
    fixture.assert_published(&fixture.info(), &raw);
}

#[test]
fn pi_cache_refuses_live_or_unverifiable_stale_lock() {
    // Given: a crashed writer leaves an authentic lock record for this cache path.
    let fixture = CacheFixture::new();
    assert!(fixture.generate(32).status.success());
    let crashed = fixture
        .command()
        .env("PI_CASSO_TEST_CACHE_CRASH_PHASE", "after_raw_sync")
        .args([
            "pi",
            "generate",
            "--digits",
            "1",
            "--generator-backend",
            "cpu",
        ])
        .output()
        .expect("lock-producing generation starts");
    assert_failed(&crashed, "lock-producing generation");
    assert!(fixture.lock_path().exists());
    let raw_after_crash = fixture.raw();
    fixture.write_lock_with_pid(std::process::id());

    // When: forced repair sees a live owner, then an unverifiable lock record.
    let live = fixture.repair();
    assert_failed(&live, "live writer lock");
    assert_eq!(fixture.raw(), raw_after_crash);
    fs::write(fixture.lock_path(), b"unverifiable lock").expect("unverifiable lock");
    let unverifiable = fixture.repair();

    // Then: neither state is auto-deleted or bypassed by --force.
    assert_failed(&unverifiable, "unverifiable writer lock");
    assert_eq!(fixture.raw(), raw_after_crash);
    assert!(fixture.lock_path().exists());
}

#[test]
fn pi_cache_failure_recovery_contract() {
    // Given: a complete publication and a source that fails validation.
    let fixture = CacheFixture::new();
    assert!(fixture.generate(64).status.success());
    let expected = fixture.raw();
    let invalid = fixture.path("failure.txt");
    fs::write(&invalid, b"31415x9265").expect("invalid source");

    // When: validation fails, the sidecar is damaged, and a writer crashes before publication.
    assert_failed(&fixture.import_file(&invalid), "invalid replacement");
    assert_eq!(fixture.raw(), expected);
    fs::write(fixture.sidecar_path(), b"broken").expect("damaged sidecar");
    assert_eq!(fixture.info()["sidecar_status"], "invalid");
    assert!(fixture.repair().status.success());
    let crashed = fixture
        .command()
        .env("PI_CASSO_TEST_CACHE_CRASH_PHASE", "after_raw_sync")
        .args([
            "pi",
            "generate",
            "--digits",
            "1",
            "--generator-backend",
            "cpu",
        ])
        .output()
        .expect("failure-path generation starts");
    assert_failed(&crashed, "failure-path crash");
    assert_eq!(fixture.info()["sidecar_status"], "inconsistent");

    // Then: repair preserves the last committed prefix and one later append commits exactly once.
    assert!(fixture.repair().status.success());
    assert_eq!(fixture.raw(), expected);
    assert!(fixture.generate(1).status.success());
    let final_raw = fixture.raw();
    assert_eq!(&final_raw[..expected.len()], expected.as_slice());
    assert!(final_raw[expected.len()].is_ascii_digit());
    fixture.assert_published(&fixture.info(), &final_raw);
    assert!(!fixture.lock_path().exists());
}
