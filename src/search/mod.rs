//! Searching pi for visual echoes of a target shape.
//!
//! - [`types`] — the request/response vocabulary shared with the front-ends
//! - [`scoring`] — how well one window resembles the target
//! - [`backend`] — where that scoring runs (CPU pool or GPU)
//! - [`session`] — the mutable state of one invocation
//! - [`engine`] — the loop that drives all of the above

mod backend;
mod engine;
mod rate;
mod scoring;
mod session;
mod types;

pub use engine::{run_search, run_search_controlled};
pub use types::{
    BestMatchDetails, FinishReason, GenerationProgress, MatchMode, SearchCommand, SearchOptions,
    SearchReporter, SearchSnapshot, TopMatch,
};

#[cfg(test)]
mod tests {
    use std::io::Write;
    use std::time::Duration;

    use anyhow::Result;
    use tempfile::{NamedTempFile, tempdir};

    use super::*;
    use crate::art::Bitmap;
    use crate::digits::{DigitSourceSpec, FileDigitSource};
    use crate::performance::{
        GeneratorBackendChoice, GpuMode, PerformanceProfile, SearchBackendChoice, ThermalMode,
    };
    use crate::performance::{PerformanceOverrides, PerformanceSettings};
    use crate::storage::{BestEventRecord, RunStatus};
    use crate::storage::{NewRun, Storage};

    struct NullReporter;

    impl SearchReporter for NullReporter {
        fn on_update(&mut self, _snapshot: &SearchSnapshot) -> Result<()> {
            Ok(())
        }

        fn on_new_best(
            &mut self,
            _snapshot: &SearchSnapshot,
            _event: &BestEventRecord,
        ) -> Result<()> {
            Ok(())
        }

        fn on_finish(&mut self, _snapshot: &SearchSnapshot, _reason: FinishReason) -> Result<()> {
            Ok(())
        }
    }

    fn options(limit: Option<u64>) -> SearchOptions {
        SearchOptions {
            max_offset: None,
            limit,
            match_mode: MatchMode::Threshold,
            canvas_width: 2,
            canvas_height: 2,
            threshold: 5,
            invert: false,
            workers: None,
            checkpoint_every: Duration::from_secs(60),
            top_n: 10,
            keep_going_after_perfect: false,
            chunk_windows: 2,
            performance: PerformanceSettings::from_profile(
                PerformanceProfile::Custom,
                SearchBackendChoice::Cpu,
                GeneratorBackendChoice::Cpu,
                GpuMode::Off,
                None,
                ThermalMode::Normal,
                false,
                false,
                MatchMode::Threshold,
                PerformanceOverrides {
                    chunk_size: Some(2),
                    checkpoint_every_secs: Some(60),
                    ..PerformanceOverrides::default()
                },
            ),
        }
    }
    #[test]
    fn finds_exact_match_across_chunk_boundary() {
        let mut digits = NamedTempFile::new().unwrap();
        write!(digits, "110660123").unwrap();
        let source = FileDigitSource::new_with_options(digits.path().to_path_buf(), false);
        let dir = tempdir().unwrap();
        let mut storage = Storage::open_path(dir.path().join("state.db")).unwrap();
        let target = Bitmap::new(2, 2, vec![0, 1, 1, 0]).unwrap();
        let run = storage
            .create_run(NewRun {
                name: "boundary".to_string(),
                source: DigitSourceSpec::file(digits.path().to_path_buf(), false),
                template_name: None,
                art_hash: target.sha256(),
                width: 2,
                height: 2,
                canvas_width: 2,
                canvas_height: 2,
                match_mode: MatchMode::Threshold,
                threshold: 5,
                invert_enabled: false,
                start_offset: Some(0),
                target_bitmap: target,
                generated_digit_count: 0,
                params_json: "{}".to_string(),
            })
            .unwrap();

        let mut reporter = NullReporter;
        let run = run_search(&mut storage, run, &source, options(None), &mut reporter).unwrap();
        assert_eq!(run.status, RunStatus::PerfectFound);
        assert_eq!(run.best_offset, Some(2));
        assert_eq!(run.best_score, 1.0);
    }

    #[test]
    fn checkpoint_resume_continues_from_current_offset() {
        let mut digits = NamedTempFile::new().unwrap();
        write!(digits, "1110660123").unwrap();
        let source = FileDigitSource::new_with_options(digits.path().to_path_buf(), false);
        let dir = tempdir().unwrap();
        let mut storage = Storage::open_path(dir.path().join("state.db")).unwrap();
        let target = Bitmap::new(2, 2, vec![0, 1, 1, 0]).unwrap();
        let run = storage
            .create_run(NewRun {
                name: "resume".to_string(),
                source: DigitSourceSpec::file(digits.path().to_path_buf(), false),
                template_name: None,
                art_hash: target.sha256(),
                width: 2,
                height: 2,
                canvas_width: 2,
                canvas_height: 2,
                match_mode: MatchMode::Threshold,
                threshold: 5,
                invert_enabled: false,
                start_offset: Some(0),
                target_bitmap: target,
                generated_digit_count: 0,
                params_json: "{}".to_string(),
            })
            .unwrap();

        let mut reporter = NullReporter;
        let partial =
            run_search(&mut storage, run, &source, options(Some(2)), &mut reporter).unwrap();
        assert_eq!(partial.status, RunStatus::Paused);
        assert_eq!(partial.current_offset, 2);

        let loaded = storage.resolve_run("resume").unwrap();
        let resumed =
            run_search(&mut storage, loaded, &source, options(None), &mut reporter).unwrap();
        assert_eq!(resumed.status, RunStatus::PerfectFound);
        assert_eq!(resumed.best_offset, Some(3));
    }

    #[test]
    fn emergence_finds_shape_from_one_repeated_digit() {
        let mut digits = NamedTempFile::new().unwrap();
        write!(digits, "712173456").unwrap();
        let source = FileDigitSource::new_with_options(digits.path().to_path_buf(), false);
        let dir = tempdir().unwrap();
        let mut storage = Storage::open_path(dir.path().join("state.db")).unwrap();
        let target = Bitmap::new(2, 2, vec![1, 0, 0, 1]).unwrap();
        let run = storage
            .create_run(NewRun {
                name: "emergence".to_string(),
                source: DigitSourceSpec::file(digits.path().to_path_buf(), false),
                template_name: None,
                art_hash: target.sha256(),
                width: 2,
                height: 2,
                canvas_width: 3,
                canvas_height: 3,
                match_mode: MatchMode::Emergence,
                threshold: 5,
                invert_enabled: false,
                start_offset: Some(0),
                target_bitmap: target,
                generated_digit_count: 0,
                params_json: "{}".to_string(),
            })
            .unwrap();

        let mut reporter = NullReporter;
        let mut options = options(None);
        options.match_mode = MatchMode::Emergence;
        options.canvas_width = 3;
        options.canvas_height = 3;
        options.chunk_windows = 1;
        let run = run_search(&mut storage, run, &source, options, &mut reporter).unwrap();

        assert_eq!(run.status, RunStatus::PerfectFound);
        assert_eq!(run.best_offset, Some(0));
        let details = run.best_match.unwrap();
        assert_eq!(details.digit, Some(7));
        assert_eq!(details.x, Some(0));
        assert_eq!(details.y, Some(0));
        assert_eq!(details.raw_canvas_digits.as_deref(), Some("712173456"));
    }
}
