#[cfg(unix)]
#[test]
fn continuation_rejects_symlinked_source_without_changing_publication() {
    use std::os::unix::fs::symlink;

    use tempfile::tempdir;

    // Given: a published prefix and a continuation path that aliases another file.
    let root = tempdir().expect("cache publication root");
    let raw = root.path().join("pi-cache.txt");
    super::append_digits(&raw, &[3, 1, 4]).expect("published prefix");
    let before = std::fs::read(&raw).expect("raw prefix");
    let source = root.path().join("continuation.txt");
    let linked = root.path().join("linked-continuation.txt");
    std::fs::write(&source, b"314159").expect("continuation source");
    symlink(&source, &linked).expect("continuation symlink");

    // When: continuation validation opens the source at the publication boundary.
    let error = super::append_from_validated_source(&raw, &linked)
        .expect_err("symlinked continuation must fail");

    // Then: no target is followed and the prior publication remains intact.
    assert!(error.to_string().contains("symlink"));
    assert_eq!(std::fs::read(raw).expect("unchanged raw"), before);
}

#[test]
fn range_reads_do_not_rescan_the_published_cache() {
    use tempfile::tempdir;

    let root = tempdir().expect("cache publication root");
    let raw = root.path().join("pi-cache.txt");
    super::append_digits(&raw, &vec![3; 4096]).expect("published cache");
    super::snapshot::reset_raw_snapshot_reads();

    for offset in [0, 512, 1024, 1536] {
        let read = super::read_range_timed(&raw, offset, 256).expect("range read");
        assert_eq!(read.digits.len(), 256);
    }

    assert_eq!(super::snapshot::raw_snapshot_reads(), 0);
}

#[test]
fn published_digit_count_does_not_rescan_the_published_cache() {
    use tempfile::tempdir;

    let root = tempdir().expect("cache publication root");
    let raw = root.path().join("pi-cache.txt");
    super::append_digits(&raw, &vec![3; 4096]).expect("published cache");
    super::snapshot::reset_raw_snapshot_reads();

    assert_eq!(
        super::published_digit_count(&raw).expect("digit count"),
        4096
    );
    assert_eq!(super::snapshot::raw_snapshot_reads(), 0);
}

#[test]
fn repair_rewrites_sidecar_for_fast_readers() {
    use std::fs::OpenOptions;
    use std::io::Write;

    use tempfile::tempdir;

    let root = tempdir().expect("cache publication root");
    let raw = root.path().join("pi-cache.txt");
    super::append_digits(&raw, &[3; 32]).expect("published cache");

    let mut file = OpenOptions::new()
        .append(true)
        .open(&raw)
        .expect("raw cache");
    file.write_all(b"14").expect("unpublished suffix");
    file.sync_all().expect("sync unpublished suffix");
    assert_eq!(
        super::fast_info(&raw)
            .expect("inconsistent snapshot")
            .sidecar_status,
        "inconsistent"
    );

    super::repair_publication(&raw).expect("repair publication");

    assert_eq!(
        super::fast_info(&raw)
            .expect("repaired snapshot")
            .sidecar_status,
        "ok"
    );
    assert_eq!(
        super::published_digit_count(&raw).expect("published digit count"),
        32
    );
    assert_eq!(
        super::read_range_timed(&raw, 0, 32)
            .expect("repaired range read")
            .digits,
        vec![3; 32]
    );
}

#[test]
fn repair_rejects_an_unpublished_same_size_mutation() {
    use std::fs;

    use tempfile::tempdir;

    let root = tempdir().expect("cache publication root");
    let raw = root.path().join("pi-cache.txt");
    super::append_digits(&raw, &[3; 32]).expect("published cache");
    fs::write(&raw, [b'9'; 32]).expect("same-size mutation");

    let error = super::repair_publication(&raw).expect_err("mutation must not be re-signed");

    assert!(
        error
            .to_string()
            .contains("do not match the published sidecar")
    );
    assert_eq!(fs::read(raw).expect("raw cache"), vec![b'9'; 32]);
}
