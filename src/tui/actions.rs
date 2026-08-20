//! Every user-triggerable action, in one registry.
//!
//! The registry is the single source for three things that used to be written
//! out separately and drift apart: the key bindings, the command palette, and
//! the help overlay. Adding an action here makes it reachable and documented in
//! all three at once.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::performance::PerformanceProfile;
use crate::tui::tabs::Tab;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    // Global
    Goto(Tab),
    NextTab,
    PrevTab,
    OpenPalette,
    ToggleHelp,
    Quit,

    // Hunt: wizard
    StartSearch,
    ResetWizard,

    // Hunt: live search
    TogglePause,
    SaveCheckpoint,
    SetProfile(PerformanceProfile),
    CycleProfile,
    AdjustWorkers(i32),
    AdjustChunk(i32),
    CycleThermal,
    ToggleGpu,
    ToggleMetrics,
    StopSearch,
    DismissFinished,
    ExportActiveRun,

    // Runs
    ResumeSelectedRun,
    DeleteSelectedRun,
    ExportSelectedRun,
    RefreshRuns,

    // Gallery
    UseSelectedTemplate,

    // Data
    ValidateDigitFile,
    ImportDigitFile,

    // Settings
    CycleTheme,
    ToggleUnicode,
    SaveSettings,
}

/// Where an action makes sense. Palette and help filter on this so the user is
/// never offered "pause search" while browsing templates.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scope {
    Global,
    Tab(Tab),
    /// Only while a search worker exists on the Hunt tab.
    LiveSearch,
    /// Only while the Hunt tab is showing the wizard.
    Wizard,
}

pub struct Command {
    pub action: Action,
    pub title: &'static str,
    pub hint: &'static str,
    /// Displayed binding, e.g. "space" or "1..4". Empty means palette-only.
    pub keys: &'static str,
    pub scope: Scope,
}

/// The whole vocabulary of the app.
pub const COMMANDS: &[Command] = &[
    Command {
        action: Action::Goto(Tab::Hunt),
        title: "Go to Hunt",
        hint: "live search or the new-search wizard",
        keys: "1",
        scope: Scope::Global,
    },
    Command {
        action: Action::Goto(Tab::Runs),
        title: "Go to Runs",
        hint: "saved runs and their best matches",
        keys: "2",
        scope: Scope::Global,
    },
    Command {
        action: Action::Goto(Tab::Gallery),
        title: "Go to Gallery",
        hint: "built-in templates",
        keys: "3",
        scope: Scope::Global,
    },
    Command {
        action: Action::Goto(Tab::Data),
        title: "Go to Data",
        hint: "pi cache and digit import",
        keys: "4",
        scope: Scope::Global,
    },
    Command {
        action: Action::Goto(Tab::Settings),
        title: "Go to Settings",
        hint: "theme and defaults",
        keys: "5",
        scope: Scope::Global,
    },
    Command {
        action: Action::NextTab,
        title: "Next tab",
        hint: "",
        keys: "tab",
        scope: Scope::Global,
    },
    Command {
        action: Action::PrevTab,
        title: "Previous tab",
        hint: "",
        keys: "shift+tab",
        scope: Scope::Global,
    },
    Command {
        action: Action::OpenPalette,
        title: "Command palette",
        hint: "fuzzy-find any action",
        keys: "ctrl+p",
        scope: Scope::Global,
    },
    Command {
        action: Action::ToggleHelp,
        title: "Help",
        hint: "keys available right here",
        keys: "? / F1",
        scope: Scope::Global,
    },
    Command {
        action: Action::Quit,
        title: "Quit",
        hint: "stops and checkpoints any live search first",
        keys: "ctrl+q",
        scope: Scope::Global,
    },
    Command {
        action: Action::StartSearch,
        title: "Start search",
        hint: "begin hunting with the current settings",
        keys: "F9 / enter",
        scope: Scope::Wizard,
    },
    Command {
        action: Action::ResetWizard,
        title: "Reset wizard",
        hint: "back to defaults",
        keys: "",
        scope: Scope::Wizard,
    },
    Command {
        action: Action::TogglePause,
        title: "Pause / resume",
        hint: "checkpoints on pause",
        keys: "space",
        scope: Scope::LiveSearch,
    },
    Command {
        action: Action::CycleProfile,
        title: "Next performance profile",
        hint: "eco to balanced to performance to max, and around",
        keys: "p",
        scope: Scope::LiveSearch,
    },
    Command {
        action: Action::SaveCheckpoint,
        title: "Save checkpoint",
        hint: "write progress now",
        keys: "s",
        scope: Scope::LiveSearch,
    },
    Command {
        action: Action::SetProfile(PerformanceProfile::Eco),
        title: "Profile: eco",
        hint: "quiet and slow",
        keys: "F2",
        scope: Scope::LiveSearch,
    },
    Command {
        action: Action::SetProfile(PerformanceProfile::Balanced),
        title: "Profile: balanced",
        hint: "the default",
        keys: "F3",
        scope: Scope::LiveSearch,
    },
    Command {
        action: Action::SetProfile(PerformanceProfile::Performance),
        title: "Profile: performance",
        hint: "most of the machine",
        keys: "F4",
        scope: Scope::LiveSearch,
    },
    Command {
        action: Action::SetProfile(PerformanceProfile::Max),
        title: "Profile: max",
        hint: "all of the machine",
        keys: "F5",
        scope: Scope::LiveSearch,
    },
    Command {
        action: Action::AdjustWorkers(1),
        title: "More CPU workers",
        hint: "",
        keys: "+",
        scope: Scope::LiveSearch,
    },
    Command {
        action: Action::AdjustWorkers(-1),
        title: "Fewer CPU workers",
        hint: "",
        keys: "-",
        scope: Scope::LiveSearch,
    },
    Command {
        action: Action::AdjustChunk(1),
        title: "Larger chunk",
        hint: "more windows per batch",
        keys: "]",
        scope: Scope::LiveSearch,
    },
    Command {
        action: Action::AdjustChunk(-1),
        title: "Smaller chunk",
        hint: "fewer windows per batch",
        keys: "[",
        scope: Scope::LiveSearch,
    },
    Command {
        action: Action::CycleThermal,
        title: "Cycle thermal mode",
        hint: "quiet / normal / aggressive",
        keys: "t",
        scope: Scope::LiveSearch,
    },
    Command {
        action: Action::ToggleGpu,
        title: "Cycle GPU mode",
        hint: "off / auto / on",
        keys: "g",
        scope: Scope::LiveSearch,
    },
    Command {
        action: Action::ToggleMetrics,
        title: "Toggle metrics",
        hint: "extra engine detail",
        keys: "m",
        scope: Scope::LiveSearch,
    },
    Command {
        action: Action::ExportActiveRun,
        title: "Export run to JSON",
        hint: "",
        keys: "e",
        scope: Scope::LiveSearch,
    },
    Command {
        action: Action::StopSearch,
        title: "Stop search",
        hint: "checkpoint and return to the wizard",
        keys: "esc",
        scope: Scope::LiveSearch,
    },
    Command {
        action: Action::DismissFinished,
        title: "Dismiss finished search",
        hint: "",
        keys: "enter",
        scope: Scope::LiveSearch,
    },
    Command {
        action: Action::ResumeSelectedRun,
        title: "Resume run",
        hint: "continue from its checkpoint",
        keys: "r",
        scope: Scope::Tab(Tab::Runs),
    },
    Command {
        action: Action::ExportSelectedRun,
        title: "Export run to JSON",
        hint: "",
        keys: "e",
        scope: Scope::Tab(Tab::Runs),
    },
    Command {
        action: Action::DeleteSelectedRun,
        title: "Delete run",
        hint: "asks for confirmation",
        keys: "d",
        scope: Scope::Tab(Tab::Runs),
    },
    Command {
        action: Action::RefreshRuns,
        title: "Reload runs",
        hint: "",
        keys: "F5",
        scope: Scope::Tab(Tab::Runs),
    },
    Command {
        action: Action::UseSelectedTemplate,
        title: "Hunt this template",
        hint: "opens the wizard preloaded",
        keys: "enter",
        scope: Scope::Tab(Tab::Gallery),
    },
    Command {
        action: Action::ValidateDigitFile,
        title: "Validate digit file",
        hint: "count usable digits",
        keys: "v",
        scope: Scope::Tab(Tab::Data),
    },
    Command {
        action: Action::ImportDigitFile,
        title: "Import digits into cache",
        hint: "",
        keys: "i",
        scope: Scope::Tab(Tab::Data),
    },
    Command {
        action: Action::CycleTheme,
        title: "Cycle theme",
        hint: "dark / light / mono",
        keys: "t",
        scope: Scope::Tab(Tab::Settings),
    },
    Command {
        action: Action::ToggleUnicode,
        title: "Toggle Unicode glyphs",
        hint: "off means pure ASCII",
        keys: "u",
        scope: Scope::Tab(Tab::Settings),
    },
    Command {
        action: Action::SaveSettings,
        title: "Save settings",
        hint: "writes config.toml",
        keys: "ctrl+s",
        scope: Scope::Tab(Tab::Settings),
    },
];

/// Which tab and mode the user is in, used to decide whether a scoped command
/// is currently available.
#[derive(Clone, Copy, Debug)]
pub struct ActionContext {
    pub tab: Tab,
    pub search_active: bool,
}

impl Scope {
    pub fn is_available(self, context: ActionContext) -> bool {
        match self {
            Self::Global => true,
            Self::Tab(tab) => context.tab == tab,
            Self::LiveSearch => context.tab == Tab::Hunt && context.search_active,
            Self::Wizard => context.tab == Tab::Hunt && !context.search_active,
        }
    }
}

pub fn available(context: ActionContext) -> impl Iterator<Item = &'static Command> {
    COMMANDS
        .iter()
        .filter(move |command| command.scope.is_available(context))
}

/// Maps a keypress to an action within the current context.
///
/// Single-letter bindings are matched only *after* the caller has given a
/// focused text input its chance, which is what makes both typing and shortcuts
/// possible on the same screen.
pub fn resolve(key: KeyEvent, context: ActionContext) -> Option<Action> {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let shift = key.modifiers.contains(KeyModifiers::SHIFT);

    if ctrl {
        return match key.code {
            KeyCode::Char('p') => Some(Action::OpenPalette),
            KeyCode::Char('q') => Some(Action::Quit),
            KeyCode::Char('s') if context.tab == Tab::Settings => Some(Action::SaveSettings),
            _ => None,
        };
    }

    match key.code {
        KeyCode::BackTab => return Some(Action::PrevTab),
        KeyCode::Tab if shift => return Some(Action::PrevTab),
        KeyCode::Tab => return Some(Action::NextTab),
        // F1 is the binding that always works: a focused text field claims '?'
        // as a character, and on those screens F1 is what the status bar shows.
        KeyCode::F(1) => return Some(Action::ToggleHelp),
        // Starting must not depend on having navigated to the last field.
        KeyCode::F(9) if context.tab == Tab::Hunt && !context.search_active => {
            return Some(Action::StartSearch);
        }
        KeyCode::Char('?') => return Some(Action::ToggleHelp),
        KeyCode::Char(digit @ '1'..='5') => {
            let index = digit as usize - '1' as usize;
            return Some(Action::Goto(Tab::ALL[index]));
        }
        KeyCode::F(2) if context.search_active => {
            return Some(Action::SetProfile(PerformanceProfile::Eco));
        }
        KeyCode::F(3) if context.search_active => {
            return Some(Action::SetProfile(PerformanceProfile::Balanced));
        }
        KeyCode::F(4) if context.search_active => {
            return Some(Action::SetProfile(PerformanceProfile::Performance));
        }
        KeyCode::F(5) if context.search_active => {
            return Some(Action::SetProfile(PerformanceProfile::Max));
        }
        _ => {}
    }

    match context.tab {
        Tab::Hunt if context.search_active => match key.code {
            KeyCode::Char(' ') => Some(Action::TogglePause),
            KeyCode::Char('p') => Some(Action::CycleProfile),
            KeyCode::Char('s') => Some(Action::SaveCheckpoint),
            KeyCode::Char('+') | KeyCode::Char('=') => Some(Action::AdjustWorkers(1)),
            KeyCode::Char('-') => Some(Action::AdjustWorkers(-1)),
            KeyCode::Char(']') => Some(Action::AdjustChunk(1)),
            KeyCode::Char('[') => Some(Action::AdjustChunk(-1)),
            KeyCode::Char('t') => Some(Action::CycleThermal),
            KeyCode::Char('g') => Some(Action::ToggleGpu),
            KeyCode::Char('m') => Some(Action::ToggleMetrics),
            KeyCode::Char('e') => Some(Action::ExportActiveRun),
            KeyCode::Esc => Some(Action::StopSearch),
            KeyCode::Enter => Some(Action::DismissFinished),
            _ => None,
        },
        Tab::Runs => match key.code {
            KeyCode::Char('r') => Some(Action::ResumeSelectedRun),
            KeyCode::Char('e') => Some(Action::ExportSelectedRun),
            KeyCode::Char('d') => Some(Action::DeleteSelectedRun),
            KeyCode::F(5) => Some(Action::RefreshRuns),
            _ => None,
        },
        Tab::Gallery => match key.code {
            KeyCode::Enter => Some(Action::UseSelectedTemplate),
            _ => None,
        },
        Tab::Data => match key.code {
            KeyCode::Char('v') => Some(Action::ValidateDigitFile),
            KeyCode::Char('i') => Some(Action::ImportDigitFile),
            _ => None,
        },
        Tab::Settings => match key.code {
            KeyCode::Char('t') => Some(Action::CycleTheme),
            KeyCode::Char('u') => Some(Action::ToggleUnicode),
            _ => None,
        },
        Tab::Hunt => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn context(tab: Tab, search_active: bool) -> ActionContext {
        ActionContext { tab, search_active }
    }

    #[test]
    fn digits_switch_tabs_everywhere() {
        for (index, tab) in Tab::ALL.iter().enumerate() {
            let digit = char::from(b'1' + index as u8);
            assert_eq!(
                resolve(key(KeyCode::Char(digit)), context(Tab::Runs, false)),
                Some(Action::Goto(*tab))
            );
        }
    }

    #[test]
    fn live_search_keys_do_nothing_in_the_wizard() {
        // `space` must not "pause" a search that is not running.
        assert_eq!(
            resolve(key(KeyCode::Char(' ')), context(Tab::Hunt, false)),
            None
        );
        assert_eq!(
            resolve(key(KeyCode::Char(' ')), context(Tab::Hunt, true)),
            Some(Action::TogglePause)
        );
    }

    #[test]
    fn tab_scoped_keys_do_not_leak_between_tabs() {
        // `t` is thermal mode during a search and theme cycling in settings.
        assert_eq!(
            resolve(key(KeyCode::Char('t')), context(Tab::Hunt, true)),
            Some(Action::CycleThermal)
        );
        assert_eq!(
            resolve(key(KeyCode::Char('t')), context(Tab::Settings, false)),
            Some(Action::CycleTheme)
        );
        assert_eq!(
            resolve(key(KeyCode::Char('t')), context(Tab::Runs, false)),
            None
        );
    }

    #[test]
    fn f9_starts_a_search_from_any_field_in_the_wizard() {
        assert_eq!(
            resolve(key(KeyCode::F(9)), context(Tab::Hunt, false)),
            Some(Action::StartSearch)
        );
        // But not while one is already running, or on another tab.
        assert_eq!(resolve(key(KeyCode::F(9)), context(Tab::Hunt, true)), None);
        assert_eq!(resolve(key(KeyCode::F(9)), context(Tab::Runs, false)), None);
    }

    #[test]
    fn the_profile_cycles_from_a_single_letter_during_a_search() {
        // The old TUI used 1-4 for profiles; those now switch tabs, so the
        // live-search profile needs a reachable binding of its own.
        assert_eq!(
            resolve(key(KeyCode::Char('p')), context(Tab::Hunt, true)),
            Some(Action::CycleProfile)
        );
        assert_eq!(
            resolve(key(KeyCode::Char('p')), context(Tab::Hunt, false)),
            None
        );
    }

    #[test]
    fn f1_opens_help_even_where_a_text_field_would_eat_the_question_mark() {
        assert_eq!(
            resolve(key(KeyCode::F(1)), context(Tab::Data, false)),
            Some(Action::ToggleHelp)
        );
        // F1 must not be stolen by the live-search profile block either.
        assert_eq!(
            resolve(key(KeyCode::F(1)), context(Tab::Hunt, true)),
            Some(Action::ToggleHelp)
        );
        assert_eq!(
            resolve(key(KeyCode::F(2)), context(Tab::Hunt, true)),
            Some(Action::SetProfile(PerformanceProfile::Eco))
        );
    }

    #[test]
    fn quitting_requires_a_modifier() {
        // A bare `q` used to quit, which is hostile next to text fields.
        assert_eq!(
            resolve(key(KeyCode::Char('q')), context(Tab::Hunt, false)),
            None
        );
        assert_eq!(
            resolve(
                KeyEvent::new(KeyCode::Char('q'), KeyModifiers::CONTROL),
                context(Tab::Hunt, false)
            ),
            Some(Action::Quit)
        );
    }

    #[test]
    fn scope_filtering_matches_the_context() {
        let wizard: Vec<_> = available(context(Tab::Hunt, false))
            .map(|command| command.action)
            .collect();
        assert!(wizard.contains(&Action::StartSearch));
        assert!(!wizard.contains(&Action::TogglePause));

        let live: Vec<_> = available(context(Tab::Hunt, true))
            .map(|command| command.action)
            .collect();
        assert!(live.contains(&Action::TogglePause));
        assert!(!live.contains(&Action::StartSearch));
    }

    #[test]
    fn every_bound_key_resolves_back_to_its_command() {
        // Guards against a registry entry advertising a key that resolve() does
        // not actually honour.
        for command in COMMANDS {
            let Some(code) = single_char_code(command.keys) else {
                continue;
            };
            let context = match command.scope {
                Scope::Global => context(Tab::Gallery, false),
                Scope::Tab(tab) => context(tab, false),
                Scope::LiveSearch => context(Tab::Hunt, true),
                Scope::Wizard => context(Tab::Hunt, false),
            };
            if let Some(resolved) = resolve(key(code), context) {
                assert_eq!(
                    resolved, command.action,
                    "key {:?} advertised by {:?}",
                    command.keys, command.title
                );
            }
        }
    }

    fn single_char_code(keys: &str) -> Option<KeyCode> {
        let mut chars = keys.chars();
        let first = chars.next()?;
        if chars.next().is_some() {
            return None;
        }
        Some(KeyCode::Char(first))
    }
}
