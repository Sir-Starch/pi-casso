//! The SETTINGS tab. It used to be three read-only lines; it now edits the
//! config file the rest of the app reads.

use anyhow::Result;
use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::widgets::{Paragraph, Wrap};

use crate::config::{Config, config_path};
use crate::render::{Theme, ThemeName};
use crate::tui::form::{Field, Form, FormOutcome};
use crate::tui::widgets::{RowRegion, dim_line, field_line, focused_panel, panel};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SettingField {
    Theme,
    Unicode,
    MaxFps,
    Profile,
    MatchMode,
    Width,
    Height,
    CanvasWidth,
    CanvasHeight,
    Threshold,
}

/// Derived from `ThemeName::ALL` so a new theme cannot be forgotten here.
const THEMES: [&str; 3] = [
    ThemeName::ALL[0].as_str(),
    ThemeName::ALL[1].as_str(),
    ThemeName::ALL[2].as_str(),
];
const PROFILES: [&str; 4] = ["eco", "balanced", "performance", "max"];
const MATCH_MODES: [&str; 3] = ["emergence", "threshold", "exact"];

pub struct SettingsTab {
    pub form: Form<SettingField>,
    /// True while the form differs from what is on disk.
    pub dirty: bool,
}

impl SettingsTab {
    pub fn new(config: &Config) -> Self {
        let theme_index = THEMES
            .iter()
            .position(|name| *name == config.appearance.theme.as_str())
            .unwrap_or(0);
        let profile_index = PROFILES
            .iter()
            .position(|name| *name == config.search.profile.as_str())
            .unwrap_or(1);
        let match_index = MATCH_MODES
            .iter()
            .position(|name| *name == config.search.match_mode.as_str())
            .unwrap_or(0);
        Self {
            form: Form::new(vec![
                Field::choice(
                    SettingField::Theme,
                    "Theme",
                    "dark, light, or monochrome",
                    &THEMES,
                    theme_index,
                ),
                Field::toggle(
                    SettingField::Unicode,
                    "Unicode glyphs",
                    "off falls back to pure ASCII",
                    config.appearance.unicode,
                ),
                Field::number(
                    SettingField::MaxFps,
                    "Max FPS",
                    "upper bound on redraws per second",
                    config.appearance.max_fps,
                    1,
                    120,
                ),
                Field::choice(
                    SettingField::Profile,
                    "Default profile",
                    "starting performance profile for new hunts",
                    &PROFILES,
                    profile_index,
                ),
                Field::choice(
                    SettingField::MatchMode,
                    "Default match mode",
                    "starting match mode for new hunts",
                    &MATCH_MODES,
                    match_index,
                ),
                Field::number(
                    SettingField::Width,
                    "Default width",
                    "",
                    config.search.width,
                    1,
                    128,
                ),
                Field::number(
                    SettingField::Height,
                    "Default height",
                    "",
                    config.search.height,
                    1,
                    128,
                ),
                Field::number(
                    SettingField::CanvasWidth,
                    "Default canvas width",
                    "",
                    config.search.canvas_width,
                    1,
                    256,
                ),
                Field::number(
                    SettingField::CanvasHeight,
                    "Default canvas height",
                    "",
                    config.search.canvas_height,
                    1,
                    256,
                ),
                Field::number(
                    SettingField::Threshold,
                    "Default threshold",
                    "",
                    config.search.threshold,
                    0,
                    9,
                ),
            ]),
            dirty: false,
        }
    }

    pub fn handle_key(&mut self, key: crossterm::event::KeyEvent) -> FormOutcome {
        let outcome = self.form.handle_key(key);
        if outcome == FormOutcome::Consumed {
            self.dirty = true;
        }
        outcome
    }

    pub fn cycle_theme(&mut self) {
        let next = (self.form.choice(SettingField::Theme) + 1) % THEMES.len();
        self.form.set_choice(SettingField::Theme, next);
        self.dirty = true;
    }

    pub fn toggle_unicode(&mut self) {
        let current = self.form.toggled(SettingField::Unicode);
        self.form.set_toggle(SettingField::Unicode, !current);
        self.dirty = true;
    }

    /// Folds the form back into a config. Unparseable numbers keep whatever the
    /// config already had rather than resetting to zero.
    pub fn apply(&self, base: &Config) -> Config {
        let mut config = base.clone();
        config.appearance.theme =
            ThemeName::from_str_name(THEMES[self.form.choice(SettingField::Theme)])
                .unwrap_or(ThemeName::Dark);
        config.appearance.unicode = self.form.toggled(SettingField::Unicode);
        config.appearance.max_fps = parse_or(
            self.form.text(SettingField::MaxFps),
            config.appearance.max_fps as u64,
        ) as u32;
        config.search.profile = parse_profile(PROFILES[self.form.choice(SettingField::Profile)])
            .unwrap_or(config.search.profile);
        config.search.match_mode = crate::search::MatchMode::from_str(
            MATCH_MODES[self.form.choice(SettingField::MatchMode)],
        )
        .unwrap_or(config.search.match_mode);
        config.search.width = parse_or(
            self.form.text(SettingField::Width),
            config.search.width as u64,
        ) as usize;
        config.search.height = parse_or(
            self.form.text(SettingField::Height),
            config.search.height as u64,
        ) as usize;
        config.search.canvas_width = parse_or(
            self.form.text(SettingField::CanvasWidth),
            config.search.canvas_width as u64,
        ) as usize;
        config.search.canvas_height = parse_or(
            self.form.text(SettingField::CanvasHeight),
            config.search.canvas_height as u64,
        ) as usize;
        config.search.threshold = parse_or(
            self.form.text(SettingField::Threshold),
            config.search.threshold as u64,
        )
        .min(9) as u8;
        config
    }

    pub fn save(&mut self, base: &Config) -> Result<(Config, std::path::PathBuf)> {
        let config = self.apply(base);
        let path = config.save()?;
        self.dirty = false;
        Ok((config, path))
    }

    pub fn draw(&mut self, frame: &mut Frame<'_>, area: Rect, theme: &Theme) -> RowRegion {
        let rows = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Min(12), Constraint::Length(6)])
            .split(area);

        let title = if self.dirty {
            "settings — unsaved"
        } else {
            "settings"
        };
        frame.render_widget(
            Paragraph::new(self.form.lines(theme, 24)).block(focused_panel(title, theme)),
            rows[0],
        );

        let mut lines = vec![dim_line(self.form.hint(), theme)];
        match config_path() {
            Ok(path) => lines.push(field_line("config file", path.display().to_string(), theme)),
            Err(err) => lines.push(dim_line(err.to_string(), theme)),
        }
        lines.push(field_line("live theme", theme.name.as_str(), theme));
        lines.push(dim_line(
            "Theme and glyph changes apply immediately; ctrl+s writes them to disk.",
            theme,
        ));
        frame.render_widget(
            Paragraph::new(lines)
                .wrap(Wrap { trim: true })
                .block(panel("", theme)),
            rows[1],
        );
        RowRegion::panel(rows[0], 0)
    }
}

fn parse_or(value: &str, fallback: u64) -> u64 {
    value.trim().parse::<u64>().unwrap_or(fallback)
}

fn parse_profile(value: &str) -> Option<crate::performance::PerformanceProfile> {
    use crate::performance::PerformanceProfile::*;
    match value {
        "eco" => Some(Eco),
        "balanced" => Some(Balanced),
        "performance" => Some(Performance),
        "max" => Some(Max),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_form_round_trips_a_config() {
        let mut config = Config::default();
        config.appearance.theme = ThemeName::Light;
        config.appearance.unicode = false;
        config.search.width = 16;
        config.search.threshold = 7;
        let tab = SettingsTab::new(&config);
        assert_eq!(tab.apply(&Config::default()), config);
    }

    #[test]
    fn cycling_the_theme_visits_every_preset() {
        let mut tab = SettingsTab::new(&Config::default());
        let mut seen = vec![tab.apply(&Config::default()).appearance.theme];
        for _ in 0..2 {
            tab.cycle_theme();
            seen.push(tab.apply(&Config::default()).appearance.theme);
        }
        seen.sort_by_key(|name| name.as_str());
        seen.dedup();
        assert_eq!(seen.len(), 3);
    }

    #[test]
    fn an_unparseable_number_keeps_the_existing_value() {
        let mut tab = SettingsTab::new(&Config::default());
        tab.form.set_text(SettingField::Width, "");
        let base = Config {
            search: crate::config::SearchDefaults {
                width: 20,
                ..Default::default()
            },
            ..Default::default()
        };
        assert_eq!(tab.apply(&base).search.width, 20);
    }

    #[test]
    fn threshold_is_clamped_to_a_single_digit() {
        let mut tab = SettingsTab::new(&Config::default());
        tab.form.set_text(SettingField::Threshold, "42");
        assert_eq!(tab.apply(&Config::default()).search.threshold, 9);
    }

    #[test]
    fn editing_marks_the_form_dirty() {
        let mut tab = SettingsTab::new(&Config::default());
        assert!(!tab.dirty);
        tab.toggle_unicode();
        assert!(tab.dirty);
    }
}
