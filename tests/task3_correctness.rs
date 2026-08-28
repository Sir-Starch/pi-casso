mod task3_support;

use tempfile::tempdir;

use task3_support::{diagnostic_score_q, pi_casso, start_finite, status, write_art, write_digits};

#[test]
fn finite_ranges_use_exclusive_offsets_and_report_short_source_counts() {
    // Given: finite sources provisioned for three windows, an empty range, and
    // a deliberately short source containing only two complete windows.
    let root = tempdir().unwrap();
    let art = write_art(root.path());
    let complete = write_digits(root.path(), "complete.txt", "000001231415");
    let empty = write_digits(root.path(), "empty.txt", "000001231");
    let short = write_digits(root.path(), "short.txt", "0000012314");

    // When: limit, max-offset, empty, and short-source searches run independently.
    start_finite(
        &root.path().join("limit-data"),
        &art,
        &complete,
        "limit",
        &["--start-offset", "5", "--limit", "3"],
    );
    start_finite(
        &root.path().join("max-data"),
        &art,
        &complete,
        "max",
        &["--start-offset", "5", "--max-offset", "8"],
    );
    start_finite(
        &root.path().join("empty-data"),
        &art,
        &empty,
        "empty",
        &["--start-offset", "5", "--max-offset", "5"],
    );
    start_finite(
        &root.path().join("short-data"),
        &art,
        &short,
        "short",
        &["--start-offset", "5", "--limit", "3"],
    );

    // Then: both finite bounds are exclusive, empty work consumes zero, and
    // source exhaustion preserves the exact completed-window count.
    for (data, name) in [("limit-data", "limit"), ("max-data", "max")] {
        let report = status(&root.path().join(data), name);
        assert_eq!(report["current_offset"], 8);
        assert_eq!(report["scanned_windows"], 3);
        assert_eq!(report["status"], "paused");
    }
    let empty_report = status(&root.path().join("empty-data"), "empty");
    assert_eq!(empty_report["current_offset"], 5);
    assert_eq!(empty_report["scanned_windows"], 0);
    assert_eq!(empty_report["status"], "paused");
    let short_report = status(&root.path().join("short-data"), "short");
    assert_eq!(short_report["current_offset"], 7);
    assert_eq!(short_report["scanned_windows"], 2);
    assert_eq!(short_report["status"], "source_exhausted");
}

#[test]
fn checkpoint_resume_preserves_absolute_ties_and_score_telemetry() {
    // Given: four windows beginning at absolute offset 41, split over two invocations.
    let root = tempdir().unwrap();
    let data_dir = root.path().join("data");
    let art = write_art(root.path());
    let digits = write_digits(root.path(), "resume.txt", &("0".repeat(41) + "1231415"));
    start_finite(
        &data_dir,
        &art,
        &digits,
        "resume",
        &["--start-offset", "41", "--limit", "2"],
    );

    // When: the persisted checkpoint is resumed for two more exclusive windows.
    pi_casso(&data_dir)
        .args([
            "resume",
            "resume",
            "--no-tui",
            "--keep-going-after-perfect",
            "--limit",
            "2",
            "--backend",
            "cpu",
            "--gpu",
            "off",
            "--cpu-workers",
            "2",
            "--chunk-size",
            "2",
            "--top",
            "8",
        ])
        .assert()
        .success();
    let report = status(&data_dir, "resume");

    // Then: offsets remain absolute, exact f64 ties remain offset-ascending,
    // and score_q is derived only as deterministic diagnostics.
    assert_eq!(report["current_offset"], 45);
    assert_eq!(report["scanned_windows"], 4);
    let winners = report["top_matches"].as_array().unwrap();
    let observed = winners
        .iter()
        .map(|winner| {
            let score = winner["score"].as_f64().unwrap();
            (
                winner["offset"].as_u64().unwrap(),
                score.to_bits(),
                diagnostic_score_q(score),
            )
        })
        .collect::<Vec<_>>();
    let tied_score: f64 = 0.70 * 0.25 + 0.20 * 0.5 + 0.10;
    assert_eq!(
        observed,
        vec![
            (41, 1.0_f64.to_bits(), 1_000_000),
            (42, tied_score.to_bits(), 375_000),
            (43, tied_score.to_bits(), 375_000),
            (44, tied_score.to_bits(), 375_000),
        ]
    );
}
