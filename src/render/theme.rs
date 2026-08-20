//! Visual language for every surface pi-casso draws: the TUI and the plain
//! terminal output share one palette and one glyph set.
//!
//! This replaces the `color: bool` flag that used to be threaded through ~30
//! rendering functions. Colour and glyph choices are now independent: a
//! terminal without colour can still use Unicode blocks, and a terminal without
//! a Unicode font can still use colour.

use ratatui::style::{Color, Modifier, Style};
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ThemeName {
    #[default]
    Dark,
    Light,
    Mono,
}

impl ThemeName {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Dark => "dark",
            Self::Light => "light",
            Self::Mono => "mono",
        }
    }

    pub const ALL: [Self; 3] = [Self::Dark, Self::Light, Self::Mono];

    pub fn from_str_name(value: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|candidate| candidate.as_str() == value)
    }
}

/// Glyphs are split from colours so that `--ascii` degrades shape without
/// degrading colour, which the old `if color { "█" } else { "#" }` could not do.
#[derive(Clone, Copy, Debug)]
pub struct Glyphs {
    pub filled: &'static str,
    pub empty: &'static str,
    /// Upper half block: the workhorse of the double-resolution bitmap render.
    pub half: &'static str,
    pub bullet: &'static str,
    pub arrow: &'static str,
    pub tab_marker: &'static str,
    pub rising: &'static str,
    pub scroll_thumb: &'static str,
    pub scroll_track: &'static str,
}

impl Glyphs {
    pub const UNICODE: Self = Self {
        filled: "█",
        empty: "·",
        half: "▀",
        bullet: "●",
        arrow: "▸",
        tab_marker: "┃",
        rising: "▲",
        scroll_thumb: "█",
        scroll_track: "│",
    };

    pub const ASCII: Self = Self {
        filled: "#",
        empty: ".",
        half: "\"",
        bullet: "*",
        arrow: ">",
        tab_marker: "|",
        rising: "^",
        scroll_thumb: "#",
        scroll_track: "|",
    };
}

#[derive(Clone, Copy, Debug)]
pub struct Theme {
    pub name: ThemeName,
    pub color: bool,
    pub unicode: bool,
    pub text: Color,
    pub dim: Color,
    pub border: Color,
    pub border_focus: Color,
    pub accent: Color,
    pub success: Color,
    pub warning: Color,
    pub danger: Color,
    /// Background of bitmap canvases; also the "off" pixel in half-block mode.
    pub canvas_bg: Color,
    /// Background of the pixels that fall inside the target's placement window.
    pub canvas_target_bg: Color,
    pub glyphs: Glyphs,
}

impl Default for Theme {
    fn default() -> Self {
        Self::new(ThemeName::Dark, true, true)
    }
}

impl Theme {
    pub fn new(name: ThemeName, color: bool, unicode: bool) -> Self {
        let glyphs = if unicode {
            Glyphs::UNICODE
        } else {
            Glyphs::ASCII
        };
        match name {
            // Mono never emits colour, whatever the caller asked for.
            ThemeName::Mono => Self {
                name,
                color: false,
                unicode,
                text: Color::Reset,
                dim: Color::Reset,
                border: Color::Reset,
                border_focus: Color::Reset,
                accent: Color::Reset,
                success: Color::Reset,
                warning: Color::Reset,
                danger: Color::Reset,
                canvas_bg: Color::Reset,
                canvas_target_bg: Color::Reset,
                glyphs,
            },
            ThemeName::Dark => Self {
                name,
                color,
                unicode,
                text: Color::Rgb(220, 223, 228),
                // Bright enough to stay legible, unlike the old DarkGray.
                dim: Color::Rgb(129, 135, 148),
                border: Color::Rgb(68, 74, 88),
                border_focus: Color::Rgb(122, 162, 247),
                accent: Color::Rgb(122, 162, 247),
                success: Color::Rgb(126, 208, 130),
                warning: Color::Rgb(224, 175, 104),
                danger: Color::Rgb(232, 113, 121),
                canvas_bg: Color::Rgb(28, 31, 38),
                canvas_target_bg: Color::Rgb(41, 46, 57),
                glyphs,
            },
            ThemeName::Light => Self {
                name,
                color,
                unicode,
                text: Color::Rgb(38, 42, 51),
                // On a light background DarkGray is invisible; go darker, not lighter.
                dim: Color::Rgb(104, 110, 124),
                border: Color::Rgb(188, 194, 206),
                border_focus: Color::Rgb(42, 96, 196),
                accent: Color::Rgb(42, 96, 196),
                success: Color::Rgb(30, 122, 62),
                warning: Color::Rgb(158, 100, 12),
                danger: Color::Rgb(184, 44, 52),
                canvas_bg: Color::Rgb(238, 240, 245),
                canvas_target_bg: Color::Rgb(220, 224, 234),
                glyphs,
            },
        }
    }

    /// Any colour request collapses to the terminal default when colour is off,
    /// so callers never have to branch on it themselves.
    fn paint(self, color: Color) -> Style {
        if self.color {
            Style::default().fg(color)
        } else {
            Style::default()
        }
    }

    pub fn text_style(self) -> Style {
        self.paint(self.text)
    }

    pub fn dim_style(self) -> Style {
        if self.color {
            Style::default().fg(self.dim)
        } else {
            Style::default().add_modifier(Modifier::DIM)
        }
    }

    pub fn accent_style(self) -> Style {
        self.paint(self.accent).add_modifier(Modifier::BOLD)
    }

    pub fn success_style(self) -> Style {
        self.paint(self.success).add_modifier(Modifier::BOLD)
    }

    pub fn warning_style(self) -> Style {
        self.paint(self.warning).add_modifier(Modifier::BOLD)
    }

    pub fn danger_style(self) -> Style {
        self.paint(self.danger).add_modifier(Modifier::BOLD)
    }

    pub fn border_style(self) -> Style {
        self.paint(self.border)
    }

    pub fn border_focus_style(self) -> Style {
        self.paint(self.border_focus)
    }

    /// Selection has to survive a monochrome terminal, where colour alone says nothing.
    pub fn selected_style(self) -> Style {
        if self.color {
            Style::default()
                .fg(self.accent)
                .add_modifier(Modifier::BOLD | Modifier::REVERSED)
        } else {
            Style::default().add_modifier(Modifier::REVERSED)
        }
    }

    /// A clickable key cap. Filled, so that a binding the mouse can activate
    /// looks different from one that is merely being described.
    pub fn button_style(self) -> Style {
        if self.color {
            Style::default()
                .fg(self.canvas_bg)
                .bg(self.accent)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().add_modifier(Modifier::REVERSED | Modifier::BOLD)
        }
    }

    /// The primary action on a screen — the one a first-time user should press.
    pub fn primary_button_style(self) -> Style {
        if self.color {
            Style::default()
                .fg(self.canvas_bg)
                .bg(self.success)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().add_modifier(Modifier::REVERSED | Modifier::BOLD)
        }
    }

    pub fn canvas_bg_style(self) -> Style {
        if self.color {
            Style::default().bg(self.canvas_bg)
        } else {
            Style::default()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn buttons_stay_visible_without_colour() {
        // Colour is the only difference between a key cap and plain text, so a
        // monochrome terminal has to fall back to reverse video.
        let mono = Theme::new(ThemeName::Mono, true, true);
        assert!(
            mono.button_style()
                .add_modifier
                .contains(Modifier::REVERSED)
        );
        assert!(
            mono.primary_button_style()
                .add_modifier
                .contains(Modifier::REVERSED)
        );
    }

    #[test]
    fn the_primary_button_is_distinct_from_an_ordinary_one() {
        let theme = Theme::new(ThemeName::Dark, true, true);
        assert_ne!(theme.button_style().bg, theme.primary_button_style().bg);
    }

    #[test]
    fn mono_theme_never_emits_colour() {
        let theme = Theme::new(ThemeName::Mono, true, true);
        assert!(!theme.color);
        assert_eq!(theme.accent_style().fg, None);
        assert_eq!(theme.dim_style().fg, None);
    }

    #[test]
    fn colour_off_keeps_glyphs() {
        let theme = Theme::new(ThemeName::Dark, false, true);
        assert_eq!(theme.glyphs.filled, "█");
        assert_eq!(theme.accent_style().fg, None);
    }

    #[test]
    fn ascii_glyphs_are_independent_of_colour() {
        let theme = Theme::new(ThemeName::Dark, true, false);
        assert_eq!(theme.glyphs.filled, "#");
        assert_eq!(theme.accent_style().fg, Some(theme.accent));
    }

    #[test]
    fn theme_names_round_trip() {
        for name in ThemeName::ALL {
            assert_eq!(ThemeName::from_str_name(name.as_str()), Some(name));
        }
    }
}
