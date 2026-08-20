//! The command palette: fuzzy-find any available action and run it.
//!
//! This is what makes the app discoverable without memorising single letters.
//! It draws exclusively from the action registry, so it can never offer a
//! command that does not exist or miss one that does.

use crate::tui::actions::{Action, ActionContext, Command, available};
use crate::tui::input::TextInput;

pub struct Palette {
    pub query: TextInput,
    pub selected: usize,
    matches: Vec<Match>,
}

#[derive(Clone, Copy)]
pub struct Match {
    pub command: &'static Command,
    pub score: i32,
}

impl Palette {
    pub fn new(context: ActionContext) -> Self {
        let mut palette = Self {
            query: TextInput::default(),
            selected: 0,
            matches: Vec::new(),
        };
        palette.refresh(context);
        palette
    }

    pub fn refresh(&mut self, context: ActionContext) {
        let query = self.query.trimmed().to_lowercase();
        let mut matches: Vec<Match> = available(context)
            .filter_map(|command| score(&query, command).map(|score| Match { command, score }))
            .collect();
        // Higher score first; ties keep registry order, which groups related
        // commands together.
        matches.sort_by_key(|entry| std::cmp::Reverse(entry.score));
        self.matches = matches;
        self.selected = self.selected.min(self.matches.len().saturating_sub(1));
    }

    pub fn matches(&self) -> &[Match] {
        &self.matches
    }

    pub fn move_selection(&mut self, delta: i32) {
        if self.matches.is_empty() {
            self.selected = 0;
            return;
        }
        let len = self.matches.len() as i32;
        let next = (self.selected as i32 + delta).rem_euclid(len);
        self.selected = next as usize;
    }

    pub fn chosen(&self) -> Option<Action> {
        self.matches
            .get(self.selected)
            .map(|entry| entry.command.action)
    }
}

/// Subsequence matching with a bonus for tight, early, word-start matches —
/// enough to make "psf" find "Profile: performance" without pulling in a fuzzy
/// matching dependency.
fn score(query: &str, command: &Command) -> Option<i32> {
    if query.is_empty() {
        return Some(0);
    }
    let title = command.title.to_lowercase();
    let title_score = subsequence_score(query, &title);
    if let Some(score) = title_score {
        return Some(score + 100);
    }
    // Falling back to the hint lets "quiet" find the eco profile.
    subsequence_score(query, &command.hint.to_lowercase())
}

fn subsequence_score(query: &str, haystack: &str) -> Option<i32> {
    let haystack: Vec<char> = haystack.chars().collect();
    let mut score = 0;
    let mut position = 0usize;
    let mut previous_match: Option<usize> = None;

    for needle in query.chars() {
        if needle.is_whitespace() {
            continue;
        }
        let found = haystack[position..]
            .iter()
            .position(|candidate| *candidate == needle)?
            + position;
        // Consecutive characters are much stronger evidence than scattered ones.
        if previous_match == Some(found.wrapping_sub(1)) {
            score += 8;
        }
        // So is landing on the start of a word.
        if found == 0 || !haystack[found - 1].is_alphanumeric() {
            score += 5;
        }
        // Early matches beat late ones, mildly.
        score += (20i32 - found as i32).max(0) / 4;
        previous_match = Some(found);
        position = found + 1;
    }
    Some(score)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::performance::PerformanceProfile;
    use crate::tui::tabs::Tab;

    fn context(tab: Tab, search_active: bool) -> ActionContext {
        ActionContext { tab, search_active }
    }

    #[test]
    fn an_empty_query_offers_everything_in_scope() {
        let palette = Palette::new(context(Tab::Runs, false));
        assert_eq!(
            palette.matches().len(),
            available(context(Tab::Runs, false)).count()
        );
    }

    #[test]
    fn typing_narrows_to_the_intended_command() {
        let mut palette = Palette::new(context(Tab::Hunt, true));
        palette.query.set("pause");
        palette.refresh(context(Tab::Hunt, true));
        assert_eq!(palette.chosen(), Some(Action::TogglePause));
    }

    #[test]
    fn initials_find_a_multi_word_command() {
        let mut palette = Palette::new(context(Tab::Hunt, true));
        palette.query.set("pp");
        palette.refresh(context(Tab::Hunt, true));
        assert_eq!(
            palette.chosen(),
            Some(Action::SetProfile(PerformanceProfile::Performance))
        );
    }

    #[test]
    fn out_of_scope_commands_are_never_offered() {
        let mut palette = Palette::new(context(Tab::Gallery, false));
        palette.query.set("pause");
        palette.refresh(context(Tab::Gallery, false));
        assert!(palette.chosen().is_none());
    }

    #[test]
    fn a_hopeless_query_matches_nothing() {
        let mut palette = Palette::new(context(Tab::Runs, false));
        palette.query.set("zzzzzz");
        palette.refresh(context(Tab::Runs, false));
        assert!(palette.matches().is_empty());
        assert!(palette.chosen().is_none());
    }

    #[test]
    fn selection_wraps_in_both_directions() {
        let mut palette = Palette::new(context(Tab::Gallery, false));
        let len = palette.matches().len();
        assert!(len > 1);
        palette.move_selection(-1);
        assert_eq!(palette.selected, len - 1);
        palette.move_selection(1);
        assert_eq!(palette.selected, 0);
    }

    #[test]
    fn consecutive_matches_outrank_scattered_ones() {
        let tight = subsequence_score("run", "runs").unwrap();
        let loose = subsequence_score("run", "resume current run").unwrap();
        assert!(tight > loose);
    }
}
