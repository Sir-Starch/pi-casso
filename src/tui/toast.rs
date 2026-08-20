//! Transient status messages.
//!
//! The old TUI kept a single `Option<String>` that, once set, stayed on screen
//! until something else replaced it — so a stale "delete cancelled" could sit
//! under a live search for hours. Toasts expire, stack, and carry a severity.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use ratatui::style::Style;

use crate::render::Theme;

const MAX_VISIBLE: usize = 4;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ToastLevel {
    Info,
    Success,
    Warning,
    Error,
}

impl ToastLevel {
    /// Problems linger longer than confirmations, because they are more likely
    /// to need reading twice.
    fn lifetime(self) -> Duration {
        match self {
            Self::Info | Self::Success => Duration::from_secs(4),
            Self::Warning => Duration::from_secs(7),
            Self::Error => Duration::from_secs(10),
        }
    }

    pub fn style(self, theme: &Theme) -> Style {
        match self {
            Self::Info => theme.accent_style(),
            Self::Success => theme.success_style(),
            Self::Warning => theme.warning_style(),
            Self::Error => theme.danger_style(),
        }
    }

    pub fn tag(self) -> &'static str {
        match self {
            Self::Info => "info",
            Self::Success => "ok",
            Self::Warning => "warn",
            Self::Error => "error",
        }
    }
}

#[derive(Clone, Debug)]
pub struct Toast {
    pub text: String,
    pub level: ToastLevel,
    expires_at: Instant,
}

#[derive(Default)]
pub struct Toasts {
    items: VecDeque<Toast>,
}

impl Toasts {
    pub fn push(&mut self, level: ToastLevel, text: impl Into<String>) {
        let text = text.into();
        // Repeating the same message should refresh it, not stack duplicates —
        // a search that fails to checkpoint every second would otherwise bury
        // everything else.
        if let Some(existing) = self
            .items
            .iter_mut()
            .find(|toast| toast.text == text && toast.level == level)
        {
            existing.expires_at = Instant::now() + level.lifetime();
            return;
        }
        self.items.push_back(Toast {
            text,
            level,
            expires_at: Instant::now() + level.lifetime(),
        });
        while self.items.len() > MAX_VISIBLE {
            self.items.pop_front();
        }
    }

    pub fn info(&mut self, text: impl Into<String>) {
        self.push(ToastLevel::Info, text);
    }

    pub fn success(&mut self, text: impl Into<String>) {
        self.push(ToastLevel::Success, text);
    }

    pub fn warn(&mut self, text: impl Into<String>) {
        self.push(ToastLevel::Warning, text);
    }

    pub fn error(&mut self, text: impl Into<String>) {
        self.push(ToastLevel::Error, text);
    }

    /// Reports whether anything expired, so the caller knows a redraw is due.
    pub fn prune(&mut self) -> bool {
        let now = Instant::now();
        let before = self.items.len();
        self.items.retain(|toast| toast.expires_at > now);
        self.items.len() != before
    }

    pub fn visible(&self) -> impl Iterator<Item = &Toast> {
        self.items.iter()
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expired_toasts_disappear() {
        let mut toasts = Toasts::default();
        toasts.push(ToastLevel::Info, "hello");
        assert_eq!(toasts.visible().count(), 1);
        // Reach in and backdate rather than sleeping for four seconds.
        toasts.items[0].expires_at = Instant::now() - Duration::from_secs(1);
        assert!(toasts.prune());
        assert!(toasts.is_empty());
    }

    #[test]
    fn pruning_nothing_reports_no_change() {
        let mut toasts = Toasts::default();
        toasts.info("still here");
        assert!(!toasts.prune());
        assert_eq!(toasts.visible().count(), 1);
    }

    #[test]
    fn the_queue_is_capped() {
        let mut toasts = Toasts::default();
        for index in 0..10 {
            toasts.info(format!("message {index}"));
        }
        assert_eq!(toasts.visible().count(), MAX_VISIBLE);
        // The oldest are dropped, the newest survive.
        assert_eq!(toasts.visible().next().unwrap().text, "message 6");
    }

    #[test]
    fn repeats_refresh_instead_of_stacking() {
        let mut toasts = Toasts::default();
        toasts.error("database is locked");
        toasts.error("database is locked");
        toasts.error("database is locked");
        assert_eq!(toasts.visible().count(), 1);
    }

    #[test]
    fn errors_outlive_confirmations() {
        assert!(ToastLevel::Error.lifetime() > ToastLevel::Success.lifetime());
    }
}
