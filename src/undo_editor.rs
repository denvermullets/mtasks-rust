//! Undo/redo for multi-line text editors (iced 0.13's `text_editor` has none).
//!
//! [`UndoableContent`] wraps an editor's `Content` and records a snapshot
//! before each edit. Typing is grouped into word-sized steps, so one Cmd+Z
//! removes a word, not a character. [`view`] builds the editor with Cmd+Z /
//! Cmd+Shift+Z bound, and [`EditorAction`] carries those keys to `update`.

use std::ops::Deref;
use std::sync::Arc;

use iced::advanced::text::highlighter::PlainText;
use iced::keyboard::Key;
use iced::widget::text_editor::{Action, Binding, Content, Edit, KeyPress, Status, TextEditor};

/// Most undo steps kept per editor.
const MAX_STEPS: usize = 200;

/// What an editor sends `update`: an iced action, or an undo/redo key.
#[derive(Debug, Clone)]
pub enum EditorAction {
    Perform(Action),
    Undo,
    Redo,
}

impl EditorAction {
    /// Filter the iced action (e.g. drop Enter in a one-line editor). Undo and
    /// redo pass through.
    pub fn filter_action(self, f: impl FnOnce(Action) -> Option<Action>) -> Option<Self> {
        match self {
            Self::Perform(a) => f(a).map(Self::Perform),
            other => Some(other),
        }
    }
}

/// A text editor's text and cursor (a byte offset into the text).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Snapshot {
    pub text: String,
    pub cursor: usize,
}

/// How an edit groups with the one before it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EditKind {
    /// Typed a character (`true` = whitespace).
    Insert { whitespace: bool },
    Backspace,
    Delete,
    /// Its own step every time: paste, cut, Enter, typing over a selection.
    Standalone,
}

/// The undo and redo stacks. Pure: it never touches an editor.
#[derive(Debug, Default)]
pub struct History {
    undo: Vec<Snapshot>,
    redo: Vec<Snapshot>,
    /// The kind of the last recorded edit, while further edits may join it.
    open: Option<EditKind>,
}

impl History {
    /// Whether an edit of `kind` starts a new step (vs. joining the open one).
    fn starts_step(&self, kind: EditKind) -> bool {
        match (self.open, kind) {
            (_, EditKind::Standalone) | (None, _) => true,
            // Words: a letter after a space starts the next word's step.
            (Some(EditKind::Insert { whitespace: prev }), EditKind::Insert { whitespace }) => {
                prev && !whitespace
            }
            (Some(prev), kind) => prev != kind,
        }
    }

    /// Record an edit made from `before`. `changed` is false for edits that
    /// left the text alone (Backspace at the start), which record nothing.
    pub fn record(&mut self, before: Snapshot, kind: EditKind, changed: bool) {
        if !changed {
            return;
        }
        if self.starts_step(kind) {
            self.undo.push(before);
            if self.undo.len() > MAX_STEPS {
                self.undo.remove(0);
            }
        }
        self.redo.clear();
        self.open = (kind != EditKind::Standalone).then_some(kind);
    }

    /// The next edit starts a new step (the cursor moved, or a click).
    pub fn close_step(&mut self) {
        self.open = None;
    }

    /// The snapshot to restore for undo; `current` moves to the redo stack.
    pub fn undo(&mut self, current: Snapshot) -> Option<Snapshot> {
        let prev = self.undo.pop()?;
        self.redo.push(current);
        self.open = None;
        Some(prev)
    }

    /// The snapshot to restore for redo; `current` moves to the undo stack.
    pub fn redo(&mut self, current: Snapshot) -> Option<Snapshot> {
        let next = self.redo.pop()?;
        self.undo.push(current);
        self.open = None;
        Some(next)
    }
}

/// An editor `Content` that records its edits for undo. Reads go through
/// `Deref`; every change goes through [`Self::apply`] or [`Self::perform`].
#[derive(Default)]
pub struct UndoableContent {
    content: Content,
    history: History,
}

impl std::fmt::Debug for UndoableContent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("UndoableContent").field("content", &self.content).finish()
    }
}

impl Deref for UndoableContent {
    type Target = Content;
    fn deref(&self) -> &Content {
        &self.content
    }
}

impl UndoableContent {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_text(text: &str) -> Self {
        Self { content: Content::with_text(text), history: History::default() }
    }

    /// The cursor as (line, column); the column is a byte offset into the line.
    pub fn cursor_position(&self) -> (usize, usize) {
        let p = self.content.cursor().position;
        (p.line, p.column)
    }

    /// The wrapped content, for `text_editor(..)`.
    pub fn content(&self) -> &Content {
        &self.content
    }

    /// Handle an editor message.
    pub fn apply(&mut self, action: EditorAction) {
        match action {
            EditorAction::Perform(a) => self.perform(a),
            EditorAction::Undo => {
                if let Some(s) = self.history.undo(self.snapshot()) {
                    self.restore(&s);
                }
            }
            EditorAction::Redo => {
                if let Some(s) = self.history.redo(self.snapshot()) {
                    self.restore(&s);
                }
            }
        }
    }

    /// Perform an iced action, recording it if it edits the text.
    pub fn perform(&mut self, action: Action) {
        let Action::Edit(edit) = &action else {
            // Scrolling leaves the step open; anything that moves the cursor
            // or selection closes it.
            if !matches!(action, Action::Scroll { .. }) {
                self.history.close_step();
            }
            self.content.perform(action);
            return;
        };
        let kind = if self.content.selection().is_some() {
            EditKind::Standalone
        } else {
            match edit {
                Edit::Insert(c) => EditKind::Insert { whitespace: c.is_whitespace() },
                Edit::Backspace => EditKind::Backspace,
                Edit::Delete => EditKind::Delete,
                Edit::Paste(_) | Edit::Enter | Edit::Indent | Edit::Unindent => EditKind::Standalone,
            }
        };
        let before = self.snapshot();
        self.content.perform(action);
        let changed = self.text_only() != before.text;
        self.history.record(before, kind, changed);
    }

    /// Perform several actions as one undo step (inserting a suggestion is a
    /// run of Backspaces then a Paste).
    pub fn perform_as_one(&mut self, actions: impl IntoIterator<Item = Action>) {
        let before = self.snapshot();
        for a in actions {
            self.content.perform(a);
        }
        let changed = self.text_only() != before.text;
        self.history.record(before, EditKind::Standalone, changed);
    }

    /// The text without the trailing newline `Content::text` adds.
    fn text_only(&self) -> String {
        self.content.lines().map(|l| l.text.into_owned()).collect::<Vec<_>>().join("\n")
    }

    fn snapshot(&self) -> Snapshot {
        let (line, index) = self.cursor_position();
        let mut cursor = 0;
        let mut text = String::new();
        for (i, l) in self.content.lines().enumerate() {
            if i > 0 {
                text.push('\n');
            }
            if i < line {
                cursor += l.text.len() + 1;
            } else if i == line {
                cursor += index.min(l.text.len());
            }
            text.push_str(&l.text);
        }
        Snapshot { cursor: cursor.min(text.len()), text }
    }

    /// Replace the text and put the cursor back. iced 0.13 can't place a
    /// cursor directly, so: start from the text after the cursor (cursor at
    /// 0), then paste the text before it, which leaves the cursor between.
    fn restore(&mut self, s: &Snapshot) {
        let at = floor_char_boundary(&s.text, s.cursor);
        let (before, after) = s.text.split_at(at);
        self.content = Content::with_text(after);
        if !before.is_empty() {
            self.content.perform(Action::Edit(Edit::Paste(Arc::new(before.to_string()))));
        }
    }
}

fn floor_char_boundary(s: &str, mut i: usize) -> usize {
    while i > 0 && !s.is_char_boundary(i) {
        i -= 1;
    }
    i
}

/// Undo/redo for a key press: Cmd+Z and Cmd+Shift+Z.
pub fn undo_key(press: &KeyPress) -> Option<EditorAction> {
    if !matches!(press.status, Status::Focused { .. }) || !press.modifiers.command() {
        return None;
    }
    match press.key.as_ref() {
        Key::Character(c) if c.eq_ignore_ascii_case("z") => Some(if press.modifiers.shift() {
            EditorAction::Redo
        } else {
            EditorAction::Undo
        }),
        _ => None,
    }
}

/// Wrap an editor's key bindings so Cmd+Z / Cmd+Shift+Z undo and redo.
pub fn with_undo<M: Clone>(
    inner: impl Fn(KeyPress) -> Option<Binding<M>>,
    on_change: fn(EditorAction) -> M,
) -> impl Fn(KeyPress) -> Option<Binding<M>> {
    move |press| match undo_key(&press) {
        Some(a) => Some(Binding::Custom(on_change(a))),
        None => inner(press),
    }
}

/// A `text_editor` over `content` that sends every action (and undo/redo)
/// as `on_change`. `binding` is the editor's other key bindings
/// (`Binding::from_key_press` for the defaults).
pub fn view<'a, M: Clone + 'a>(
    content: &'a UndoableContent,
    on_change: fn(EditorAction) -> M,
    binding: impl Fn(KeyPress) -> Option<Binding<M>> + 'a,
) -> TextEditor<'a, PlainText, M> {
    iced::widget::text_editor(content.content())
        .on_action(move |a| on_change(EditorAction::Perform(a)))
        .key_binding(with_undo(binding, on_change))
}

#[cfg(test)]
mod tests {
    use super::*;
    use iced::keyboard::Modifiers;
    use iced::widget::text_editor::Motion;

    fn typed(c: &mut UndoableContent, s: &str) {
        for ch in s.chars() {
            let edit = if ch == '\n' { Edit::Enter } else { Edit::Insert(ch) };
            c.perform(Action::Edit(edit));
        }
    }

    fn text(c: &UndoableContent) -> String {
        c.text_only()
    }

    #[test]
    fn undo_removes_a_word_at_a_time_and_redo_puts_it_back() {
        let mut c = UndoableContent::new();
        typed(&mut c, "hello world");
        c.apply(EditorAction::Undo);
        assert_eq!(text(&c), "hello ");
        c.apply(EditorAction::Undo);
        assert_eq!(text(&c), "");
        c.apply(EditorAction::Undo); // nothing left: no-op
        assert_eq!(text(&c), "");
        c.apply(EditorAction::Redo);
        assert_eq!(text(&c), "hello ");
        c.apply(EditorAction::Redo);
        assert_eq!(text(&c), "hello world");
    }

    #[test]
    fn undo_restores_the_cursor() {
        let mut c = UndoableContent::with_text("ab\ncd");
        c.perform(Action::Move(Motion::DocumentEnd));
        c.perform(Action::Move(Motion::Left)); // between c and d
        typed(&mut c, "X");
        assert_eq!(text(&c), "ab\ncXd");
        c.apply(EditorAction::Undo);
        assert_eq!(text(&c), "ab\ncd");
        assert_eq!(c.cursor_position(), (1, 1));
        // Typing after the undo lands where the cursor was.
        typed(&mut c, "Y");
        assert_eq!(text(&c), "ab\ncYd");
    }

    #[test]
    fn a_new_edit_clears_redo() {
        let mut c = UndoableContent::new();
        typed(&mut c, "one");
        c.apply(EditorAction::Undo);
        typed(&mut c, "two");
        c.apply(EditorAction::Redo);
        assert_eq!(text(&c), "two");
    }

    #[test]
    fn moving_the_cursor_splits_typing_into_steps() {
        let mut c = UndoableContent::new();
        typed(&mut c, "ab");
        c.perform(Action::Move(Motion::Left));
        typed(&mut c, "cd");
        assert_eq!(text(&c), "acdb");
        c.apply(EditorAction::Undo);
        assert_eq!(text(&c), "ab");
    }

    #[test]
    fn paste_and_enter_are_their_own_steps() {
        let mut c = UndoableContent::new();
        typed(&mut c, "a");
        c.perform(Action::Edit(Edit::Paste(Arc::new("PASTED".into()))));
        typed(&mut c, "\nb");
        c.apply(EditorAction::Undo);
        assert_eq!(text(&c), "aPASTED\n");
        c.apply(EditorAction::Undo);
        assert_eq!(text(&c), "aPASTED");
        c.apply(EditorAction::Undo);
        assert_eq!(text(&c), "a");
    }

    #[test]
    fn backspaces_group_and_no_op_edits_record_nothing() {
        let mut c = UndoableContent::new();
        c.perform(Action::Edit(Edit::Backspace)); // empty: nothing to undo
        typed(&mut c, "abc");
        c.perform(Action::Edit(Edit::Backspace));
        c.perform(Action::Edit(Edit::Backspace));
        assert_eq!(text(&c), "a");
        c.apply(EditorAction::Undo);
        assert_eq!(text(&c), "abc");
        c.apply(EditorAction::Undo);
        assert_eq!(text(&c), "");
    }

    #[test]
    fn perform_as_one_is_a_single_step() {
        let mut c = UndoableContent::new();
        typed(&mut c, "@jo");
        c.perform_as_one([
            Action::Edit(Edit::Backspace),
            Action::Edit(Edit::Backspace),
            Action::Edit(Edit::Paste(Arc::new("john ".into()))),
        ]);
        assert_eq!(text(&c), "@john ");
        c.apply(EditorAction::Undo);
        assert_eq!(text(&c), "@jo");
    }

    #[test]
    fn history_caps_its_steps() {
        let mut h = History::default();
        for i in 0..MAX_STEPS + 10 {
            let s = Snapshot { text: i.to_string(), cursor: 0 };
            h.record(s, EditKind::Standalone, true);
        }
        assert_eq!(h.undo.len(), MAX_STEPS);
        assert_eq!(h.undo[0].text, "10");
    }

    fn press(key: &str, modifiers: Modifiers, status: Status) -> KeyPress {
        KeyPress {
            key: Key::Character(key.into()),
            modified_key: Key::Character(key.into()),
            physical_key: iced::keyboard::key::Physical::Unidentified(iced::keyboard::key::NativeCode::Unidentified),
            modifiers,
            text: None,
            status,
        }
    }

    #[test]
    fn cmd_z_undoes_and_cmd_shift_z_redoes_only_when_focused() {
        let cmd = Modifiers::COMMAND;
        let cmd_shift = Modifiers::COMMAND | Modifiers::SHIFT;
        assert!(matches!(undo_key(&press("z", cmd, Status::Focused { is_hovered: false })), Some(EditorAction::Undo)));
        assert!(matches!(undo_key(&press("Z", cmd_shift, Status::Focused { is_hovered: false })), Some(EditorAction::Redo)));
        assert!(matches!(undo_key(&press("z", cmd_shift, Status::Focused { is_hovered: false })), Some(EditorAction::Redo)));
        assert!(undo_key(&press("z", Modifiers::empty(), Status::Focused { is_hovered: false })).is_none());
        assert!(undo_key(&press("z", cmd, Status::Active)).is_none());
        assert!(undo_key(&press("c", cmd, Status::Focused { is_hovered: false })).is_none());
    }
}
