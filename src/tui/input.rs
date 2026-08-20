//! A text field with a real cursor.
//!
//! The previous wizard could only append to and pop from the end of a string,
//! and worse, it let single-letter shortcuts (`h`, `j`, `k`, `?`) win over the
//! field — so a path containing an `h` was literally untypeable. A focused
//! input now consumes printable keys before any shortcut is considered.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

#[derive(Clone, Debug, Default)]
pub struct TextInput {
    value: String,
    /// Position in *characters*, not bytes, so multi-byte input cannot split a
    /// character or panic on slicing.
    cursor: usize,
}

impl TextInput {
    pub fn new(value: impl Into<String>) -> Self {
        let value = value.into();
        let cursor = value.chars().count();
        Self { value, cursor }
    }

    pub fn value(&self) -> &str {
        &self.value
    }

    pub fn trimmed(&self) -> &str {
        self.value.trim()
    }

    pub fn is_empty(&self) -> bool {
        self.value.trim().is_empty()
    }

    pub fn set(&mut self, value: impl Into<String>) {
        self.value = value.into();
        self.cursor = self.value.chars().count();
    }

    /// Returns true when the key was consumed. Anything not consumed falls
    /// through to the shortcut layer.
    pub fn handle_key(&mut self, key: KeyEvent) -> bool {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Char('u') if ctrl => {
                self.value.clear();
                self.cursor = 0;
                true
            }
            KeyCode::Char('w') if ctrl => {
                self.delete_word_before();
                true
            }
            KeyCode::Char('a') if ctrl => {
                self.cursor = 0;
                true
            }
            KeyCode::Char('e') if ctrl => {
                self.cursor = self.value.chars().count();
                true
            }
            // Alt-modified letters stay available as shortcuts even while typing.
            KeyCode::Char(ch) if !ctrl && !key.modifiers.contains(KeyModifiers::ALT) => {
                self.insert(ch);
                true
            }
            KeyCode::Backspace => {
                self.delete_before();
                true
            }
            KeyCode::Delete => {
                self.delete_at();
                true
            }
            KeyCode::Left => {
                self.cursor = self.cursor.saturating_sub(1);
                true
            }
            KeyCode::Right => {
                self.cursor = (self.cursor + 1).min(self.value.chars().count());
                true
            }
            KeyCode::Home => {
                self.cursor = 0;
                true
            }
            KeyCode::End => {
                self.cursor = self.value.chars().count();
                true
            }
            _ => false,
        }
    }

    pub fn insert(&mut self, ch: char) {
        let index = self.byte_index(self.cursor);
        self.value.insert(index, ch);
        self.cursor += 1;
    }

    fn delete_before(&mut self) {
        if self.cursor == 0 {
            return;
        }
        let start = self.byte_index(self.cursor - 1);
        let end = self.byte_index(self.cursor);
        self.value.replace_range(start..end, "");
        self.cursor -= 1;
    }

    fn delete_at(&mut self) {
        if self.cursor >= self.value.chars().count() {
            return;
        }
        let start = self.byte_index(self.cursor);
        let end = self.byte_index(self.cursor + 1);
        self.value.replace_range(start..end, "");
    }

    fn delete_word_before(&mut self) {
        let chars: Vec<char> = self.value.chars().collect();
        let mut target = self.cursor;
        while target > 0 && chars[target - 1].is_whitespace() {
            target -= 1;
        }
        while target > 0 && !chars[target - 1].is_whitespace() {
            target -= 1;
        }
        let start = self.byte_index(target);
        let end = self.byte_index(self.cursor);
        self.value.replace_range(start..end, "");
        self.cursor = target;
    }

    fn byte_index(&self, char_index: usize) -> usize {
        self.value
            .char_indices()
            .nth(char_index)
            .map(|(index, _)| index)
            .unwrap_or(self.value.len())
    }

    /// The value with a block cursor drawn in, for display only.
    pub fn display_with_cursor(&self, focused: bool) -> String {
        if !focused {
            return self.value.clone();
        }
        let mut out: String = self.value.chars().take(self.cursor).collect();
        out.push('▏');
        out.extend(self.value.chars().skip(self.cursor));
        out
    }
}

/// A numeric field: the same editing behaviour, but non-digits never enter the
/// buffer in the first place, so parsing cannot be surprised later.
#[derive(Clone, Debug, Default)]
pub struct NumberInput {
    inner: TextInput,
}

impl NumberInput {
    pub fn new(value: impl ToString) -> Self {
        Self {
            inner: TextInput::new(value.to_string()),
        }
    }

    pub fn value(&self) -> &str {
        self.inner.value()
    }

    pub fn set(&mut self, value: impl ToString) {
        self.inner.set(value.to_string());
    }

    pub fn display_with_cursor(&self, focused: bool) -> String {
        self.inner.display_with_cursor(focused)
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> bool {
        if let KeyCode::Char(ch) = key.code {
            if !ch.is_ascii_digit() && !key.modifiers.contains(KeyModifiers::CONTROL) {
                return false;
            }
        }
        self.inner.handle_key(key)
    }

    /// Arrow-key nudging, clamped. An unparseable or empty field starts from the
    /// minimum rather than refusing to move.
    pub fn nudge(&mut self, delta: i64, min: i64, max: i64) {
        let current = self.inner.value().parse::<i64>().unwrap_or(min);
        self.inner
            .set((current + delta).clamp(min, max).to_string());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn ctrl(ch: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(ch), KeyModifiers::CONTROL)
    }

    #[test]
    fn shortcut_letters_are_typeable() {
        let mut input = TextInput::default();
        // The exact characters the old wizard stole for navigation and help.
        for ch in "hjkq?".chars() {
            assert!(input.handle_key(key(KeyCode::Char(ch))));
        }
        assert_eq!(input.value(), "hjkq?");
    }

    #[test]
    fn cursor_moves_and_inserts_in_the_middle() {
        let mut input = TextInput::new("abd");
        input.handle_key(key(KeyCode::Left));
        input.handle_key(key(KeyCode::Char('c')));
        assert_eq!(input.value(), "abcd");
        // The cursor sits after the inserted character, before the "d".
        assert_eq!(input.display_with_cursor(true), "abc▏d");
    }

    #[test]
    fn backspace_and_delete_respect_the_cursor() {
        let mut input = TextInput::new("abc");
        input.handle_key(key(KeyCode::Home));
        input.handle_key(key(KeyCode::Delete));
        assert_eq!(input.value(), "bc");
        input.handle_key(key(KeyCode::End));
        input.handle_key(key(KeyCode::Backspace));
        assert_eq!(input.value(), "b");
    }

    #[test]
    fn multibyte_editing_does_not_split_characters() {
        let mut input = TextInput::new("путь");
        input.handle_key(key(KeyCode::Backspace));
        assert_eq!(input.value(), "пут");
        input.handle_key(key(KeyCode::Home));
        input.handle_key(key(KeyCode::Char('к')));
        assert_eq!(input.value(), "кпут");
    }

    #[test]
    fn ctrl_u_clears_and_ctrl_w_deletes_a_word() {
        let mut input = TextInput::new("/home/user/pi digits.txt");
        input.handle_key(ctrl('w'));
        assert_eq!(input.value(), "/home/user/pi ");
        input.handle_key(ctrl('u'));
        assert_eq!(input.value(), "");
    }

    #[test]
    fn number_input_rejects_non_digits() {
        let mut input = NumberInput::new(12);
        assert!(!input.handle_key(key(KeyCode::Char('x'))));
        assert!(input.handle_key(key(KeyCode::Char('3'))));
        assert_eq!(input.value(), "123");
    }

    #[test]
    fn nudging_clamps_and_survives_an_empty_field() {
        let mut input = NumberInput::new(9);
        input.nudge(5, 0, 9);
        assert_eq!(input.value(), "9");
        input.set("");
        input.nudge(-1, 1, 128);
        assert_eq!(input.value(), "1");
    }

    #[test]
    fn unhandled_keys_fall_through_to_shortcuts() {
        let mut input = TextInput::new("x");
        assert!(!input.handle_key(key(KeyCode::Enter)));
        assert!(!input.handle_key(key(KeyCode::Tab)));
        assert!(!input.handle_key(key(KeyCode::Up)));
    }
}
