//! Scoring: how closely one window of pi digits resembles the target, and how
//! the best windows of a chunk are folded into the run's leaderboard.

use std::cmp::Ordering;

use anyhow::Result;

use crate::art::Bitmap;
use crate::search::types::{
    BestMatchDetails, EmergenceStatistics, MatchMode, TopMatch, WindowScore,
};

const EMERGENCE_COVERAGE_WEIGHT: f64 = 0.70;
const EMERGENCE_CONTRAST_WEIGHT: f64 = 0.20;
const EMERGENCE_CLEANLINESS_WEIGHT: f64 = 0.10;
const SCORE_QUANTIZATION_SCALE: f64 = 1_000_000.0;

#[derive(Clone, Debug)]
pub(crate) struct EmergencePlan {
    shape_pixels: usize,
    background_pixels: usize,
    coverage_by_count: Vec<f64>,
    leakage_by_count: Vec<f64>,
    placements: Vec<EmergencePlacement>,
}

#[derive(Clone, Debug)]
pub(crate) struct EmergencePlacement {
    x: usize,
    y: usize,
    shape_offsets: Vec<usize>,
    background_offsets: Vec<usize>,
}

pub(crate) fn emergence_score(coverage: f64, leakage: f64) -> f64 {
    let coverage = coverage.clamp(0.0, 1.0);
    let leakage = leakage.clamp(0.0, 1.0);
    if coverage == 1.0 && leakage == 0.0 {
        return 1.0;
    }
    let coverage_density = coverage * coverage;
    let contrast = if coverage > leakage {
        (coverage - leakage) / (1.0 - leakage).max(f64::EPSILON)
    } else {
        0.0
    };
    let cleanliness = 1.0 - leakage;
    EMERGENCE_COVERAGE_WEIGHT * coverage_density
        + EMERGENCE_CONTRAST_WEIGHT * contrast
        + EMERGENCE_CLEANLINESS_WEIGHT * cleanliness
}

pub(crate) fn quantize_score(score: f64) -> u32 {
    ((score.max(0.0) * SCORE_QUANTIZATION_SCALE + 0.5)
        .floor()
        .min(SCORE_QUANTIZATION_SCALE)) as u32
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn score_candidate_window(
    index: usize,
    digits: &[u8],
    target: &Bitmap,
    mode: MatchMode,
    canvas_width: usize,
    canvas_height: usize,
    threshold: u8,
    invert: bool,
    emergence_plan: Option<&EmergencePlan>,
) -> WindowScore {
    match mode {
        MatchMode::Emergence => {
            if let Some(plan) = emergence_plan {
                score_emergence_window_with_plan(index, digits, plan)
            } else {
                score_emergence_window(index, digits, target, canvas_width, canvas_height)
            }
        }
        MatchMode::Threshold | MatchMode::Exact => {
            let matching = score_threshold_window(digits, &target.pixels, threshold);
            let total_pixels = target.pixels.len() as u32;
            let inverted_matching = total_pixels - matching;
            let (matched, inverted) = if invert && inverted_matching > matching {
                (inverted_matching, true)
            } else {
                (matching, false)
            };
            WindowScore {
                index,
                score: matched as f64 / total_pixels as f64,
                score_q: quantize_score(matched as f64 / total_pixels as f64),
                inverted,
                digit: None,
                x: None,
                y: None,
                coverage: None,
                leakage: None,
                coverage_q: None,
                leakage_q: None,
                statistics: None,
            }
        }
    }
}

pub(crate) fn score_threshold_window(digits: &[u8], target: &[u8], threshold: u8) -> u32 {
    digits
        .iter()
        .zip(target.iter())
        .filter(|(digit, target_pixel)| u8::from(**digit >= threshold) == **target_pixel)
        .count() as u32
}

impl EmergencePlan {
    pub(crate) fn new(target: &Bitmap, canvas_width: usize, canvas_height: usize) -> Self {
        let shape_pixels = target.pixels.iter().filter(|pixel| **pixel == 1).count();
        let background_pixels = target.pixels.len().saturating_sub(shape_pixels);
        let coverage_by_count = if shape_pixels == 0 {
            vec![0.0]
        } else {
            (0..=shape_pixels)
                .map(|matched| matched as f64 / shape_pixels as f64)
                .collect()
        };
        let leakage_by_count = if background_pixels == 0 {
            vec![0.0]
        } else {
            (0..=background_pixels)
                .map(|leaked| leaked as f64 / background_pixels as f64)
                .collect()
        };
        let mut placements = Vec::new();
        for y_offset in 0..=canvas_height - target.height {
            for x_offset in 0..=canvas_width - target.width {
                let mut shape_offsets = Vec::with_capacity(shape_pixels);
                let mut background_offsets = Vec::with_capacity(background_pixels);
                for target_y in 0..target.height {
                    for target_x in 0..target.width {
                        let offset = (y_offset + target_y) * canvas_width + x_offset + target_x;
                        if target.get(target_x, target_y) == 1 {
                            shape_offsets.push(offset);
                        } else {
                            background_offsets.push(offset);
                        }
                    }
                }
                placements.push(EmergencePlacement {
                    x: x_offset,
                    y: y_offset,
                    shape_offsets,
                    background_offsets,
                });
            }
        }
        Self {
            shape_pixels,
            background_pixels,
            coverage_by_count,
            leakage_by_count,
            placements,
        }
    }
}

pub(crate) fn score_emergence_window_with_plan(
    index: usize,
    digits: &[u8],
    plan: &EmergencePlan,
) -> WindowScore {
    let mut best = WindowScore::empty(index);

    if plan.shape_pixels == 0 {
        return best;
    }

    for placement in &plan.placements {
        let mut shape_counts = [0usize; 10];
        let mut background_counts = [0usize; 10];
        for offset in &placement.shape_offsets {
            shape_counts[digits[*offset] as usize] += 1;
        }
        for offset in &placement.background_offsets {
            background_counts[digits[*offset] as usize] += 1;
        }
        for digit in 0..=9 {
            let matched_shape = shape_counts[digit];
            let leaked = background_counts[digit];
            let coverage = plan.coverage_by_count[matched_shape];
            let leakage = plan.leakage_by_count[leaked];
            let score = emergence_score(coverage, leakage);
            let is_better = score > best.score
                || (score == best.score && coverage > best.coverage.unwrap_or(0.0))
                || (score == best.score
                    && coverage == best.coverage.unwrap_or(0.0)
                    && leakage < best.leakage.unwrap_or(1.0));
            if is_better {
                best = WindowScore {
                    index,
                    score,
                    score_q: quantize_score(score),
                    inverted: false,
                    digit: Some(digit as u8),
                    x: Some(placement.x),
                    y: Some(placement.y),
                    coverage: Some(coverage),
                    leakage: Some(leakage),
                    coverage_q: Some(quantize_score(coverage)),
                    leakage_q: Some(quantize_score(leakage)),
                    statistics: Some(EmergenceStatistics {
                        covered: matched_shape,
                        total: plan.shape_pixels,
                        leaked,
                        background_total: plan.background_pixels,
                    }),
                };
            }
        }
    }

    best
}

pub(crate) fn score_emergence_window(
    index: usize,
    digits: &[u8],
    target: &Bitmap,
    canvas_width: usize,
    canvas_height: usize,
) -> WindowScore {
    let shape_pixels = target.pixels.iter().filter(|pixel| **pixel == 1).count();
    let background_pixels = target.pixels.len().saturating_sub(shape_pixels);
    let mut best = WindowScore::empty(index);

    if shape_pixels == 0 {
        return best;
    }

    for y_offset in 0..=canvas_height - target.height {
        for x_offset in 0..=canvas_width - target.width {
            for digit in 0..=9 {
                let mut matched_shape = 0usize;
                let mut leaked = 0usize;
                for target_y in 0..target.height {
                    for target_x in 0..target.width {
                        let target_pixel = target.get(target_x, target_y);
                        let canvas_index =
                            (y_offset + target_y) * canvas_width + x_offset + target_x;
                        if digits[canvas_index] == digit {
                            if target_pixel == 1 {
                                matched_shape += 1;
                            } else {
                                leaked += 1;
                            }
                        }
                    }
                }

                let coverage = matched_shape as f64 / shape_pixels as f64;
                let leakage = if background_pixels == 0 {
                    0.0
                } else {
                    leaked as f64 / background_pixels as f64
                };
                let score = emergence_score(coverage, leakage);
                let is_better = score > best.score
                    || (score == best.score && coverage > best.coverage.unwrap_or(0.0))
                    || (score == best.score
                        && coverage == best.coverage.unwrap_or(0.0)
                        && leakage < best.leakage.unwrap_or(1.0));
                if is_better {
                    best = WindowScore {
                        index,
                        score,
                        score_q: quantize_score(score),
                        inverted: false,
                        digit: Some(digit),
                        x: Some(x_offset),
                        y: Some(y_offset),
                        coverage: Some(coverage),
                        leakage: Some(leakage),
                        coverage_q: Some(quantize_score(coverage)),
                        leakage_q: Some(quantize_score(leakage)),
                        statistics: Some(EmergenceStatistics {
                            covered: matched_shape,
                            total: shape_pixels,
                            leaked,
                            background_total: background_pixels,
                        }),
                    };
                }
            }
        }
    }

    best
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn build_match_output(
    digits: &[u8],
    score: &WindowScore,
    target: &Bitmap,
    mode: MatchMode,
    canvas_width: usize,
    canvas_height: usize,
    threshold: u8,
) -> Result<(Bitmap, Option<BestMatchDetails>)> {
    match mode {
        MatchMode::Emergence => {
            let digit = score.digit.unwrap_or(0);
            let pixels = digits
                .iter()
                .map(|value| u8::from(*value == digit))
                .collect();
            let bitmap = Bitmap::new(canvas_width, canvas_height, pixels)?;
            let raw_canvas_digits = digits
                .iter()
                .map(|digit| char::from(b'0' + *digit))
                .collect();
            let details = BestMatchDetails {
                mode,
                digit: Some(digit),
                x: score.x.map(|value| value as u32),
                y: score.y.map(|value| value as u32),
                canvas_width: canvas_width as u32,
                canvas_height: canvas_height as u32,
                raw_canvas_digits: Some(raw_canvas_digits),
                coverage: score.coverage,
                leakage: score.leakage,
            };
            Ok((bitmap, Some(details)))
        }
        MatchMode::Threshold | MatchMode::Exact => {
            let bitmap = Bitmap::from_digit_window(digits, target.width, target.height, threshold)?;
            let details = BestMatchDetails {
                mode,
                digit: None,
                x: None,
                y: None,
                canvas_width: target.width as u32,
                canvas_height: target.height as u32,
                raw_canvas_digits: None,
                coverage: None,
                leakage: None,
            };
            Ok((bitmap, Some(details)))
        }
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn merge_chunk_top_matches(
    top_matches: &mut Vec<TopMatch>,
    scores: &mut [WindowScore],
    digits: &[u8],
    chunk_start_offset: u64,
    chunk_start_scanned: u64,
    width: usize,
    height: usize,
    canvas_width: usize,
    canvas_height: usize,
    match_mode: MatchMode,
    threshold: u8,
    top_n: usize,
) -> Result<()> {
    if top_n == 0 {
        top_matches.clear();
        return Ok(());
    }

    let selected = select_top_window_scores(scores, top_n);

    let window_len = if match_mode.is_emergence() {
        canvas_width * canvas_height
    } else {
        width * height
    };
    let target = Bitmap::blank(width, height);
    for score in selected {
        let offset = chunk_start_offset + score.index as u64;
        let (bitmap, details) = build_match_output(
            &digits[score.index..score.index + window_len],
            score,
            &target,
            match_mode,
            canvas_width,
            canvas_height,
            threshold,
        )?;
        merge_top_match(
            top_matches,
            TopMatch {
                offset,
                score: score.score,
                bitmap,
                inverted: score.inverted,
                scanned_windows: chunk_start_scanned + score.index as u64 + 1,
                details,
            },
            top_n,
        );
    }
    Ok(())
}

fn select_top_window_scores(scores: &mut [WindowScore], top_n: usize) -> &[WindowScore] {
    let selected_len = top_n.min(scores.len());
    if selected_len < scores.len() {
        scores.select_nth_unstable_by(selected_len, compare_window_scores);
    }
    scores[..selected_len].sort_unstable_by(compare_window_scores);
    &scores[..selected_len]
}

fn compare_window_scores(left: &WindowScore, right: &WindowScore) -> Ordering {
    right
        .score
        .partial_cmp(&left.score)
        .unwrap_or(Ordering::Equal)
        .then_with(|| left.index.cmp(&right.index))
}

pub(crate) fn merge_top_match(top_matches: &mut Vec<TopMatch>, candidate: TopMatch, top_n: usize) {
    if top_n == 0 {
        top_matches.clear();
        return;
    }
    if top_matches
        .iter()
        .any(|item| item.offset == candidate.offset)
    {
        return;
    }
    let insertion_index =
        top_matches.partition_point(|current| compare_top(current, &candidate) != Ordering::Less);
    if insertion_index < top_n {
        top_matches.insert(insertion_index, candidate);
        top_matches.truncate(top_n);
    }
}

pub(crate) fn compare_top(left: &TopMatch, right: &TopMatch) -> Ordering {
    left.score
        .partial_cmp(&right.score)
        .unwrap_or(Ordering::Equal)
        .then_with(|| right.offset.cmp(&left.offset))
}

#[cfg(test)]
mod tests {
    use super::*;
    use rayon::prelude::*;

    #[test]
    fn optimized_emergence_scoring_matches_reference() {
        let target = Bitmap::new(2, 2, vec![1, 0, 0, 1]).unwrap();
        let digits = vec![7, 1, 2, 1, 7, 3, 4, 5, 6];
        let plan = EmergencePlan::new(&target, 3, 3);
        let reference = score_emergence_window(0, &digits, &target, 3, 3);
        let optimized = score_emergence_window_with_plan(0, &digits, &plan);
        assert_eq!(optimized, reference);
    }

    #[test]
    fn a_perfect_emergence_scores_one() {
        assert_eq!(emergence_score(1.0, 0.0), 1.0);
    }

    #[test]
    fn leakage_always_costs_score() {
        // Same coverage, more bleed into the background: strictly worse.
        assert!(emergence_score(0.8, 0.1) > emergence_score(0.8, 0.4));
    }

    #[test]
    fn top_matches_stay_sorted_and_deduplicated() {
        let mut top = Vec::new();
        for (offset, score) in [(10, 0.4), (20, 0.9), (30, 0.6)] {
            merge_top_match(&mut top, top_match(offset, score), 3);
        }
        assert_eq!(
            top.iter().map(|item| item.offset).collect::<Vec<_>>(),
            vec![20, 30, 10]
        );
        // The same offset arriving twice must not occupy two slots.
        merge_top_match(&mut top, top_match(20, 0.9), 3);
        assert_eq!(top.len(), 3);
    }

    #[test]
    fn top_matches_respect_the_cap() {
        let mut top = Vec::new();
        for offset in 0..10u64 {
            merge_top_match(&mut top, top_match(offset, offset as f64 / 10.0), 2);
        }
        assert_eq!(top.len(), 2);
        assert_eq!(top[0].offset, 9);
    }

    #[test]
    fn a_zero_cap_keeps_no_matches() {
        let mut top = vec![top_match(1, 0.5)];
        merge_top_match(&mut top, top_match(2, 0.9), 0);
        assert!(top.is_empty());
    }

    #[test]
    fn transport_score_quantization_matches_the_half_unit_contract() {
        // Given: scores below, on, and above the diagnostic transport range.
        let fixtures = [
            (-1.0, 0),
            (0.0, 0),
            (0.000_000_49, 0),
            (0.000_000_50, 1),
            (0.123_456_49, 123_456),
            (0.123_456_50, 123_457),
            (1.0, 1_000_000),
            (f64::INFINITY, 1_000_000),
        ];

        // When: each score crosses the score_q transport boundary.
        let quantized = fixtures.map(|(score, _)| quantize_score(score));

        // Then: rounding is nearest with a half-unit bound and saturates safely.
        assert_eq!(
            quantized,
            fixtures.map(|(_, expected)| expected),
            "score_q must remain telemetry-only but deterministic"
        );
    }

    #[test]
    fn all_small_canvas_offsets_keep_canonical_scores_and_absolute_tie_order() {
        // Given: four overlapping 2x2 emergence windows with one perfect score
        // and three exact canonical ties.
        let target = Bitmap::new(2, 2, vec![1, 0, 0, 1]).unwrap();
        let digits = vec![1, 2, 3, 1, 4, 1, 5];
        let plan = EmergencePlan::new(&target, 2, 2);
        let mut scores = (0..4)
            .map(|index| {
                score_candidate_window(
                    index,
                    &digits[index..index + 4],
                    &target,
                    MatchMode::Emergence,
                    2,
                    2,
                    5,
                    false,
                    Some(&plan),
                )
            })
            .collect::<Vec<_>>();
        let canonical_nonperfect: f64 = 0.70 * 0.25 + 0.20 * 0.5 + 0.10;

        // When: the complete chunk is reduced from a nonzero absolute offset.
        let mut winners = Vec::new();
        merge_chunk_top_matches(
            &mut winners,
            &mut scores,
            &digits,
            41,
            0,
            2,
            2,
            2,
            2,
            MatchMode::Emergence,
            5,
            4,
        )
        .unwrap();

        // Then: every f64 is canonical and ties use absolute offset ascending.
        assert_eq!(scores[0].score.to_bits(), 1.0_f64.to_bits());
        assert!(
            scores[1..]
                .iter()
                .all(|score| score.score.to_bits() == canonical_nonperfect.to_bits())
        );
        assert_eq!(
            winners
                .iter()
                .map(|winner| (winner.offset, winner.score.to_bits()))
                .collect::<Vec<_>>(),
            vec![
                (41, 1.0_f64.to_bits()),
                (42, canonical_nonperfect.to_bits()),
                (43, canonical_nonperfect.to_bits()),
                (44, canonical_nonperfect.to_bits()),
            ]
        );
    }

    #[test]
    fn emergence_diagnostics_preserve_exact_statistics_and_half_unit_bounds() {
        // Given: one perfect emergence window and one partial-coverage window.
        let target = Bitmap::new(2, 2, vec![1, 0, 0, 1]).unwrap();
        let plan = EmergencePlan::new(&target, 2, 2);
        let fixtures = [vec![1, 2, 3, 1], vec![1, 2, 3, 4]];

        // When: canonical scoring emits exact and quantized diagnostics.
        let scores = fixtures
            .iter()
            .enumerate()
            .map(|(index, digits)| score_emergence_window_with_plan(index, digits, &plan))
            .collect::<Vec<_>>();

        // Then: integer sufficient statistics are exact and every diagnostic
        // remains within half a transport unit of its canonical f64.
        let perfect = scores[0].statistics.as_ref().unwrap();
        assert_eq!((perfect.covered, perfect.total), (2, 2));
        assert_eq!((perfect.leaked, perfect.background_total), (0, 2));
        let partial = scores[1].statistics.as_ref().unwrap();
        assert_eq!((partial.covered, partial.total), (1, 2));
        assert_eq!((partial.leaked, partial.background_total), (0, 2));
        for score in scores {
            let diagnostics = [
                (score.score, score.score_q),
                (score.coverage.unwrap(), score.coverage_q.unwrap()),
                (score.leakage.unwrap(), score.leakage_q.unwrap()),
            ];
            assert!(diagnostics.into_iter().all(|(canonical, quantized)| {
                (canonical - f64::from(quantized) / SCORE_QUANTIZATION_SCALE).abs()
                    <= 0.5 / SCORE_QUANTIZATION_SCALE
            }));
        }
    }

    #[test]
    fn bounded_score_selection_preserves_ties_and_quantized_scores() {
        // Given: more scored windows than the reducer is allowed to materialize.
        let mut scores = [
            window_score(4, 0.8, 800_000),
            window_score(3, 0.9, 900_000),
            window_score(1, 0.9, 900_000),
            window_score(2, 0.9, 900_000),
            window_score(0, 0.7, 700_000),
        ];

        // When: the reducer selects a bounded top three.
        let selected = select_top_window_scores(&mut scores, 3);

        // Then: only three scores are exposed, canonical f64 ordering wins,
        // source-index ties are deterministic, and score_q stays telemetry-only.
        assert_eq!(selected.len(), 3);
        assert_eq!(
            selected
                .iter()
                .map(|score| (score.index, score.score.to_bits(), score.score_q))
                .collect::<Vec<_>>(),
            vec![
                (1, 0.9_f64.to_bits(), 900_000),
                (2, 0.9_f64.to_bits(), 900_000),
                (3, 0.9_f64.to_bits(), 900_000),
            ]
        );
    }

    #[test]
    fn cpu_determinism_under_worker_sweep() {
        // Given: repeated worker counts plus the host's selected maximum.
        let target = Bitmap::new(2, 2, vec![1, 0, 0, 1]).unwrap();
        let digits = (0..68)
            .map(|index| (index * 7 % 10) as u8)
            .collect::<Vec<_>>();
        let plan = EmergencePlan::new(&target, 2, 2);
        let selected_maximum = std::thread::available_parallelism()
            .map(std::num::NonZero::get)
            .unwrap_or(1);
        let worker_counts = [1, 1, 2, 2, 4, 4, selected_maximum, selected_maximum];

        // When: each explicit, non-nested Rayon pool scores and reduces the same chunk.
        let results =
            worker_counts.map(|workers| deterministic_cpu_result(workers, &digits, &target, &plan));

        // Then: ordered absolute offsets, canonical f64 bits, and quantized
        // diagnostics are byte-for-byte equal across repeated configurations.
        assert!(results.windows(2).all(|pair| pair[0] == pair[1]));
    }

    fn deterministic_cpu_result(
        workers: usize,
        digits: &[u8],
        target: &Bitmap,
        plan: &EmergencePlan,
    ) -> Vec<u8> {
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(workers)
            .build()
            .unwrap();
        let actual_windows = digits.len() - 4 + 1;
        let mut scores = pool.install(|| {
            (0..actual_windows)
                .into_par_iter()
                .map(|index| {
                    score_candidate_window(
                        index,
                        &digits[index..index + 4],
                        target,
                        MatchMode::Emergence,
                        2,
                        2,
                        5,
                        false,
                        Some(plan),
                    )
                })
                .collect::<Vec<_>>()
        });
        let mut winners = Vec::new();
        merge_chunk_top_matches(
            &mut winners,
            &mut scores,
            digits,
            100,
            0,
            2,
            2,
            2,
            2,
            MatchMode::Emergence,
            5,
            5,
        )
        .unwrap();
        let mut bytes = Vec::new();
        for winner in winners {
            bytes.extend_from_slice(&winner.offset.to_le_bytes());
            bytes.extend_from_slice(&winner.score.to_bits().to_le_bytes());
        }
        for score in scores.iter().take(5) {
            bytes.extend_from_slice(&score.index.to_le_bytes());
            bytes.extend_from_slice(&score.score_q.to_le_bytes());
        }
        bytes
    }

    fn window_score(index: usize, score: f64, score_q: u32) -> WindowScore {
        WindowScore {
            index,
            score,
            score_q,
            ..WindowScore::empty(index)
        }
    }

    fn top_match(offset: u64, score: f64) -> TopMatch {
        TopMatch {
            offset,
            score,
            bitmap: Bitmap::blank(1, 1),
            inverted: false,
            scanned_windows: offset + 1,
            details: None,
        }
    }
}
