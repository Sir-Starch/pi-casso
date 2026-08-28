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
