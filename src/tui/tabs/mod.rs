//! The five top-level tabs. Each owns its own state and draws itself; the
//! previous design kept all eight screens' state in one struct and dispatched
//! through two giant `match` blocks.

pub mod data;
pub mod gallery;
pub mod hunt;
pub mod runs;
pub mod settings;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tab {
    Hunt,
    Runs,
    Gallery,
    Data,
    Settings,
}

impl Tab {
    pub const ALL: [Tab; 5] = [Tab::Hunt, Tab::Runs, Tab::Gallery, Tab::Data, Tab::Settings];

    pub fn title(self) -> &'static str {
        match self {
            Self::Hunt => "HUNT",
            Self::Runs => "RUNS",
            Self::Gallery => "GALLERY",
            Self::Data => "DATA",
            Self::Settings => "SETTINGS",
        }
    }

    pub fn index(self) -> usize {
        Self::ALL
            .iter()
            .position(|tab| *tab == self)
            .unwrap_or_default()
    }

    pub fn step(self, delta: i32) -> Self {
        let len = Self::ALL.len() as i32;
        let next = (self.index() as i32 + delta).rem_euclid(len);
        Self::ALL[next as usize]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stepping_wraps_around_both_ends() {
        assert_eq!(Tab::Hunt.step(-1), Tab::Settings);
        assert_eq!(Tab::Settings.step(1), Tab::Hunt);
        assert_eq!(Tab::Hunt.step(1), Tab::Runs);
    }

    #[test]
    fn indices_match_the_digit_shortcuts() {
        for (index, tab) in Tab::ALL.iter().enumerate() {
            assert_eq!(tab.index(), index);
        }
    }
}
