//! Scoring: how closely one window of pi digits resembles the target, and how
//! the best windows of a chunk are folded into the run's leaderboard.

use std::cmp::Ordering;

use anyhow::Result;

use crate::art::Bitmap;
use crate::search::types::{BestMatchDetails, MatchMode, TopMatch, WindowScore};

const EMERGENCE_COVERAGE_WEIGHT: f64 = 0.70;
const EMERGENCE_CONTRAST_WEIGHT: f64 = 0.20;
const EMERGENCE_CLEANLINESS_WEIGHT: f64 = 0.10;

#[derive(Clone, Debug)]
pub(crate) struct EmergencePlan {
    shape_pixels: usize,
    background_pixels: usize,
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
                inverted,
                digit: None,
                x: None,
                y: None,
                coverage: None,
                leakage: None,
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
            let coverage = matched_shape as f64 / plan.shape_pixels as f64;
            let leakage = if plan.background_pixels == 0 {
                0.0
            } else {
                leaked as f64 / plan.background_pixels as f64
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
                    inverted: false,
                    digit: Some(digit as u8),
                    x: Some(placement.x),
                    y: Some(placement.y),
                    coverage: Some(coverage),
                    leakage: Some(leakage),
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
                        inverted: false,
                        digit: Some(digit),
                        x: Some(x_offset),
                        y: Some(y_offset),
                        coverage: Some(coverage),
                        leakage: Some(leakage),
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

    scores.sort_unstable_by(|left, right| {
        right
            .score
            .partial_cmp(&left.score)
            .unwrap_or(Ordering::Equal)
            .then_with(|| left.index.cmp(&right.index))
    });

    let window_len = if match_mode.is_emergence() {
        canvas_width * canvas_height
    } else {
        width * height
    };
    let target = Bitmap::blank(width, height);
    for score in scores.iter().take(top_n) {
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
    top_matches.push(candidate);
    top_matches.sort_by(|left, right| compare_top(right, left));
    top_matches.truncate(top_n);
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

    #[test]
    fn optimized_emergence_scoring_matches_reference() {
        let target = Bitmap::new(2, 2, vec![1, 0, 0, 1]).unwrap();
        let digits = vec![7, 1, 2, 1, 7, 3, 4, 5, 6];
        let plan = EmergencePlan::new(&target, 3, 3);
        let reference = score_emergence_window(0, &digits, &target, 3, 3);
        let optimized = score_emergence_window_with_plan(0, &digits, &plan);
        assert_eq!(optimized.digit, reference.digit);
        assert_eq!(optimized.x, reference.x);
        assert_eq!(optimized.y, reference.y);
        assert!((optimized.score - reference.score).abs() < f64::EPSILON);
        assert_eq!(optimized.coverage, reference.coverage);
        assert_eq!(optimized.leakage, reference.leakage);
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
