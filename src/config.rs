//! User configuration, persisted as TOML.
//!
//! Everything here is a *default* or a *preference*: nothing the search engine
//! needs to run correctly lives in this file, so a missing or corrupt config is
//! always recoverable by falling back to built-in defaults rather than failing.

use std::path::PathBuf;

use anyhow::{Context, Result, anyhow};
use directories::BaseDirs;
use serde::{Deserialize, Serialize};

use crate::performance::PerformanceProfile;
use crate::render::{Theme, ThemeName};
use crate::search::MatchMode;
use crate::storage::app_data_dir;

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub appearance: Appearance,
    pub search: SearchDefaults,
    pub paths: Paths,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct Appearance {
    pub theme: ThemeName,
    /// Unicode block glyphs and half-block bitmaps. Off means pure ASCII.
    pub unicode: bool,
    /// Upper bound on TUI redraws. The search worker can produce snapshots far
    /// faster than a terminal can usefully show them.
    pub max_fps: u32,
}

impl Default for Appearance {
    fn default() -> Self {
        Self {
            theme: ThemeName::Dark,
            unicode: true,
            max_fps: 30,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct SearchDefaults {
    pub profile: PerformanceProfile,
    pub match_mode: MatchMode,
    pub width: usize,
    pub height: usize,
    pub canvas_width: usize,
    pub canvas_height: usize,
    pub threshold: u8,
    pub template: String,
}

impl Default for SearchDefaults {
    fn default() -> Self {
        Self {
            profile: PerformanceProfile::Balanced,
            match_mode: MatchMode::Emergence,
            width: 12,
            height: 12,
            canvas_width: 24,
            canvas_height: 24,
            threshold: 5,
            template: "arch".to_string(),
        }
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct Paths {
    /// Where `export` writes JSON. `None` means `<data dir>/exports`, which is
    /// what keeps export artefacts out of whatever directory happened to be the
    /// working directory.
    pub export_dir: Option<PathBuf>,
}

/// The outcome of loading a config: a usable config plus, possibly, something
/// the user should know about their file. A broken config must never be fatal,
/// but it must not be silent either.
pub struct ConfigLoad {
    pub config: Config,
    pub warning: Option<String>,
}

impl Config {
    /// Never fails: a missing file yields defaults, an unreadable or malformed
    /// file yields defaults plus a warning for the caller to surface.
    pub fn load() -> ConfigLoad {
        let path = match config_path() {
            Ok(path) => path,
            Err(err) => {
                return ConfigLoad {
                    config: Self::default(),
                    warning: Some(format!("could not locate config: {err:#}")),
                };
            }
        };
        if !path.exists() {
            return ConfigLoad {
                config: Self::default(),
                warning: None,
            };
        }
        match std::fs::read_to_string(&path) {
            Ok(body) => match toml::from_str::<Self>(&body) {
                Ok(config) => ConfigLoad {
                    config,
                    warning: None,
                },
                Err(err) => ConfigLoad {
                    config: Self::default(),
                    warning: Some(format!(
                        "{} is not valid config, using defaults: {err}",
                        path.display()
                    )),
                },
            },
            Err(err) => ConfigLoad {
                config: Self::default(),
                warning: Some(format!("could not read {}: {err}", path.display())),
            },
        }
    }

    /// Written via a temporary file and a rename so an interrupted save cannot
    /// leave a half-written config behind.
    pub fn save(&self) -> Result<PathBuf> {
        let path = config_path()?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("failed to create {}", parent.display()))?;
        }
        let body = toml::to_string_pretty(self).context("failed to serialize config")?;
        let temp = path.with_extension("toml.tmp");
        std::fs::write(&temp, body)
            .with_context(|| format!("failed to write {}", temp.display()))?;
        std::fs::rename(&temp, &path)
            .with_context(|| format!("failed to replace {}", path.display()))?;
        Ok(path)
    }

    /// `color_enabled` comes from the CLI flag and `NO_COLOR`, which override the
    /// configured theme without discarding the user's glyph preference.
    pub fn theme(&self, color_enabled: bool) -> Theme {
        Theme::new(
            self.appearance.theme,
            color_enabled,
            self.appearance.unicode,
        )
    }

    pub fn export_dir(&self) -> Result<PathBuf> {
        match &self.paths.export_dir {
            Some(dir) => Ok(dir.clone()),
            None => Ok(app_data_dir()?.join("exports")),
        }
    }
}

/// `PI_CASSO_CONFIG` points at a file; `PI_CASSO_DATA_DIR` (already used to
/// isolate the database in tests) also isolates the config so a test run can
/// never read or clobber the developer's real settings.
pub fn config_path() -> Result<PathBuf> {
    if let Some(explicit) = std::env::var_os("PI_CASSO_CONFIG") {
        return Ok(PathBuf::from(explicit));
    }
    if std::env::var_os("PI_CASSO_DATA_DIR").is_some() {
        return Ok(app_data_dir()?.join("config.toml"));
    }
    let base = BaseDirs::new().ok_or_else(|| anyhow!("could not determine config directory"))?;
    Ok(base.config_dir().join("pi-casso").join("config.toml"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_round_trip_through_toml() {
        let config = Config::default();
        let body = toml::to_string_pretty(&config).unwrap();
        let parsed: Config = toml::from_str(&body).unwrap();
        assert_eq!(config, parsed);
    }

    #[test]
    fn partial_config_keeps_defaults_for_missing_sections() {
        let parsed: Config = toml::from_str("[appearance]\ntheme = \"light\"\n").unwrap();
        assert_eq!(parsed.appearance.theme, ThemeName::Light);
        // Untouched fields must not collapse to zero values.
        assert!(parsed.appearance.unicode);
        assert_eq!(parsed.search.width, 12);
        assert_eq!(parsed.search.threshold, 5);
    }

    #[test]
    fn malformed_config_is_reported_not_fatal() {
        let parsed = toml::from_str::<Config>("this is not toml at all {{{");
        assert!(parsed.is_err());
    }

    #[test]
    fn unknown_keys_are_rejected_so_typos_surface() {
        // A silently-ignored typo is worse than a warning: the user thinks the
        // setting took effect.
        let parsed = toml::from_str::<Config>("[appearance]\nthemee = \"light\"\n");
        assert!(parsed.is_err());
    }

    #[test]
    fn colour_flag_overrides_theme_without_losing_glyphs() {
        let mut config = Config::default();
        config.appearance.unicode = true;
        let theme = config.theme(false);
        assert!(!theme.color);
        assert!(theme.unicode);
    }
}
