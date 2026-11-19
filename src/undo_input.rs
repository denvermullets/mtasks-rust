//! Undo/redo for single-line text inputs (iced 0.13's `text_input` has none).
//!
//! Build an input with [`text_input`] instead of `iced::widget::text_input`.
//! It gives the input an `Id` (its key) and wraps every edit in
//! [`Message::TextInputEdited`], which `App::update` passes to
//! [`Fields::record`] before handling the field's own message.
//!
//! A focused `text_input` captures Cmd+Z without acting on it. The key
//! subscription turns a captured Cmd+Z / Cmd+Shift+Z into
//! [`Message::TextUndoKey`]. [`focused`] then asks the widget tree which
//! input has focus, and [`Fields::undo`] restores that field and only that
//! one. A focused multi-line editor has no key here, so it is left to
//! `undo_editor`.
//!
//! The history itself is `undo_editor::History`: typing groups into word
//! steps, and paste or typing over a selection is its own step.
//!
//!
//! The input is an [`UndoInput`], which wraps iced's and also gives it
//! macOS Cmd+Delete: delete from the cursor back to the start of the line
//! (or just the selection), one undo step.

use std::borrow::Cow;
use std::cell::RefCell;
use std::collections::HashMap;
use std::fmt;
use std::sync::Arc;

use iced::advanced::layout::{self, Layout};
use iced::advanced::renderer;
use iced::advanced::widget::operation::{Focusable, Operation, Outcome};
use iced::advanced::widget::{tree, Id, Tree, Widget};
use iced::advanced::{overlay, text, Clipboard, Shell};
use iced::event::{self, Event};
use iced::keyboard::{self, key::Named, Key};
use iced::widget::text_input::{cursor, Status, Style, Value};
use iced::widget::{self, TextInput};
use iced::{mouse, Element, Font, Length, Padding, Pixels, Rectangle, Renderer, Size, Task, Theme, Vector};

use crate::app::Message;
use crate::undo_editor::{EditKind, History, Snapshot};

/// Builds a field's own message from its new text (the input's `on_input`).
pub struct Setter<M>(Arc<dyn Fn(String) -> M + Send + Sync>);

impl<M> Clone for Setter<M> {
    fn clone(&self) -> Self {
        Self(Arc::clone(&self.0))
    }
}

impl<M> Setter<M> {
    pub fn new(f: impl Fn(String) -> M + Send + Sync + 'static) -> Self {
        Self(Arc::new(f))
    }

    fn call(&self, text: String) -> M {
        (self.0)(text)
    }
}

impl<M> fmt::Debug for Setter<M> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Setter")
    }
}

/// An undo key the focused input captured but doesn't act on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyAction {
    Undo,
    Redo,
}

/// The action for a key the focused input captured, if it is one of ours:
/// Cmd+Z or Cmd+Shift+Z.
pub fn key_action(key: &Key, modifiers: keyboard::Modifiers) -> Option<KeyAction> {
    match key {
        Key::Character(c) if modifiers.command() && c.eq_ignore_ascii_case("z") => {
            Some(if modifiers.shift() { KeyAction::Redo } else { KeyAction::Undo })
        }
        _ => None,
    }
}

/// One edit to a field: its text before and after.
#[derive(Debug, Clone)]
pub struct Edit<M> {
    pub key: Cow<'static, str>,
    pub before: String,
    pub after: String,
    pub set: Setter<M>,
}

struct Field<M> {
    history: History,
    /// The text as of the last edit or undo, and where the cursor is in it.
    current: Snapshot,
    set: Setter<M>,
}

/// Every field's undo history, by key.
pub struct Fields<M> {
    fields: HashMap<String, Field<M>>,
}

impl<M> Default for Fields<M> {
    fn default() -> Self {
        Self { fields: HashMap::new() }
    }
}

impl<M> fmt::Debug for Fields<M> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Fields").field("keys", &self.fields.keys().collect::<Vec<_>>()).finish()
    }
}

impl<M> Fields<M> {
    /// Record an edit and return the field's own message for it.
    pub fn record(&mut self, edit: Edit<M>) -> M {
        let Edit { key, before, after, set } = edit;
        let field = self.fields.entry(key.into_owned()).or_insert_with(|| Field {
            history: History::default(),
            current: Snapshot { cursor: before.len(), text: before.clone() },
            set: set.clone(),
        });
        // The text changed outside the input (a form reset, another issue
        // loaded): the old steps no longer apply.
        if field.current.text != before {
            field.history = History::default();
            field.current = Snapshot { cursor: before.len(), text: before.clone() };
        }
        let d = diff(&before, &after);
        // Typing somewhere else than where the last edit left the cursor.
        if d.before_cursor != field.current.cursor {
            field.history.close_step();
        }
        let changed = before != after;
        field.history.record(Snapshot { text: before, cursor: d.before_cursor }, d.kind, changed);
        field.current = Snapshot { text: after.clone(), cursor: d.after_cursor };
        field.set = set;
        field.set.call(after)
    }

    /// Undo (or redo) the field `key`, whose input shows `shown`. Returns the
    /// field's message carrying the restored text, and the cursor (a byte
    /// offset) to put back. Nothing happens when the field has no step to
    /// undo, or when its text changed since the last edit.
    pub fn undo(&mut self, key: &str, redo: bool, shown: Option<&str>) -> Option<(M, Snapshot)> {
        let field = self.fields.get_mut(key)?;
        if shown != Some(field.current.text.as_str()) {
            self.fields.remove(key);
            return None;
        }
        let current = field.current.clone();
        let s = if redo { field.history.redo(current) } else { field.history.undo(current) }?;
        field.current = s.clone();
        Some((field.set.call(s.text.clone()), s))
    }

    /// The keys of every field with a history.
    pub fn keys(&self) -> Vec<String> {
        self.fields.keys().cloned().collect()
    }
}

/// What an edit did, read from the text before and after it.
#[derive(Debug, PartialEq, Eq)]
struct Diff {
    kind: EditKind,
    /// The cursor before the edit: the end of the replaced text.
    before_cursor: usize,
    /// The cursor after the edit: the end of the inserted text.
    after_cursor: usize,
}

fn diff(before: &str, after: &str) -> Diff {
    let prefix: usize = before
        .chars()
        .zip(after.chars())
        .take_while(|(a, b)| a == b)
        .map(|(a, _)| a.len_utf8())
        .sum();
    let room = before.len().min(after.len()) - prefix;
    let suffix: usize = before[prefix..]
        .chars()
        .rev()
        .zip(after[prefix..].chars().rev())
        .take_while(|(a, b)| a == b)
        .map(|(a, _)| a.len_utf8())
        .scan(0, |n, len| {
            *n += len;
            (*n <= room).then_some(len)
        })
        .sum();
    let removed = &before[prefix..before.len() - suffix];
    let inserted = &after[prefix..after.len() - suffix];
    let one_char = |s: &str| s.chars().count() == 1;
    let kind = match (removed.is_empty(), inserted.is_empty()) {
        (true, false) if one_char(inserted) => {
            EditKind::Insert { whitespace: inserted.chars().all(char::is_whitespace) }
        }
        (false, true) if one_char(removed) => EditKind::Backspace,
        _ => EditKind::Standalone,
    };
    Diff { kind, before_cursor: before.len() - suffix, after_cursor: after.len() - suffix }
}

thread_local! {
    /// The text each keyed input showed in the last `view`. Undo checks it so
    /// it never restores a field whose text was changed outside the input.
    static SHOWN: RefCell<HashMap<String, String>> = RefCell::new(HashMap::new());
}

fn note_shown(key: &str, value: &str) {
    SHOWN.with(|m| {
        let mut m = m.borrow_mut();
        if m.get(key).map(String::as_str) != Some(value) {
            m.insert(key.to_string(), value.to_string());
        }
    });
}

/// The text the input `key` showed in the last `view`.
pub fn shown(key: &str) -> Option<String> {
    SHOWN.with(|m| m.borrow().get(key).cloned())
}

/// A `text_input` with undo/redo and Cmd+Delete. `key` names the field (and
/// is its `Id`, so `text_input::focus` and friends can target it with
/// [`id`]); give fields that belong to one record a key that includes the
/// record's id.
pub fn text_input<'a>(
    key: impl Into<Cow<'static, str>>,
    placeholder: &str,
    value: &str,
    on_input: impl Fn(String) -> Message + Send + Sync + 'static,
) -> UndoInput<'a> {
    let key = key.into();
    note_shown(&key, value);
    let before = value.to_string();
    let set = Setter::new(on_input);
    let input_id = id(key.clone());
    let edit = move |after| {
        Message::TextInputEdited(Edit {
            key: key.clone(),
            before: before.clone(),
            after,
            set: set.clone(),
        })
    };
    let input = widget::text_input(placeholder, value).id(input_id).on_input(edit.clone());
    UndoInput { input, value: value.to_string(), on_input: Some(Box::new(edit)) }
}

/// What Cmd+Delete leaves: the selection removed, or else everything from
/// the start of the line to the cursor. Returns the new text and where the
/// cursor goes (in graphemes); `None` when there is nothing to delete.
fn delete_to_line_start(value: &Value, cursor: cursor::State) -> Option<(String, usize)> {
    let (start, end) = match cursor {
        cursor::State::Index(i) => (0, i),
        cursor::State::Selection { start, end } => (start.min(end), start.max(end)),
    };
    if start >= end {
        return None;
    }
    let mut rest = value.clone();
    rest.remove_many(start, end.min(value.len()));
    Some((rest.to_string(), start))
}

type Paragraph = <Renderer as text::Renderer>::Paragraph;

/// An iced `text_input` that also handles Cmd+Delete like macOS. Built by
/// [`text_input`]; takes the same builder calls the app uses.
pub struct UndoInput<'a> {
    input: TextInput<'a, Message>,
    value: String,
    /// The input's `on_input`; `None` when it is read-only.
    on_input: Option<Box<dyn Fn(String) -> Message + 'a>>,
}

impl<'a> From<TextInput<'a, Message>> for UndoInput<'a> {
    /// A read-only input (no undo, no Cmd+Delete), so both can share code.
    fn from(input: TextInput<'a, Message>) -> Self {
        Self { input, value: String::new(), on_input: None }
    }
}

impl<'a> UndoInput<'a> {
    pub fn padding(self, padding: impl Into<Padding>) -> Self {
        Self { input: self.input.padding(padding), ..self }
    }

    pub fn size(self, size: impl Into<Pixels>) -> Self {
        Self { input: self.input.size(size), ..self }
    }

    pub fn width(self, width: impl Into<Length>) -> Self {
        Self { input: self.input.width(width), ..self }
    }

    pub fn font(self, font: Font) -> Self {
        Self { input: self.input.font(font), ..self }
    }

    pub fn secure(self, secure: bool) -> Self {
        Self { input: self.input.secure(secure), ..self }
    }

    pub fn on_submit(self, message: Message) -> Self {
        Self { input: self.input.on_submit(message), ..self }
    }

    pub fn style(self, style: impl Fn(&Theme, Status) -> Style + 'a) -> Self {
        Self { input: self.input.style(style), ..self }
    }
}

type Inner<'a> = TextInput<'a, Message>;

impl<'a> Widget<Message, Theme, Renderer> for UndoInput<'a> {
    fn size(&self) -> Size<Length> {
        <Inner as Widget<Message, Theme, Renderer>>::size(&self.input)
    }

    fn layout(&self, tree: &mut Tree, renderer: &Renderer, limits: &layout::Limits) -> layout::Node {
        <Inner as Widget<Message, Theme, Renderer>>::layout(&self.input, tree, renderer, limits)
    }

    fn draw(
        &self,
        tree: &Tree,
        renderer: &mut Renderer,
        theme: &Theme,
        style: &renderer::Style,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
    ) {
        <Inner as Widget<Message, Theme, Renderer>>::draw(
            &self.input, tree, renderer, theme, style, layout, cursor, viewport,
        );
    }

    fn tag(&self) -> tree::Tag {
        <Inner as Widget<Message, Theme, Renderer>>::tag(&self.input)
    }

    fn state(&self) -> tree::State {
        <Inner as Widget<Message, Theme, Renderer>>::state(&self.input)
    }

    fn children(&self) -> Vec<Tree> {
        <Inner as Widget<Message, Theme, Renderer>>::children(&self.input)
    }

    fn diff(&self, tree: &mut Tree) {
        <Inner as Widget<Message, Theme, Renderer>>::diff(&self.input, tree);
    }

    fn operate(
        &self,
        tree: &mut Tree,
        layout: Layout<'_>,
        renderer: &Renderer,
        operation: &mut dyn Operation,
    ) {
        <Inner as Widget<Message, Theme, Renderer>>::operate(&self.input, tree, layout, renderer, operation);
    }

    fn on_event(
        &mut self,
        tree: &mut Tree,
        event: Event,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        renderer: &Renderer,
        clipboard: &mut dyn Clipboard,
        shell: &mut Shell<'_, Message>,
        viewport: &Rectangle,
    ) -> event::Status {
        // iced runs Cmd+Delete as a plain Backspace; do the macOS delete.
        if let (
            Event::Keyboard(keyboard::Event::KeyPressed { key: Key::Named(Named::Backspace), modifiers, .. }),
            Some(on_input),
        ) = (&event, &self.on_input)
        {
            let state = tree.state.downcast_mut::<widget::text_input::State<Paragraph>>();
            if modifiers.command() && state.is_focused() {
                let value = Value::new(&self.value);
                if let Some((rest, at)) = delete_to_line_start(&value, state.cursor().state(&value)) {
                    state.move_cursor_to(at);
                    shell.publish(on_input(rest));
                }
                return event::Status::Captured;
            }
        }
        <Inner as Widget<Message, Theme, Renderer>>::on_event(
            &mut self.input, tree, event, layout, cursor, renderer, clipboard, shell, viewport,
        )
    }

    fn mouse_interaction(
        &self,
        tree: &Tree,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
        renderer: &Renderer,
    ) -> mouse::Interaction {
        <Inner as Widget<Message, Theme, Renderer>>::mouse_interaction(
            &self.input, tree, layout, cursor, viewport, renderer,
        )
    }

    fn overlay<'b>(
        &'b mut self,
        tree: &'b mut Tree,
        layout: Layout<'_>,
        renderer: &Renderer,
        translation: Vector,
    ) -> Option<overlay::Element<'b, Message, Theme, Renderer>> {
        <Inner as Widget<Message, Theme, Renderer>>::overlay(&mut self.input, tree, layout, renderer, translation)
    }
}

impl<'a> From<UndoInput<'a>> for Element<'a, Message> {
    fn from(input: UndoInput<'a>) -> Self {
        Element::new(input)
    }
}

/// The `Id` of the input built with `key`.
pub fn id(key: impl Into<Cow<'static, str>>) -> widget::text_input::Id {
    widget::text_input::Id::new(key)
}

/// Find which of the inputs `keys` has focus. Produces `None` when none does
/// (a multi-line editor, or nothing, is focused).
pub fn focused(keys: Vec<String>) -> Task<Option<String>> {
    let candidates = keys.into_iter().map(|k| (Id::new(k.clone()), k)).collect();
    iced::advanced::widget::operate(FindFocused { candidates, found: None })
}

struct FindFocused {
    candidates: Vec<(Id, String)>,
    found: Option<String>,
}

impl Operation<Option<String>> for FindFocused {
    fn container(
        &mut self,
        _id: Option<&Id>,
        _bounds: Rectangle,
        operate_on_children: &mut dyn FnMut(&mut dyn Operation<Option<String>>),
    ) {
        operate_on_children(self);
    }

    fn focusable(&mut self, state: &mut dyn Focusable, id: Option<&Id>) {
        if !state.is_focused() {
            return;
        }
        if let Some(id) = id {
            if let Some((_, key)) = self.candidates.iter().find(|(c, _)| c == id) {
                self.found = Some(key.clone());
            }
        }
    }

    fn finish(&self) -> Outcome<Option<String>> {
        Outcome::Some(self.found.clone())
    }
}

/// Put the cursor of input `key` at byte offset `cursor` in `text`.
pub fn move_cursor<T>(key: String, text: &str, cursor: usize) -> Task<T> {
    let mut at = cursor.min(text.len());
    while !text.is_char_boundary(at) {
        at -= 1;
    }
    // The input counts its cursor in characters (graphemes, strictly).
    widget::text_input::move_cursor_to(id(key), text[..at].chars().count())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, Clone, PartialEq)]
    enum Msg {
        Title(String),
        Name(String),
    }

    struct Form {
        fields: Fields<Msg>,
        title: String,
        name: String,
    }

    impl Form {
        fn new() -> Self {
            Self { fields: Fields::default(), title: String::new(), name: String::new() }
        }

        fn apply(&mut self, m: Msg) {
            match m {
                Msg::Title(s) => self.title = s,
                Msg::Name(s) => self.name = s,
            }
        }

        /// What the input does: one edit per keystroke, from the shown text.
        fn edit(&mut self, key: &'static str, after: &str) {
            let (before, set) = match key {
                "title" => (self.title.clone(), Setter::new(Msg::Title)),
                _ => (self.name.clone(), Setter::new(Msg::Name)),
            };
            let m = self.fields.record(Edit { key: key.into(), before, after: after.into(), set });
            self.apply(m);
        }

        fn type_into(&mut self, key: &'static str, s: &str) {
            for c in s.chars() {
                let mut t = if key == "title" { self.title.clone() } else { self.name.clone() };
                t.push(c);
                self.edit(key, &t);
            }
        }

        fn shown(&self, key: &str) -> String {
            if key == "title" { self.title.clone() } else { self.name.clone() }
        }

        fn undo(&mut self, key: &str, redo: bool) -> Option<usize> {
            let shown = self.shown(key);
            let (m, s) = self.fields.undo(key, redo, Some(&shown))?;
            self.apply(m);
            Some(s.cursor)
        }
    }

    #[test]
    fn undo_removes_a_word_and_redo_puts_it_back() {
        let mut f = Form::new();
        f.type_into("title", "hello world");
        assert_eq!(f.undo("title", false), Some(6));
        assert_eq!(f.title, "hello ");
        f.undo("title", false);
        assert_eq!(f.title, "");
        assert_eq!(f.undo("title", false), None);
        f.undo("title", true);
        assert_eq!(f.title, "hello ");
        f.undo("title", true);
        assert_eq!(f.title, "hello world");
    }

    #[test]
    fn paste_is_its_own_step() {
        let mut f = Form::new();
        f.type_into("title", "ab");
        f.edit("title", "abPASTED");
        f.type_into("title", "c");
        f.undo("title", false);
        assert_eq!(f.title, "abPASTED");
        f.undo("title", false);
        assert_eq!(f.title, "ab");
    }

    #[test]
    fn undo_only_touches_its_own_field() {
        let mut f = Form::new();
        f.type_into("title", "t");
        f.type_into("name", "n");
        f.undo("title", false);
        assert_eq!((f.title.as_str(), f.name.as_str()), ("", "n"));
    }

    #[test]
    fn undo_with_nothing_edited_does_nothing() {
        let mut f = Form::new();
        assert_eq!(f.undo("title", false), None);
        assert_eq!(f.undo("title", true), None);
        assert_eq!(f.title, "");
    }

    #[test]
    fn an_outside_change_drops_the_history() {
        let mut f = Form::new();
        f.type_into("title", "old");
        f.title.clear(); // the form was reset
        assert_eq!(f.undo("title", false), None);
        assert_eq!(f.title, "");
        // A fresh edit after an outside change starts a fresh history.
        f.title = "loaded".into();
        f.type_into("title", "!");
        f.undo("title", false);
        assert_eq!(f.title, "loaded");
        assert_eq!(f.undo("title", false), None);
    }

    #[test]
    fn backspaces_group_and_restore_the_cursor() {
        let mut f = Form::new();
        f.type_into("title", "abc");
        f.edit("title", "ab");
        f.edit("title", "a");
        assert_eq!(f.undo("title", false), Some(3));
        assert_eq!(f.title, "abc");
    }

    #[test]
    fn typing_elsewhere_starts_a_new_step() {
        let mut f = Form::new();
        f.type_into("title", "ac");
        f.edit("title", "abc"); // clicked between a and c, typed b
        f.undo("title", false);
        assert_eq!(f.title, "ac");
    }

    #[test]
    fn cmd_delete_deletes_back_to_the_start_of_the_line() {
        use cursor::State::{Index, Selection};
        let v = Value::new("hello world");
        assert_eq!(delete_to_line_start(&v, Index(6)), Some(("world".into(), 0)));
        assert_eq!(delete_to_line_start(&v, Index(11)), Some(("".into(), 0)));
        assert_eq!(delete_to_line_start(&v, Index(0)), None);
        // A selection: just the selection, either direction.
        assert_eq!(delete_to_line_start(&v, Selection { start: 2, end: 5 }), Some(("he world".into(), 2)));
        assert_eq!(delete_to_line_start(&v, Selection { start: 5, end: 2 }), Some(("he world".into(), 2)));
        // Graphemes, not bytes.
        let v = Value::new("héllo");
        assert_eq!(delete_to_line_start(&v, Index(2)), Some(("llo".into(), 0)));
    }

    #[test]
    fn cmd_delete_is_one_undo_step_that_puts_the_cursor_back() {
        let mut f = Form::new();
        f.type_into("title", "hello world");
        f.edit("title", "world"); // Cmd+Delete with the cursor after "hello "
        assert_eq!(f.undo("title", false), Some(6));
        assert_eq!(f.title, "hello world");
    }

    #[test]
    fn captured_keys_map_to_actions() {
        use iced::keyboard::Modifiers;
        let z = Key::Character("z".into());
        let cmd = Modifiers::COMMAND;
        assert_eq!(key_action(&z, cmd), Some(KeyAction::Undo));
        assert_eq!(key_action(&z, cmd | Modifiers::SHIFT), Some(KeyAction::Redo));
        assert_eq!(key_action(&z, Modifiers::empty()), None);
        assert_eq!(key_action(&Key::Character("c".into()), cmd), None);
    }

    #[test]
    fn diff_reads_the_edit() {
        let d = diff("ab", "abc");
        assert_eq!(d, Diff { kind: EditKind::Insert { whitespace: false }, before_cursor: 2, after_cursor: 3 });
        let d = diff("aa", "aaa"); // ambiguous repeat: still one insert
        assert_eq!(d.kind, EditKind::Insert { whitespace: false });
        assert_eq!(diff("ab", "a b").kind, EditKind::Insert { whitespace: true });
        let d = diff("abc", "ac");
        assert_eq!(d, Diff { kind: EditKind::Backspace, before_cursor: 2, after_cursor: 1 });
        assert_eq!(diff("abc", "aXc").kind, EditKind::Standalone);
        assert_eq!(diff("", "pasted").kind, EditKind::Standalone);
        assert_eq!(diff("héllo", "hélo").kind, EditKind::Backspace);
    }
}
