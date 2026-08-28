pub mod task11_cache_support;

use std::fs;
use std::thread;

use task11_cache_support::{CacheFixture, KNOWN_PI_PREFIX, assert_failed, distinct_digits};

#[test]
fn pi_cache_writer_lock_and_snapshot_publication() {
    // Given: two real writers extend one isolated cache while a reader observes it.
    let fixture = CacheFixture::new();
    let reference = CacheFixture::new();
    assert!(reference.generate(160).status.success());
    let expected = reference.raw();

    // When: concurrent generation and read-only cache-info requests share the path.
    thread::scope(|scope| {
        let first = scope.spawn(|| fixture.generate(64));
        let second = scope.spawn(|| fixture.generate(96));
        let reader = scope.spawn(|| {
            for _ in 0..8 {
                let info = fixture.info();
                assert!(matches!(
                    info["sidecar_status"].as_str(),
                    Some("missing" | "ok")
                ));
                assert!(info["published_digits"].as_u64().is_some_and(
                    |published| published <= info["raw_file_size"].as_u64().unwrap_or(0)
                ));
            }
        });
        assert!(first.join().expect("first writer joins").status.success());
        assert!(second.join().expect("second writer joins").status.success());
        reader.join().expect("reader joins");
    });

    // Then: the lock serializes extensions and readers observe a complete publication.
    assert_eq!(fixture.raw(), expected);
    fixture.assert_published(&fixture.info(), &expected);
    assert!(!fixture.lock_path().exists());
    assert!(fixture.previous_path_if_present().is_none());
}

#[test]
fn pi_cache_serializes_generation_import_and_repair() {
    // Given: a valid π prefix and a valid replacement source are ready.
    let fixture = CacheFixture::new();
    assert!(fixture.generate(64).status.success());
    let source = fixture.path("import.txt");
    fs::write(&source, KNOWN_PI_PREFIX).expect("replacement source");

    // When: generation, replacement import, and explicit repair race on one cache path.
    thread::scope(|scope| {
        let generator = scope.spawn(|| fixture.generate(32));
        let importer = scope.spawn(|| fixture.import_file(&source));
        let repair = scope.spawn(|| fixture.repair());
        for (name, output) in [
            ("generator", generator.join().expect("generator joins")),
            ("import", importer.join().expect("import joins")),
            ("repair", repair.join().expect("repair joins")),
        ] {
            assert!(
                output.status.success(),
                "{name} failed: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
    });

    // Then: only one complete raw/sidecar publication remains and no lock leaks.
    let raw = fixture.raw();
    fixture.assert_published(&fixture.info(), &raw);
    assert!(!fixture.lock_path().exists());
    assert!(fixture.previous_path_if_present().is_none());
}

#[test]
fn pi_cache_missing_sidecar_read_only_status() {
    // Given: a valid raw ASCII cache with no publication sidecar.
    let fixture = CacheFixture::new();
    fixture.write_raw(KNOWN_PI_PREFIX);
    assert!(fixture.sidecar_path_if_present().is_none());

    // When: read-only cache-info inspects the cache.
    let info = fixture.info();

    // Then: diagnostics validate the raw prefix but publish no digits or metadata.
    assert_eq!(info["sidecar_status"], "missing");
    assert_eq!(info["digits"], KNOWN_PI_PREFIX.len());
    assert_eq!(info["published_digits"], 0);
    assert_eq!(info["raw_file_size"], KNOWN_PI_PREFIX.len());
    assert_eq!(info["valid_ascii"], true);
    assert!(fixture.sidecar_path_if_present().is_none());
    assert!(!fixture.lock_path().exists());
}

#[test]
fn pi_cache_replacement_interruption_recovers_previous_publication() {
    // Given: an established publication and a different valid replacement source.
    let fixture = CacheFixture::new();
    assert!(fixture.generate(64).status.success());
    let previous_raw = fixture.raw();
    let previous_sidecar = fs::read(fixture.sidecar_path()).expect("previous sidecar");
    let source = fixture.path("replacement.txt");
    fs::write(&source, distinct_digits()).expect("replacement digits");

    // When: replacement is interrupted after the raw file is synchronized.
    let interrupted = fixture
        .command()
        .env("PI_CASSO_TEST_CACHE_CRASH_PHASE", "after_raw_sync")
        .args(["pi", "import"])
        .arg(&source)
        .output()
        .expect("interrupted import starts");
    assert_failed(&interrupted, "interrupted replacement");
    let interrupted_info = fixture.info();
    assert!(matches!(
        interrupted_info["sidecar_status"].as_str(),
        Some("inconsistent" | "invalid")
    ));

    // Then: repair restores the prior complete publication and cleans recovery state.
    let repaired = fixture.repair();
    assert!(
        repaired.status.success(),
        "repair failed: {}",
        String::from_utf8_lossy(&repaired.stderr)
    );
    assert_eq!(fixture.raw(), previous_raw);
    assert_eq!(
        fs::read(fixture.sidecar_path()).expect("repaired sidecar"),
        previous_sidecar
    );
    fixture.assert_published(&fixture.info(), &previous_raw);
    assert!(fixture.previous_path_if_present().is_none());
    assert!(!fixture.lock_path().exists());
}

#[test]
fn pi_cache_replacement_interruption_after_sidecar_rename_preserves_current_publication() {
    // Given: an established publication and a different valid replacement source.
    let fixture = CacheFixture::new();
    assert!(fixture.generate(64).status.success());
    let source = fixture.path("replacement.txt");
    let replacement = distinct_digits();
    fs::write(&source, replacement).expect("replacement digits");

    // When: replacement is interrupted after the new sidecar has been renamed.
    let interrupted = fixture
        .command()
        .env("PI_CASSO_TEST_CACHE_CRASH_PHASE", "after_sidecar_rename")
        .args(["pi", "import"])
        .arg(&source)
        .output()
        .expect("interrupted import starts");
    assert_failed(&interrupted, "interrupted replacement");
    fixture.assert_published(&fixture.info(), replacement);
    assert!(fixture.previous_path_if_present().is_some());

    // Then: repair preserves the complete current publication and cleans recovery state.
    assert!(fixture.repair().status.success());
    assert_eq!(fixture.raw(), replacement);
    fixture.assert_published(&fixture.info(), replacement);
    assert!(fixture.previous_path_if_present().is_none());
    assert!(!fixture.lock_path().exists());
}

#[test]
fn pi_cache_replacement_failpoint_is_ignored_outside_test_mode() {
    // Given: a valid replacement is ready for a normal production-mode invocation.
    let fixture = CacheFixture::new();
    assert!(fixture.generate(64).status.success());
    let source = fixture.path("replacement.txt");
    let replacement = distinct_digits();
    fs::write(&source, replacement).expect("replacement digits");

    // When: the test-only crash variable is set without enabling test mode.
    let published = fixture
        .command()
        .env_remove("PI_CASSO_TEST_MODE")
        .env("PI_CASSO_TEST_CACHE_CRASH_PHASE", "after_sidecar_rename")
        .args(["pi", "import"])
        .arg(&source)
        .output()
        .expect("replacement import starts");

    // Then: publication completes and leaves no crash-recovery artifacts.
    assert!(published.status.success());
    assert_eq!(fixture.raw(), replacement);
    fixture.assert_published(&fixture.info(), replacement);
    assert!(fixture.previous_path_if_present().is_none());
    assert!(!fixture.lock_path().exists());
}

#[test]
fn pi_cache_crash_after_sync_before_sidecar_reports_and_repairs_without_duplicate_digits() {
    // Given: an independently known 64-digit target and a 31-digit publication.
    let reference = CacheFixture::new();
    assert!(reference.generate(64).status.success());
    let expected = reference.raw();
    let fixture = CacheFixture::new();
    assert!(fixture.generate(31).status.success());

    // When: generation crashes after `sync_all` fulfills `sync_data` for the raw suffix.
    let crashed = fixture
        .command()
        .env("PI_CASSO_TEST_CACHE_CRASH_PHASE", "after_raw_sync")
        .args([
            "pi",
            "generate",
            "--digits",
            "33",
            "--generator-backend",
            "cpu",
            "--workers",
            "1",
        ])
        .output()
        .expect("crashed generation starts");
    assert_failed(&crashed, "crashed generation");
    let crash_info = fixture.info();
    assert_eq!(crash_info["sidecar_status"], "inconsistent");
    assert_eq!(crash_info["published_digits"], 31);
    assert_eq!(crash_info["raw_file_size"], 64);

    // Then: repair discards only the uncommitted suffix and continuation has no duplicate digits.
    assert!(fixture.repair().status.success());
    assert!(fixture.generate(33).status.success());
    assert_eq!(fixture.raw(), expected);
    fixture.assert_published(&fixture.info(), &expected);
}
