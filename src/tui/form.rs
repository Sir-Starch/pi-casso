//! A declarative form.
//!
//! The old wizard described its fields in five places at once — a label array
//! plus four `match` blocks keyed by a bare index (`form_value`,
//! `adjust_form_field`, `push_form_char`, `backspace_form_field`). Inserting a
//! field in the middle silently misaligned the others. Here a field is one
//! value in one list, and every behaviour follows from its kind.

use crossterm::event::{KeyCode, KeyEvent};
use ratatui::text::{Line, Span};

use crate::render::Theme;
use crate::tui::input::{NumberInput, TextInput};

#[derive(Debug)]
pub enum FieldKind {
    Text(TextInput),
    Number {
        input: NumberInput,
        min: i64,
        max: i64,
    },
    Choice {
        index: usize,
        options: Vec<String>,
    },
    Toggle(bool),
    /// A field that does something on Enter rather than holding a value.
    Submit,
    /// A non-focusable heading that groups the fields below it.
    Separator,
}

pub struct Field<Id> {
    pub id: Id,
    pub label: &'static str,
    pub kind: FieldKind,
    /// Shown under the form when this field has focus.
    pub hint: &'static str,
    /// Disabled fields are skipped by navigation and greyed out — used for
    /// settings that do not apply to the current choices, such as a canvas size
    /// outside emergence mode.
    pub enabled: bool,
}

impl<Id> Field<Id> {
    pub fn text(id: Id, label: &'static str, hint: &'static str, value: &str) -> Self {
        Self {
            id,
            label,
            kind: FieldKind::Text(TextInput::new(value)),
            hint,
            enabled: true,
        }
    }

    pub fn number(
        id: Id,
        label: &'static str,
        hint: &'static str,
        value: impl ToString,
        min: i64,
        max: i64,
    ) -> Self {
        Self {
            id,
            label,
            kind: FieldKind::Number {
                input: NumberInput::new(value),
                min,
                max,
            },
            hint,
            enabled: true,
        }
    }

    pub fn choice(
        id: Id,
        label: &'static str,
        hint: &'static str,
        options: &[&str],
        index: usize,
    ) -> Self {
        Self {
            id,
            label,
            kind: FieldKind::Choice {
                index,
                options: options.iter().map(|option| (*option).to_string()).collect(),
            },
            hint,
            enabled: true,
        }
    }

    pub fn toggle(id: Id, label: &'static str, hint: &'static str, value: bool) -> Self {
        Self {
            id,
            label,
            kind: FieldKind::Toggle(value),
            hint,
            enabled: true,
        }
    }

    pub fn submit(id: Id, label: &'static str, hint: &'static str) -> Self {
        Self {
            id,
            label,
            kind: FieldKind::Submit,
            hint,
            enabled: true,
        }
    }

    /// A heading. Never focusable, so navigation steps straight past it.
    pub fn separator(id: Id, label: &'static str) -> Self {
        Self {
            id,
            label,
            kind: FieldKind::Separator,
            hint: "",
            enabled: false,
        }
    }

    /// What the field shows in the value column.
    pub fn display(&self, focused: bool) -> String {
        match &self.kind {
            FieldKind::Text(input) => {
                if input.is_empty() && !focused {
                    "-".to_string()
                } else {
                    input.display_with_cursor(focused)
                }
            }
            FieldKind::Number { input, .. } => input.display_with_cursor(focused),
            FieldKind::Choice { index, options } => options
                .get(*index)
                .cloned()
                .unwrap_or_else(|| "-".to_string()),
            FieldKind::Toggle(value) => if *value { "yes" } else { "no" }.to_string(),
            // Submit rows render as a button and never use this text.
            FieldKind::Submit => String::new(),
            FieldKind::Separator => String::new(),
        }
    }

    pub fn text_value(&self) -> Option<&str> {
        match &self.kind {
            FieldKind::Text(input) => Some(input.value()),
            FieldKind::Number { input, .. } => Some(input.value()),
            _ => None,
        }
    }

    pub fn choice_index(&self) -> Option<usize> {
        match &self.kind {
            FieldKind::Choice { index, .. } => Some(*index),
            _ => None,
        }
    }

    pub fn toggle_value(&self) -> Option<bool> {
        match &self.kind {
            FieldKind::Toggle(value) => Some(*value),
            _ => None,
        }
    }

    pub fn set_text(&mut self, value: impl ToString) {
        match &mut self.kind {
            FieldKind::Text(input) => input.set(value.to_string()),
            FieldKind::Number { input, .. } => input.set(value.to_string()),
            _ => {}
        }
    }

    pub fn set_choice(&mut self, new_index: usize) {
        if let FieldKind::Choice { index, options } = &mut self.kind {
            if new_index < options.len() {
                *index = new_index;
            }
        }
    }

    fn step(&mut self, delta: i32) {
        match &mut self.kind {
            FieldKind::Number { input, min, max } => input.nudge(delta as i64, *min, *max),
            FieldKind::Choice { index, options } => {
                if !options.is_empty() {
                    let len = options.len() as i32;
                    *index = ((*index as i32 + delta).rem_euclid(len)) as usize;
                }
            }
            FieldKind::Toggle(value) => *value = !*value,
            _ => {}
        }
    }
}

/// What the form did with a key.
#[derive(Debug, PartialEq, Eq)]
pub enum FormOutcome {
    /// Handled internally; nothing else should see this key.
    Consumed,
    /// The user activated a submit field.
    Submit,
    /// Not ours — let the shortcut layer try.
    Ignored,
}

pub struct Form<Id> {
    pub fields: Vec<Field<Id>>,
    selected: usize,
}

impl<Id: Copy + PartialEq> Form<Id> {
    pub fn new(fields: Vec<Field<Id>>) -> Self {
        let mut form = Self {
            fields,
            selected: 0,
        };
        form.ensure_enabled(1);
        form
    }

    pub fn selected_index(&self) -> usize {
        self.selected
    }

    pub fn selected_id(&self) -> Option<Id> {
        self.fields.get(self.selected).map(|field| field.id)
    }

    /// True when the focused field consumes printable characters, which means
    /// single-letter shortcuts are unavailable while it has focus.
    pub fn focus_is_text(&self) -> bool {
        matches!(
            self.fields.get(self.selected).map(|field| &field.kind),
            Some(FieldKind::Text(_)) | Some(FieldKind::Number { .. })
        )
    }

    pub fn hint(&self) -> &'static str {
        self.fields
            .get(self.selected)
            .map(|field| field.hint)
            .unwrap_or("")
    }

    pub fn get(&self, id: Id) -> Option<&Field<Id>> {
        self.fields.iter().find(|field| field.id == id)
    }

    pub fn get_mut(&mut self, id: Id) -> Option<&mut Field<Id>> {
        self.fields.iter_mut().find(|field| field.id == id)
    }

    pub fn set_text(&mut self, id: Id, value: impl ToString) {
        if let Some(field) = self.get_mut(id) {
            field.set_text(value);
        }
    }

    pub fn set_choice(&mut self, id: Id, index: usize) {
        if let Some(field) = self.get_mut(id) {
            field.set_choice(index);
        }
    }

    pub fn set_toggle(&mut self, id: Id, value: bool) {
        if let Some(field) = self.get_mut(id) {
            if let FieldKind::Toggle(current) = &mut field.kind {
                *current = value;
            }
        }
    }

    pub fn set_enabled(&mut self, id: Id, enabled: bool) {
        if let Some(field) = self.get_mut(id) {
            field.enabled = enabled;
        }
    }

    pub fn text(&self, id: Id) -> &str {
        self.get(id)
            .and_then(|field| field.text_value())
            .unwrap_or("")
    }

    pub fn choice(&self, id: Id) -> usize {
        self.get(id)
            .and_then(|field| field.choice_index())
            .unwrap_or(0)
    }

    pub fn toggled(&self, id: Id) -> bool {
        self.get(id)
            .and_then(|field| field.toggle_value())
            .unwrap_or(false)
    }

    pub fn focus(&mut self, id: Id) {
        if let Some(index) = self.fields.iter().position(|field| field.id == id) {
            self.selected = index;
        }
    }

    /// Focuses by position, ignoring rows that cannot take focus. Returns true
    /// when focus actually moved somewhere.
    pub fn focus_index(&mut self, index: usize) -> bool {
        match self.fields.get(index) {
            Some(field) if field.enabled && !matches!(field.kind, FieldKind::Separator) => {
                self.selected = index;
                true
            }
            _ => false,
        }
    }

    /// What a click or Enter on the focused field should do. Choices and toggles
    /// advance; a submit field submits; a text field just takes focus.
    pub fn activate_selected(&mut self) -> FormOutcome {
        match self.fields.get_mut(self.selected) {
            Some(field) if matches!(field.kind, FieldKind::Submit) => FormOutcome::Submit,
            Some(field)
                if matches!(field.kind, FieldKind::Choice { .. } | FieldKind::Toggle(_)) =>
            {
                field.step(1);
                FormOutcome::Consumed
            }
            _ => FormOutcome::Consumed,
        }
    }

    fn move_focus(&mut self, delta: i32) {
        if self.fields.is_empty() {
            return;
        }
        let len = self.fields.len() as i32;
        let mut next = self.selected as i32;
        // Walk past disabled fields rather than landing on one.
        for _ in 0..len {
            next = (next + delta).rem_euclid(len);
            if self.fields[next as usize].enabled {
                self.selected = next as usize;
                return;
            }
        }
    }

    /// Keeps focus off a field that has just been disabled.
    pub fn ensure_enabled(&mut self, direction: i32) {
        if self
            .fields
            .get(self.selected)
            .map(|field| field.enabled)
            .unwrap_or(true)
        {
            return;
        }
        self.move_focus(direction);
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> FormOutcome {
        match key.code {
            KeyCode::Up => {
                self.move_focus(-1);
                return FormOutcome::Consumed;
            }
            KeyCode::Down => {
                self.move_focus(1);
                return FormOutcome::Consumed;
            }
            KeyCode::Enter => {
                let is_submit = matches!(
                    self.fields.get(self.selected).map(|field| &field.kind),
                    Some(FieldKind::Submit)
                );
                if is_submit {
                    return FormOutcome::Submit;
                }
                // Enter on a value field advances, which is what makes the form
                // usable as a top-to-bottom wizard.
                self.move_focus(1);
                return FormOutcome::Consumed;
            }
            _ => {}
        }

        let Some(field) = self.fields.get_mut(self.selected) else {
            return FormOutcome::Ignored;
        };

        // A focused text field claims printable keys before any shortcut can.
        match &mut field.kind {
            FieldKind::Text(input) => {
                if input.handle_key(key) {
                    return FormOutcome::Consumed;
                }
            }
            FieldKind::Number { input, .. } => {
                if !matches!(key.code, KeyCode::Left | KeyCode::Right) && input.handle_key(key) {
                    return FormOutcome::Consumed;
                }
            }
            _ => {}
        }

        match key.code {
            KeyCode::Left => {
                field.step(-1);
                FormOutcome::Consumed
            }
            KeyCode::Right | KeyCode::Char(' ') => {
                field.step(1);
                FormOutcome::Consumed
            }
            _ => FormOutcome::Ignored,
        }
    }

    pub fn lines(&self, theme: &Theme, label_width: usize) -> Vec<Line<'static>> {
        self.fields
            .iter()
            .enumerate()
            .map(|(index, field)| {
                if matches!(field.kind, FieldKind::Separator) {
                    let rule = if theme.unicode { "─" } else { "-" };
                    return Line::styled(
                        format!("{rule}{rule} {} ", field.label),
                        theme.dim_style(),
                    );
                }
                let focused = index == self.selected;
                // A submit row is an action, not a setting, so it is drawn as a
                // filled button. Reading it as just another label was why it
                // looked keyboard-only.
                if matches!(field.kind, FieldKind::Submit) {
                    return Line::from(vec![
                        Span::styled(
                            format!("{} ", if focused { theme.glyphs.arrow } else { " " }),
                            theme.text_style(),
                        ),
                        Span::styled(format!("  {}  ", field.label), theme.primary_button_style()),
                        Span::styled(format!("  {}", field.hint), theme.dim_style()),
                    ]);
                }
                let marker = if focused { theme.glyphs.arrow } else { " " };
                let label_style = if !field.enabled {
                    theme.dim_style()
                } else if focused {
                    theme.warning_style()
                } else {
                    theme.text_style()
                };
                let value_style = if !field.enabled {
                    theme.dim_style()
                } else {
                    theme.accent_style()
                };
                Line::from(vec![
                    Span::styled(
                        format!("{marker} {:<width$}", field.label, width = label_width),
                        label_style,
                    ),
                    Span::styled(field.display(focused), value_style),
                ])
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::KeyModifiers;

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    enum Id {
        Name,
        Size,
        Mode,
        Flag,
        Go,
    }

    fn form() -> Form<Id> {
        Form::new(vec![
            Field::text(Id::Name, "Name", "run name", ""),
            Field::number(Id::Size, "Size", "target size", 12, 1, 128),
            Field::choice(
                Id::Mode,
                "Mode",
                "match mode",
                &["emergence", "threshold"],
                0,
            ),
            Field::toggle(Id::Flag, "Invert", "allow inverted", false),
            Field::submit(Id::Go, "Start", "begin"),
        ])
    }

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    #[test]
    fn typing_into_a_text_field_beats_shortcut_letters() {
        let mut form = form();
        for ch in "hjkq".chars() {
            assert_eq!(
                form.handle_key(key(KeyCode::Char(ch))),
                FormOutcome::Consumed
            );
        }
        assert_eq!(form.text(Id::Name), "hjkq");
    }

    #[test]
    fn numbers_reject_letters_and_let_them_fall_through() {
        let mut form = form();
        form.focus(Id::Size);
        assert_eq!(
            form.handle_key(key(KeyCode::Char('x'))),
            FormOutcome::Ignored
        );
        assert_eq!(
            form.handle_key(key(KeyCode::Char('8'))),
            FormOutcome::Consumed
        );
        assert_eq!(form.text(Id::Size), "128");
    }

    #[test]
    fn arrows_step_numbers_choices_and_toggles() {
        let mut form = form();
        form.focus(Id::Size);
        form.handle_key(key(KeyCode::Right));
        assert_eq!(form.text(Id::Size), "13");
        form.focus(Id::Mode);
        form.handle_key(key(KeyCode::Right));
        assert_eq!(form.choice(Id::Mode), 1);
        form.focus(Id::Flag);
        form.handle_key(key(KeyCode::Char(' ')));
        assert!(form.toggled(Id::Flag));
    }

    #[test]
    fn enter_advances_and_submits_only_on_the_submit_field() {
        let mut form = form();
        for _ in 0..4 {
            assert_eq!(form.handle_key(key(KeyCode::Enter)), FormOutcome::Consumed);
        }
        assert_eq!(form.selected_id(), Some(Id::Go));
        assert_eq!(form.handle_key(key(KeyCode::Enter)), FormOutcome::Submit);
    }

    #[test]
    fn clicking_a_choice_advances_it_and_a_submit_submits() {
        let mut form = form();
        assert!(form.focus_index(2));
        assert_eq!(form.activate_selected(), FormOutcome::Consumed);
        assert_eq!(form.choice(Id::Mode), 1);

        assert!(form.focus_index(4));
        assert_eq!(form.activate_selected(), FormOutcome::Submit);
    }

    #[test]
    fn clicking_a_text_field_only_moves_focus() {
        let mut form = form();
        assert!(form.focus_index(0));
        assert_eq!(form.activate_selected(), FormOutcome::Consumed);
        assert_eq!(form.text(Id::Name), "");
    }

    #[test]
    fn a_click_outside_the_fields_moves_nothing() {
        let mut form = form();
        form.focus(Id::Mode);
        assert!(!form.focus_index(99));
        assert_eq!(form.selected_id(), Some(Id::Mode));
    }

    #[test]
    fn a_click_on_a_disabled_field_moves_nothing() {
        let mut form = form();
        form.set_enabled(Id::Size, false);
        assert!(!form.focus_index(1));
        assert_eq!(form.selected_id(), Some(Id::Name));
    }

    #[test]
    fn separators_are_never_focused() {
        let mut form = Form::new(vec![
            Field::text(Id::Name, "Name", "", ""),
            Field::separator(Id::Mode, "── advanced ──"),
            Field::toggle(Id::Flag, "Invert", "", false),
        ]);
        form.handle_key(key(KeyCode::Down));
        assert_eq!(form.selected_id(), Some(Id::Flag));
        form.handle_key(key(KeyCode::Up));
        assert_eq!(form.selected_id(), Some(Id::Name));
    }

    #[test]
    fn a_click_on_a_separator_selects_nothing() {
        let mut form = Form::new(vec![
            Field::text(Id::Name, "Name", "", ""),
            Field::separator(Id::Mode, "── advanced ──"),
            Field::toggle(Id::Flag, "Invert", "", false),
        ]);
        assert!(form.focus_index(0));
        assert!(!form.focus_index(1));
        assert!(form.focus_index(2));
        assert_eq!(form.selected_id(), Some(Id::Flag));
    }

    #[test]
    fn disabled_fields_are_skipped_by_navigation() {
        let mut form = form();
        form.set_enabled(Id::Size, false);
        form.set_enabled(Id::Mode, false);
        form.handle_key(key(KeyCode::Down));
        assert_eq!(form.selected_id(), Some(Id::Flag));
    }

    #[test]
    fn focus_leaves_a_field_that_becomes_disabled() {
        let mut form = form();
        form.focus(Id::Mode);
        form.set_enabled(Id::Mode, false);
        form.ensure_enabled(1);
        assert_eq!(form.selected_id(), Some(Id::Flag));
    }

    #[test]
    fn navigation_wraps_around_the_form() {
        let mut form = form();
        form.handle_key(key(KeyCode::Up));
        assert_eq!(form.selected_id(), Some(Id::Go));
    }

    #[test]
    fn text_focus_is_reported_so_callers_can_advertise_the_right_key() {
        let mut form = form();
        assert!(form.focus_is_text());
        form.focus(Id::Mode);
        assert!(!form.focus_is_text());
        form.focus(Id::Size);
        assert!(form.focus_is_text());
        form.focus(Id::Go);
        assert!(!form.focus_is_text());
    }

    #[test]
    fn unknown_keys_fall_through_to_shortcuts() {
        let mut form = form();
        form.focus(Id::Go);
        assert_eq!(form.handle_key(key(KeyCode::Esc)), FormOutcome::Ignored);
    }
}
