//! `TerminalState` is the one field `App` holds for Terminals, and
//! `TerminalMessage` the one message it handles. Everything else (the set, the
//! panel, the canvas caches) is inside.
//!
//! Terminals are outside every Tab (ADR 0001 doesn't apply to them). The
//! panel state is per Tab; until Tabs wire it (JRST-417) one `PanelState`
//! stands in as the implicit Tab.
//!
//! The app calls `sync` after every message. It reconciles what the engine is
//! told (visible, size, theme) with what is on screen, sending only changes.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

use iced::futures::channel::mpsc::{unbounded, UnboundedReceiver};
use iced::futures::{SinkExt, StreamExt};
use iced::keyboard::{self, key::Physical};
use iced::{event, window, Element, Size, Subscription, Task};

use super::input::{self, Route};
use super::panel::{self, PanelState, Toggle};
use super::roster::Roster;
use super::{active_term_theme, Closing, SpawnConfig, TerminalId, Terminals};
use crate::app::Message;
use crate::theme as t;
use crate::views::terminal_panel::{self, Pane, PanelView, StripItem, TermFont};

#[derive(Debug, Clone)]
pub enum TerminalMessage {
    /// The engine has a new frame.
    Wake,
    /// The window opened or resized.
    Window(Size),
    /// Cmd+J.
    ToggleFocus,
    /// A click on the pane.
    Focus,
    /// Palette: show the panel if needed and give the Terminal the keyboard.
    FocusTerminal,
    /// Palette: give the keyboard back to the app.
    FocusApp,
    /// The pointer moved (window point). Sent only while a Terminal has focus.
    Moved(f32, f32),
    /// A left press anywhere, sent only while a Terminal has focus. One
    /// outside the panel, at the last pointer position, moves focus to the app.
    PressedAt,
    /// The pointer pressed the strip.
    DragStart,
    /// The pointer moved (window y) while dragging.
    DragMove(f32),
    DragEnd,
    /// Hide the panel. Every Terminal keeps running; the keyboard goes to the app.
    Hide,
    /// "New Terminal", from the strip or an empty panel.
    New,
    /// A Terminal chosen in the strip.
    Select(TerminalId),
    /// The shown Terminal's name was clicked.
    RenameStart(TerminalId),
    RenameInput(String),
    /// Enter: an empty name is refused and the old one kept.
    RenameSubmit,
    /// The strip's close button. The app asks first when the Terminal is busy
    /// (`close_needs_confirm`), then sends `CloseConfirmed`.
    Close(TerminalId),
    CloseConfirmed(TerminalId),
    /// Start a new shell in an exited Terminal, keeping name and directory.
    Restart(TerminalId),
    /// "Folder" in the strip: choose where the team's new shells start.
    PickDir,
    /// The picker's answer; the app stores it (`db::terminal_dirs`).
    DirPicked(Option<std::path::PathBuf>),
    Key(Physical, keyboard::Modifiers, Option<String>),
    Pasted(Option<String>),
}

static WAKE_RX: OnceLock<Mutex<Option<UnboundedReceiver<()>>>> = OnceLock::new();

/// A strip drag in progress. The first pointer move after the press fixes
/// where the drag started.
struct Drag {
    start_y: Option<f32>,
    start_height: f32,
}

pub struct TerminalState {
    drag: Option<Drag>,
    pointer: Option<(f32, f32)>,
    terminals: Terminals,
    roster: Roster,
    renaming: Option<(TerminalId, String)>,
    /// Closes in flight; quit waits for them.
    closing: Vec<Closing>,
    shutting_down: bool,
    /// Each team's starting directory; unset means home.
    team_dirs: HashMap<String, std::path::PathBuf>,
    panel: PanelState,
    focused: bool,
    window: Size,
    cell: Option<(f32, f32)>,
    font: TermFont,
    pane: Pane,
    shown: Option<TerminalId>,
    sent_visible: HashMap<TerminalId, bool>,
    sent_size: HashMap<TerminalId, (u16, u16)>,
    theme_key: &'static str,
    error: Option<String>,
    /// Restored Terminals whose shell failed to start; `sync` doesn't retry
    /// them every message. Restart does.
    start_failed: Vec<TerminalId>,
}

impl TerminalState {
    pub fn new() -> Self {
        let (tx, rx) = unbounded::<()>();
        WAKE_RX.set(Mutex::new(Some(rx))).ok();
        TerminalState {
            terminals: Terminals::new(move || {
                let _ = tx.unbounded_send(());
            }),
            drag: None,
            pointer: None,
            roster: Roster::default(),
            renaming: None,
            closing: Vec::new(),
            shutting_down: false,
            team_dirs: HashMap::new(),
            panel: PanelState::default(),
            focused: false,
            window: Size::new(1100.0, 700.0),
            cell: None,
            font: TermFont::default(),
            pane: Pane::new(),
            shown: None,
            sent_visible: HashMap::new(),
            sent_size: HashMap::new(),
            theme_key: t::tok().key,
            error: None,
            start_failed: Vec::new(),
        }
    }

    /// Put the stored Terminals back in the strips at launch. No shells start:
    /// each starts when a pane first shows it.
    pub fn restore_records(&mut self, stored: Vec<crate::db::terminal_records::Stored>) {
        self.roster.restore(stored);
    }

    pub fn update(&mut self, message: TerminalMessage, team: Option<&str>) -> Task<Message> {
        match message {
            TerminalMessage::Wake => {
                // Take the wake first: a frame published after this notifies again.
                self.terminals.ack_wake();
                self.refresh_frame();
            }
            TerminalMessage::Window(size) => self.window = size,
            TerminalMessage::ToggleFocus => {
                let has = team.is_some_and(|t| !self.owned_by(t).is_empty());
                match panel::toggle(team.is_some(), self.panel.visible, has, self.focused) {
                    Toggle::Ignore => {}
                    Toggle::Show { create } => {
                        self.panel.visible = true;
                        self.focused = true;
                        if let (true, Some(team)) = (create, team) {
                            self.create(team);
                        }
                    }
                    Toggle::SetFocus(f) => self.focused = f,
                }
            }
            TerminalMessage::Focus => self.focused = true,
            TerminalMessage::FocusTerminal => {
                if !self.focused {
                    return self.update(TerminalMessage::ToggleFocus, team);
                }
            }
            TerminalMessage::FocusApp => self.focused = false,
            TerminalMessage::Moved(x, y) => self.pointer = Some((x, y)),
            TerminalMessage::PressedAt => {
                // Unknown until the pointer moves after focus was taken.
                let leaves = self.pointer.is_some_and(|(x, y)| {
                    panel::press_leaves_panel(self.window.height, self.panel.height, x, y)
                });
                if leaves {
                    self.focused = false;
                }
            }
            TerminalMessage::DragStart => {
                self.drag = Some(Drag { start_y: None, start_height: self.height() });
            }
            TerminalMessage::DragMove(y) => {
                if let Some(drag) = &mut self.drag {
                    let start_y = *drag.start_y.get_or_insert(y);
                    self.panel.height =
                        Some(panel::drag_height(self.window.height, drag.start_height, start_y, y));
                }
            }
            TerminalMessage::DragEnd => self.drag = None,
            TerminalMessage::Hide => {
                // `sync` tells the engine they're hidden (`set_visible`).
                self.panel.visible = false;
                self.focused = false;
            }
            TerminalMessage::New => {
                if let Some(team) = team {
                    self.panel.visible = true;
                    self.focused = true;
                    self.create(team);
                }
            }
            TerminalMessage::Select(id) => {
                self.renaming = None;
                if team.is_some_and(|t| self.owned_by(t).contains(&id)) {
                    self.panel.pane = Some(id);
                    self.focused = true;
                }
            }
            TerminalMessage::RenameStart(id) => {
                if let Some(r) = self.roster.get(id) {
                    self.renaming = Some((id, r.name.clone()));
                    // The text field takes the keyboard, not the shell.
                    self.focused = false;
                    return iced::widget::text_input::focus(terminal_panel::rename_input_id());
                }
            }
            TerminalMessage::RenameInput(s) => {
                if let Some((_, draft)) = self.renaming.as_mut() {
                    *draft = s;
                }
            }
            TerminalMessage::RenameSubmit => {
                if let Some((id, draft)) = self.renaming.take() {
                    self.roster.rename(id, &draft);
                    self.focused = true;
                }
            }
            TerminalMessage::Close(id) => {
                // Without the app's ask there is nothing to confirm with: an
                // idle Terminal closes at once, a busy one waits for the ask.
                if !self.close_needs_confirm(id) {
                    self.close(id);
                }
            }
            TerminalMessage::CloseConfirmed(id) => self.close(id),
            TerminalMessage::Restart(id) => self.restart(id),
            TerminalMessage::PickDir => {
                return Task::perform(
                    async {
                        rfd::AsyncFileDialog::new()
                            .set_title("Folder for new Terminals")
                            .pick_folder()
                            .await
                            .map(|h| h.path().to_path_buf())
                    },
                    |dir| Message::Terminal(TerminalMessage::DirPicked(dir)),
                );
            }
            // The app stores it first and then calls `set_team_dir`.
            TerminalMessage::DirPicked(_) => {}
            TerminalMessage::Key(Physical::Code(code), mods, text) => {
                match input::route(code, mods, text.as_deref()) {
                    Route::ToggleFocus => return self.update(TerminalMessage::ToggleFocus, team),
                    Route::Palette => return Task::done(Message::OpenCommandPalette),
                    Route::Paste => {
                        return iced::clipboard::read()
                            .map(|s| Message::Terminal(TerminalMessage::Pasted(s)))
                    }
                    // The app's own Tab switch. Focus follows `sync`: it stays
                    // with the Terminal while the new Tab shows a panel, and
                    // falls to the app on a page without one.
                    Route::Tab(i) => {
                        return Task::done(Message::Tab(crate::app::TabMessage::Focus(i)))
                    }
                    Route::Send(key) => {
                        if let Some(id) = self.shown {
                            self.terminals.send_key(id, key);
                        }
                    }
                    Route::Ignore => {}
                }
            }
            TerminalMessage::Key(..) => {}
            TerminalMessage::Pasted(Some(text)) => {
                if let Some(id) = self.shown {
                    self.terminals.paste(id, text);
                }
            }
            TerminalMessage::Pasted(None) => {}
        }
        Task::none()
    }

    /// Reconcile the engine and the canvas with the current team and layout.
    /// Cheap when nothing changed: it sends only differences.
    pub fn sync(&mut self, team: Option<&str>) {
        if team.is_none() || !self.panel.visible {
            self.focused = false;
        }
        let on_screen = team.is_some() && self.panel.visible;
        let shown = team.and_then(|t| panel::resolve_shown(self.panel.pane, &self.owned_by(t)));
        let mut shown = shown;
        // A restored Terminal starts its shell when a pane first shows it.
        if let (true, Some(id)) = (on_screen, shown) {
            if let Some(started) = self.start_restored(id) {
                shown = Some(started);
            }
        }
        if shown != self.shown {
            self.shown = shown;
            self.pane.reset();
        }
        if shown.is_some() {
            self.panel.pane = shown;
        }
        // Visibility, for every Terminal: only the one on screen builds frames.
        for id in self.terminals.ids() {
            let visible = on_screen && Some(id) == shown;
            if self.sent_visible.insert(id, visible) != Some(visible) {
                self.terminals.set_visible(id, visible);
                if visible {
                    self.refresh_frame_of(id);
                }
            }
        }
        // A pane takes its Terminal's size; a hidden Terminal keeps its last.
        if let (true, Some(id), Some(cell)) = (on_screen, shown, self.cell) {
            let grid = self.grid(cell);
            if self.sent_size.get(&id) != Some(&grid) {
                self.sent_size.insert(id, grid);
                self.terminals.resize(id, grid.0, grid.1, cell.0, cell.1);
            }
        }
        let key = t::tok().key;
        if key != self.theme_key {
            self.theme_key = key;
            let theme = active_term_theme();
            for id in self.terminals.ids() {
                self.terminals.set_theme(id, theme.clone());
            }
            self.pane.reset();
        }
        if !self.focused {
            // A position seen before focus began may be stale by the time of a press.
            self.pointer = None;
        }
        self.pane.set_focused(self.focused);
    }

    /// Apply the Terminal font from config, live. The cell is measured again;
    /// `sync` then resizes the shown Terminal's grid and PTY to it, and a
    /// hidden Terminal follows when it is next shown.
    pub fn set_font(&mut self, font: TermFont) {
        if font == self.font {
            return;
        }
        self.font = font;
        self.pane.set_font(font);
        if self.cell.is_some() {
            let cell = terminal_panel::measure_cell(font);
            self.cell = Some(cell);
            self.pane.set_cell(cell);
        }
        self.sent_size.clear();
    }

    /// Whether key presses go to the Terminal instead of the app's shortcuts.
    pub fn takes_keys(&self, modal_open: bool) -> bool {
        panel::keys_go_to_terminal(self.focused, self.panel.visible, modal_open)
    }

    /// Which focus commands the palette offers on this page.
    pub fn palette_focus(&self, team: Option<&str>) -> crate::palette_results::TerminalFocus {
        use crate::palette_results::TerminalFocus as F;
        match (team, self.focused && self.panel.visible) {
            (None, _) => F::Unavailable,
            (Some(_), true) => F::Terminal,
            (Some(_), false) => F::App,
        }
    }

    /// The panel is on screen for this page.
    pub fn panel_showing(&self, team: Option<&str>) -> bool {
        team.is_some() && self.panel.visible
    }

    /// The panel for the current team, if it is showing.
    pub fn panel_view(&self, team: Option<&str>) -> Option<Element<'_, Message>> {
        let team = team?;
        if !self.panel.visible {
            return None;
        }
        let pane = self.shown.map(|_| &self.pane);
        let items = self
            .roster
            .of_team(team)
            .map(|r| StripItem {
                id: r.id,
                name: r.name.clone(),
                active: Some(r.id) == self.shown,
                exited: r.shell && self.terminals.is_exited(r.id),
                editing: self
                    .renaming
                    .as_ref()
                    .filter(|(id, _)| *id == r.id)
                    .map(|(_, draft)| draft.clone()),
            })
            .collect();
        let note = self
            .error
            .clone()
            .or_else(|| (pane.is_none() && self.owned_by(team).is_empty()).then(|| "none open".to_string()));
        Some(terminal_panel::view(PanelView {
            pane,
            items,
            dir_label: self.team_dirs.get(team).map(|d| {
                d.file_name().map_or_else(|| d.display().to_string(), |n| n.to_string_lossy().into_owned())
            }),
            notice: self.shown.and_then(|id| {
                let dead = self.terminals.frame(id).is_some_and(|f| f.dead);
                let exited = self.roster.get(id).is_some_and(|r| r.shell) && self.terminals.is_exited(id);
                panel::pane_notice(exited, dead).map(|n| (id, n))
            }),
            note,
            height: panel::panel_height(self.window.height, self.panel.height),
        }))
    }

    pub fn subscription(&self, modal_open: bool) -> Subscription<Message> {
        let wake = Subscription::run(|| {
            iced::stream::channel(1, |mut out| async move {
                let rx = WAKE_RX.get().and_then(|m| m.lock().ok()?.take());
                if let Some(mut rx) = rx {
                    while rx.next().await.is_some() {
                        let _ = out.send(Message::Terminal(TerminalMessage::Wake)).await;
                    }
                }
                std::future::pending::<()>().await;
            })
        });
        let window = event::listen_with(|e, _status, _id| match e {
            iced::Event::Window(window::Event::Opened { size, .. })
            | iced::Event::Window(window::Event::Resized(size)) => {
                Some(Message::Terminal(TerminalMessage::Window(size)))
            }
            _ => None,
        });
        let mut subs = vec![wake, window];
        if self.drag.is_some() {
            subs.push(event::listen_with(|e, _status, _id| match e {
                iced::Event::Mouse(iced::mouse::Event::CursorMoved { position }) => {
                    Some(Message::Terminal(TerminalMessage::DragMove(position.y)))
                }
                iced::Event::Mouse(iced::mouse::Event::ButtonReleased(_)) => {
                    Some(Message::Terminal(TerminalMessage::DragEnd))
                }
                _ => None,
            }));
        }
        if self.takes_keys(modal_open) {
            // Needs the cursor position, which a press event doesn't carry.
            subs.push(event::listen_with(|e, _status, _id| match e {
                iced::Event::Mouse(iced::mouse::Event::CursorMoved { position }) => {
                    Some(Message::Terminal(TerminalMessage::Moved(position.x, position.y)))
                }
                iced::Event::Mouse(iced::mouse::Event::ButtonPressed(iced::mouse::Button::Left)) => {
                    Some(Message::Terminal(TerminalMessage::PressedAt))
                }
                _ => None,
            }));
            subs.push(event::listen_with(|e, _status, _id| match e {
                iced::Event::Keyboard(keyboard::Event::KeyPressed {
                    physical_key,
                    modifiers,
                    text,
                    ..
                }) => Some(Message::Terminal(TerminalMessage::Key(
                    physical_key,
                    modifiers,
                    text.map(|t| t.to_string()),
                ))),
                _ => None,
            }));
        }
        Subscription::batch(subs)
    }

    fn height(&self) -> f32 {
        panel::panel_height(self.window.height, self.panel.height)
    }

    fn owned_by(&self, team: &str) -> Vec<TerminalId> {
        self.roster.ids_of_team(team)
    }

    /// Strip changes since the last call, for the app to store.
    pub fn take_record_changes(&mut self) -> Vec<crate::db::terminal_records::Change> {
        self.roster.take_changes()
    }

    pub fn set_team_dirs(&mut self, dirs: HashMap<String, std::path::PathBuf>) {
        self.team_dirs = dirs;
    }

    /// New shells on `team` start in `dir`; existing shells don't move.
    pub fn set_team_dir(&mut self, team: &str, dir: Option<std::path::PathBuf>) {
        match dir {
            Some(d) => self.team_dirs.insert(team.to_string(), d),
            None => self.team_dirs.remove(team),
        };
    }

    /// Whether any Terminal, on any team and shown or hidden, is busy.
    pub fn any_busy(&self) -> bool {
        self.terminals.any_busy()
    }

    pub fn quit_plan(&self, confirmed: bool) -> panel::QuitPlan {
        panel::quit_plan(self.any_busy(), confirmed, self.shutting_down)
    }

    /// Close every Terminal for quitting, and hand back every close in flight
    /// (earlier closes included) for the caller to wait on.
    pub fn begin_shutdown(&mut self) -> Vec<Closing> {
        self.shutting_down = true;
        self.focused = false;
        let mut all = std::mem::take(&mut self.closing);
        all.extend(self.terminals.close_all());
        all
    }

    /// Closing a busy Terminal asks first; an idle or exited one doesn't.
    pub fn close_needs_confirm(&self, id: TerminalId) -> bool {
        self.terminals.is_busy(id)
    }

    /// The Terminal's name, for the ask.
    pub fn name_of(&self, id: TerminalId) -> Option<String> {
        self.roster.get(id).map(|r| r.name.clone())
    }

    /// Stop a Terminal now. It leaves the strip at once; its shell is
    /// reaped in the background.
    fn close(&mut self, id: TerminalId) {
        self.roster.remove(id);
        self.sent_visible.remove(&id);
        self.sent_size.remove(&id);
        if self.renaming.as_ref().is_some_and(|(r, _)| *r == id) {
            self.renaming = None;
        }
        if self.panel.pane == Some(id) {
            self.panel.pane = None;
        }
        if let Some(closing) = self.terminals.close(id) {
            self.closing.retain(|c| !c.is_done());
            self.closing.push(closing);
        }
        self.pane.reset();
        self.shown = None;
    }

    /// How many of the team's Terminals have a live shell.
    pub fn open_count(&self, team: &str) -> usize {
        self.terminals.open_count(team)
    }

    fn pane_exited(&self) -> bool {
        self.shown.is_some_and(|id| self.terminals.is_exited(id))
    }

    fn grid(&self, cell: (f32, f32)) -> (u16, u16) {
        let (w, h) = panel::pane_pixels(self.window.width, self.window.height, self.panel.height);
        panel::grid_for(w, h, cell.0, cell.1)
    }

    /// Start the shell of a restored record in its working directory (home if
    /// that is gone, as for any spawn). Returns the shell's id.
    fn start_restored(&mut self, id: TerminalId) -> Option<TerminalId> {
        let rec = self.roster.unstarted(id)?.clone();
        if self.start_failed.contains(&id) {
            return None;
        }
        match self.spawn(&rec.team, rec.cwd) {
            Some(new) => {
                self.roster.replace_id(id, new);
                self.panel.pane = Some(new);
                Some(new)
            }
            None => {
                self.start_failed.push(id);
                None
            }
        }
    }

    fn restart(&mut self, id: TerminalId) {
        let Some(rec) = self.roster.get(id).cloned() else { return };
        self.start_failed.retain(|f| *f != id);
        if !self.terminals.is_exited(id) {
            return;
        }
        let Some(new) = self.spawn(&rec.team, rec.cwd.clone()) else { return };
        self.roster.replace_id(id, new);
        if let Some(old) = self.terminals.close(id) {
            self.closing.retain(|c| !c.is_done());
            self.closing.push(old);
        }
        self.sent_visible.remove(&id);
        self.sent_size.remove(&id);
        self.panel.pane = Some(new);
        self.shown = None;
        self.focused = true;
    }

    /// Start a shell for `team`; shows the error in the strip on failure.
    fn spawn(&mut self, team: &str, cwd: Option<std::path::PathBuf>) -> Option<TerminalId> {
        let font = self.font;
        let cell = *self.cell.get_or_insert_with(|| terminal_panel::measure_cell(font));
        self.pane.set_cell(cell);
        let (cols, rows) = self.grid(cell);
        let config = SpawnConfig {
            team: team.to_string(),
            cols,
            rows,
            theme: active_term_theme(),
            cwd,
            program: None,
        };
        match self.terminals.spawn(config) {
            Ok(id) => {
                self.error = None;
                self.sent_size.insert(id, (cols, rows));
                Some(id)
            }
            Err(e) => {
                self.error = Some(format!("could not start a shell: {e}"));
                None
            }
        }
    }

    fn create(&mut self, team: &str) {
        let cwd = self.team_dirs.get(team).cloned();
        if let Some(id) = self.spawn(team, cwd.clone()) {
            self.roster.add(id, team, cwd);
            self.panel.pane = Some(id);
        }
    }

    fn refresh_frame(&mut self) {
        if let Some(id) = self.shown {
            self.refresh_frame_of(id);
        }
    }

    fn refresh_frame_of(&mut self, id: TerminalId) {
        if Some(id) != self.shown {
            return;
        }
        if let Some(frame) = self.terminals.frame(id) {
            self.pane.show(frame);
        }
    }
}

/// The team whose Terminals the current page can show: team and project pages
/// only. Cross-team pages have no panel (their Terminals keep running).
pub fn panel_team<'a>(
    view: &crate::app::View,
    selected_team_id: Option<&'a str>,
) -> Option<&'a str> {
    use crate::app::View;
    match view {
        View::MyIssues | View::Inbox | View::Dashboard(_) => None,
        _ => selected_team_id,
    }
}
