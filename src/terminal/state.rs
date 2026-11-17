//! `TerminalState` is the one field `App` holds for Terminals, and
//! `TerminalMessage` the one message it handles. Everything else (the set, the
//! panel, the canvas caches) is inside.
//!
//! Terminals are outside every Tab (ADR 0001 doesn't apply to them). The
//! panel state is per Tab: `panel` is the active Tab's, and the app parks and
//! restores it on a switch with `stored_panel` and `restore_panel`.
//!
//! Which strip the panel shows is a `Scope`: the team's own Terminals, or a
//! project's (ADR 0003). The app passes it in with every call.
//!
//! The app calls `sync` after every message. It reconciles what the engine is
//! told (visible, size, theme) with what is on screen, sending only changes.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

use iced::futures::channel::mpsc::{unbounded, UnboundedReceiver};
use iced::futures::{SinkExt, StreamExt};
use iced::keyboard::{self, key::Physical};
use iced::{event, window, Element, Size, Subscription, Task};

use super::attention::{self, Alert, Need};
use super::input::{self, Route};
use super::links::Link;
use super::panel::{self, Dock, Layout, PanelState, Toggle, MAX_PANES};
use super::roster::Roster;
use crate::db::panel_states::StoredPanel;
use super::{
    active_term_theme, Closing, MouseAction, MouseInput, Scope, SpawnConfig, TerminalId, Terminals,
    WheelInput,
};
use crate::app::Message;
use crate::theme as t;
use crate::views::terminal_panel::{self, Pane, PaneView, PanelView, StripItem, TermFont};

#[derive(Debug, Clone)]
pub enum TerminalMessage {
    /// The engine has a new frame.
    Wake,
    /// The window opened or resized.
    Window(Size),
    /// Cmd+J.
    ToggleFocus,
    /// A click on the pane in this slot.
    Focus(usize),
    /// A pointer event over the pane in this slot. A press also focuses it.
    Mouse(usize, MouseInput),
    /// Wheel or trackpad movement over the pane in this slot.
    Wheel(usize, WheelInput),
    /// The link under Cmd+hover in this slot changed (or Cmd was let go).
    Hover(usize, Option<Link>),
    /// Cmd+click on a link.
    OpenLink { url: String, explicit: bool },
    /// Palette: show the panel if needed and give the Terminal the keyboard.
    FocusTerminal,
    /// Palette: give the keyboard back to the app.
    FocusApp,
    /// The pointer moved (window point). Sent only while a Terminal has focus.
    Moved(f32, f32),
    /// A left press anywhere, sent only while a Terminal has focus. One
    /// outside the panel, at the last pointer position, moves focus to the app.
    PressedAt,
    /// The pointer pressed the strip (docked at the bottom) or the left edge
    /// (docked right).
    DragStart,
    /// The pointer moved (window x, y) while dragging.
    DragMove(f32, f32),
    /// The right-docked panel's body (every pane, strip excluded) was laid
    /// out at this size.
    BodySize(Size),
    DragEnd,
    /// Hide the panel. Every Terminal keeps running; the keyboard goes to the app.
    Hide,
    /// "New Terminal", from the strip or an empty panel.
    New,
    /// "New Terminal" offered by the empty pane in this slot.
    NewIn(usize),
    /// A Terminal chosen in the strip: it goes in the focused pane.
    Select(TerminalId),
    /// A Terminal chosen in an empty pane. One already in another pane moves.
    Pick(usize, TerminalId),
    /// A layout chosen in the strip.
    SetLayout(Layout),
    /// Focus the next or previous pane (Cmd+] and Cmd+[).
    CyclePane(i32),
    /// The shown Terminal's name was clicked.
    RenameStart(TerminalId),
    RenameInput(String),
    /// Enter: an empty name is refused and the old one kept.
    RenameSubmit,
    /// Escape: leave the field and keep the old name.
    RenameCancel,
    /// The strip's close button. The app asks first when the Terminal is busy
    /// (`close_needs_confirm`), then sends `CloseConfirmed`.
    Close(TerminalId),
    CloseConfirmed(TerminalId),
    /// Start a new shell in an exited Terminal, keeping name and directory.
    Restart(TerminalId),
    /// "Folder" in the strip: choose where the strip's new shells start.
    PickDir,
    /// The picker's answer; the app stores it (`db::terminal_dirs`) for the
    /// team, or for the project on a project's strip.
    DirPicked(Option<std::path::PathBuf>),
    /// The strip's clear button. The app forgets the stored folder
    /// (`db::terminal_dirs`): a team's new shells start in home again, a
    /// project's in the team's folder.
    ClearDir,
    Key(Physical, keyboard::Modifiers, Option<String>),
    Pasted(Option<String>),
    /// The selected text, back from the Terminal (JRST-386); `None` copies nothing.
    Copied(Option<String>),
}

static WAKE_RX: OnceLock<Mutex<Option<UnboundedReceiver<()>>>> = OnceLock::new();

/// A resize drag in progress: the panel's height docked at the bottom, its
/// width docked right. The first pointer move after the press fixes where the
/// drag started (y or x).
struct Drag {
    start: Option<f32>,
    start_size: f32,
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
    /// Each project's starting directory; unset means its team's.
    project_dirs: HashMap<String, std::path::PathBuf>,
    /// For the app to show (`take_notices`).
    notices: Vec<String>,
    panel: PanelState,
    /// Where the panel sits. Device-wide, set by the app from its config.
    dock: Dock,
    /// Docked right: the requested width (`None` is the default). Device-wide.
    width: Option<f32>,
    /// Docked right: room taken right of the panel. Nothing sits there now
    /// (detail pages move Properties inline beside the panel), so it stays 0.
    right_inset: f32,
    /// Width the app's left sidebar takes now (0 while collapsed).
    sidebar: f32,
    /// Docked right: the panel body's laid-out size, once measured.
    body: Option<Size>,
    /// The page is a removed team's: its panel stays shown (the Terminals have
    /// no other way in) and can't be hidden or add Terminals.
    pinned: bool,
    focused: bool,
    window: Size,
    cell: Option<(f32, f32)>,
    font: TermFont,
    panes: [Pane; MAX_PANES],
    /// What each pane shows now: `panel.panes` reconciled with the team.
    shown: panel::Panes,
    sent_visible: HashMap<TerminalId, bool>,
    sent_size: HashMap<TerminalId, (u16, u16)>,
    theme_key: &'static str,
    error: Option<String>,
    /// Restored Terminals whose shell failed to start; `sync` doesn't retry
    /// them every message. Restart does.
    start_failed: Vec<TerminalId>,
    /// Terminals whose program is waiting on the person (attention.rs).
    attention: HashMap<TerminalId, Need>,
    /// Banners to post, for the app to take (`take_alerts`).
    alerts: Vec<TerminalAlert>,
    /// The app window has focus. Set by the app.
    window_focused: bool,
    /// The panel was on screen at the last `sync`.
    on_screen: bool,
}

/// A banner about one Terminal, for the app to post. A click on it reveals
/// the Terminal (`reveal`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TerminalAlert {
    pub id: TerminalId,
    pub title: String,
    pub body: String,
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
            project_dirs: HashMap::new(),
            notices: Vec::new(),
            panel: PanelState::default(),
            dock: Dock::Bottom,
            width: None,
            right_inset: 0.0,
            sidebar: panel::SIDEBAR_WIDTH,
            body: None,
            pinned: false,
            focused: false,
            window: Size::new(1100.0, 700.0),
            cell: None,
            font: TermFont::default(),
            panes: std::array::from_fn(Pane::for_slot),
            shown: [None; MAX_PANES],
            sent_visible: HashMap::new(),
            sent_size: HashMap::new(),
            theme_key: t::tok().key,
            error: None,
            start_failed: Vec::new(),
            attention: HashMap::new(),
            alerts: Vec::new(),
            window_focused: true,
            on_screen: false,
        }
    }

    /// Put the stored Terminals back in the strips at launch. No shells start:
    /// each starts when a pane first shows it.
    pub fn restore_records(&mut self, stored: Vec<crate::db::terminal_records::Stored>) {
        self.roster.restore(stored);
    }

    /// The live panel as stored, panes by record key.
    pub fn stored_panel(&self) -> StoredPanel {
        self.panel.to_stored(|id| self.roster.key_of(id))
    }

    /// Make `stored` the live panel: a Tab was focused, or the app launched.
    /// A pane whose Terminal is gone comes back empty. The size each Terminal
    /// was last sent is kept: `sync` resizes any that the new panel shows at
    /// another size.
    pub fn restore_panel(&mut self, stored: &StoredPanel) {
        self.panel = PanelState::from_stored(stored, |key| self.roster.id_of_key(key));
        self.renaming = None;
        self.drag = None;
    }

    /// The strip is being dragged: its height is stored on release.
    pub fn dragging(&self) -> bool {
        self.drag.is_some()
    }

    pub fn update(&mut self, message: TerminalMessage, scope: Option<&Scope>) -> Task<Message> {
        match message {
            TerminalMessage::Wake => {
                // Take the wake first: a frame published after this notifies again.
                self.terminals.ack_wake();
                self.refresh_frame();
                let signals = self.terminals.take_signals();
                for (id, signal) in signals {
                    self.take_signal(id, &signal);
                }
            }
            TerminalMessage::Window(size) => self.window = size,
            TerminalMessage::ToggleFocus => {
                let has = scope.is_some_and(|s| !self.owned_by(s).is_empty());
                match panel::toggle(scope.is_some(), self.panel.visible, has, self.focused) {
                    Toggle::Ignore => {}
                    Toggle::Show { create } => {
                        self.panel.visible = true;
                        self.focused = true;
                        if let (true, Some(scope)) = (create, scope) {
                            self.create(scope, self.panel.focused_pane);
                        }
                    }
                    Toggle::SetFocus(f) => self.focused = f,
                }
            }
            TerminalMessage::Hover(slot, link) => {
                if let Some(pane) = self.panes.get_mut(slot) {
                    pane.set_hover(link);
                }
            }
            TerminalMessage::OpenLink { url, explicit } => {
                // `open` re-checks the scheme: nothing but http(s), or a safe explicit link, opens.
                super::links::open(&url, explicit);
            }
            TerminalMessage::Wheel(slot, w) => {
                if let Some(id) = self.shown.get(slot).copied().flatten() {
                    self.terminals.wheel(id, w);
                }
            }
            TerminalMessage::Mouse(slot, m) => {
                if matches!(m.action, MouseAction::Press(_)) {
                    self.focus_slot(slot);
                    self.focused = true;
                }
                if let Some(id) = self.shown.get(slot).copied().flatten() {
                    self.terminals.mouse(id, m);
                }
            }
            TerminalMessage::Focus(slot) => {
                self.focus_slot(slot);
                self.focused = true;
            }
            TerminalMessage::FocusTerminal => {
                if !self.focused {
                    return self.update(TerminalMessage::ToggleFocus, scope);
                }
            }
            TerminalMessage::FocusApp => self.focused = false,
            TerminalMessage::Moved(x, y) => self.pointer = Some((x, y)),
            TerminalMessage::PressedAt => {
                // Unknown until the pointer moves after focus was taken.
                let leaves = self.pointer.is_some_and(|(x, y)| self.press_leaves(x, y));
                if leaves {
                    self.focused = false;
                }
            }
            TerminalMessage::DragStart => {
                let start_size = match self.dock {
                    Dock::Bottom => self.height(),
                    Dock::Right => self.panel_width(),
                };
                self.drag = Some(Drag { start: None, start_size });
            }
            TerminalMessage::DragMove(x, y) => {
                let (window, inset, sidebar) = (self.window, self.right_inset, self.sidebar);
                if let Some(drag) = &mut self.drag {
                    match self.dock {
                        Dock::Bottom => {
                            let start = *drag.start.get_or_insert(y);
                            self.panel.height =
                                Some(panel::drag_height(window.height, drag.start_size, start, y));
                        }
                        Dock::Right => {
                            let start = *drag.start.get_or_insert(x);
                            self.width = Some(panel::drag_width(window.width, sidebar, inset, drag.start_size, start, x));
                        }
                    }
                }
            }
            TerminalMessage::BodySize(size) => self.body = Some(size),
            TerminalMessage::DragEnd => self.drag = None,
            TerminalMessage::Hide if self.pinned => {}
            TerminalMessage::New | TerminalMessage::NewIn(_) if self.pinned => {}
            TerminalMessage::Hide => {
                // `sync` tells the engine they're hidden (`set_visible`).
                self.panel.visible = false;
                self.focused = false;
            }
            TerminalMessage::New => {
                if let Some(scope) = scope {
                    self.panel.visible = true;
                    self.focused = true;
                    let slot = panel::slot_for_new(&self.shown, self.panel.layout, self.panel.focused_pane);
                    self.create(scope, slot);
                }
            }
            TerminalMessage::NewIn(slot) => {
                if let Some(scope) = scope {
                    self.focused = true;
                    self.create(scope, slot);
                }
            }
            TerminalMessage::Select(id) => {
                self.renaming = None;
                if scope.is_some_and(|s| self.owned_by(s).contains(&id)) {
                    self.place(self.panel.focused_pane, id);
                    self.focused = true;
                }
            }
            TerminalMessage::Pick(slot, id) => {
                self.renaming = None;
                if scope.is_some_and(|s| self.owned_by(s).contains(&id)) {
                    self.focus_slot(slot);
                    self.place(slot, id);
                    self.focused = true;
                }
            }
            TerminalMessage::SetLayout(layout) => {
                self.panel.layout = layout;
                self.focus_slot(self.panel.focused_pane);
                self.sent_size.clear();
            }
            TerminalMessage::CyclePane(delta) => self.panel.cycle_focus(delta),
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
            TerminalMessage::RenameCancel => {
                if self.renaming.take().is_some() {
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
            // The app stores it first and then calls `set_dir`.
            TerminalMessage::DirPicked(_) | TerminalMessage::ClearDir => {}
            TerminalMessage::Key(Physical::Code(code), mods, text) => {
                match input::route(code, mods, text.as_deref()) {
                    Route::ToggleFocus => return self.update(TerminalMessage::ToggleFocus, scope),
                    Route::Hide => return self.update(TerminalMessage::Hide, scope),
                    Route::Palette => return Task::done(Message::OpenCommandPalette),
                    Route::Paste => {
                        return iced::clipboard::read()
                            .map(|s| Message::Terminal(TerminalMessage::Pasted(s)))
                    }
                    Route::CyclePane(delta) => self.panel.cycle_focus(delta),
                    // The answer comes from the Terminal's owner thread, which
                    // can take up to 500 ms if it is stuck: wait on a plain OS
                    // thread (not tokio) so `update` returns at once. No selection copies
                    // nothing, and nothing is sent to the program either way.
                    Route::Copy => {
                        let Some(req) = self.focused_terminal().and_then(|id| self.terminals.selection_request(id))
                        else {
                            return Task::none();
                        };
                        return Task::perform(
                            req.wait_off_thread(),
                            |text| Message::Terminal(TerminalMessage::Copied(text)),
                        );
                    }
                    // The app's own Tab switch. Focus follows `sync`: it stays
                    // with the Terminal while the new Tab shows a panel, and
                    // falls to the app on a page without one.
                    Route::Tab(i) => {
                        return Task::done(Message::Tab(crate::app::TabMessage::Focus(i)))
                    }
                    Route::CloseTab => {
                        return Task::done(Message::Tab(crate::app::TabMessage::CloseActive))
                    }
                    Route::ReopenTab => {
                        return Task::done(Message::Tab(crate::app::TabMessage::ReopenClosed))
                    }
                    Route::ToggleSidebar => return Task::done(Message::ToggleSidebar),
                    Route::Send(key) => {
                        if let Some(id) = self.focused_terminal() {
                            self.terminals.send_key(id, key);
                        }
                    }
                    Route::Ignore => {}
                }
            }
            TerminalMessage::Key(..) => {}
            // No text on the clipboard (an image, say) still goes through as an
            // empty paste: a bracketed-paste program sees `ESC[200~ESC[201~` and
            // can read the clipboard itself, the way Claude Code pulls in an
            // image. Without bracketed paste nothing is sent.
            TerminalMessage::Pasted(text) => {
                if let Some(id) = self.focused_terminal() {
                    self.terminals.paste(id, text.unwrap_or_default());
                }
            }
            TerminalMessage::Copied(Some(text)) if !text.is_empty() => {
                return iced::clipboard::write(text);
            }
            TerminalMessage::Copied(_) => {}
        }
        Task::none()
    }

    /// Reconcile the engine and the canvas with the current strip and layout.
    /// Cheap when nothing changed: it sends only differences.
    pub fn sync(&mut self, scope: Option<&Scope>) {
        if self.pinned && scope.is_some() {
            self.panel.visible = true;
        }
        if scope.is_none() || !self.panel.visible {
            self.focused = false;
        }
        let on_screen = scope.is_some() && self.panel.visible;
        self.on_screen = on_screen;
        let layout = self.panel.layout;
        let shown = match scope {
            Some(s) => panel::reconcile(&self.panel.panes, &self.owned_by(s), layout),
            None => [None; MAX_PANES],
        };
        let mut shown = shown;
        // A restored Terminal starts its shell when a pane first shows it.
        if on_screen {
            for pane in shown.iter_mut().take(layout.slots()) {
                if let Some(started) = pane.and_then(|id| self.start_restored(id)) {
                    *pane = Some(started);
                }
            }
        }
        for ((new, old), pane) in shown.iter().zip(self.shown.iter()).zip(self.panes.iter_mut()) {
            if new != old {
                pane.reset();
            }
        }
        self.shown = shown;
        if scope.is_some() {
            self.panel.panes = shown;
        }
        // Visibility, for every Terminal: only those in a pane build frames.
        let visible_ids = &shown[..layout.slots()];
        for id in self.terminals.ids() {
            let visible = on_screen && visible_ids.contains(&Some(id));
            if self.sent_visible.insert(id, visible) != Some(visible) {
                self.terminals.set_visible(id, visible);
                if visible {
                    self.refresh_frame_of(id);
                }
            }
        }
        // Seen is handled: a Terminal on screen in a focused window loses its badge.
        if on_screen && self.window_focused {
            for id in visible_ids.iter().flatten() {
                self.attention.remove(id);
            }
        }
        // A pane takes its Terminal's size; a hidden Terminal keeps its last.
        if let (true, Some(cell)) = (on_screen, self.cell) {
            let grid = self.grid(cell);
            // A too-small pane leaves its PTY at the last size; enlarging
            // the pane sends the new one.
            for id in visible_ids.iter().flatten().filter(|_| !panel::too_small(grid.0, grid.1)) {
                if self.sent_size.get(id) != Some(&grid) {
                    self.sent_size.insert(*id, grid);
                    self.terminals.resize(*id, grid.0, grid.1, cell.0, cell.1);
                }
            }
        }
        let key = t::tok().key;
        if key != self.theme_key {
            self.theme_key = key;
            let theme = active_term_theme();
            for id in self.terminals.ids() {
                self.terminals.set_theme(id, theme.clone());
            }
            for pane in &mut self.panes {
                pane.reset();
            }
        }
        for (slot, pane) in self.panes.iter_mut().enumerate() {
            pane.set_focused(self.focused && slot == self.panel.focused_pane);
        }
        if !self.focused {
            // A position seen before focus began may be stale by the time of a press.
            self.pointer = None;
        }
    }

    /// Apply the Terminal font from config, live. The cell is measured again;
    /// `sync` then resizes the shown Terminal's grid and PTY to it, and a
    /// hidden Terminal follows when it is next shown.
    pub fn set_font(&mut self, font: TermFont) {
        if font == self.font {
            return;
        }
        self.font = font;
        for pane in &mut self.panes {
            pane.set_font(font);
        }
        if self.cell.is_some() {
            let cell = terminal_panel::measure_cell(font);
            self.cell = Some(cell);
            for pane in &mut self.panes {
                pane.set_cell(cell);
            }
        }
        self.sent_size.clear();
    }

    /// Whether key presses go to the Terminal instead of the app's shortcuts.
    pub fn takes_keys(&self, modal_open: bool) -> bool {
        panel::keys_go_to_terminal(self.focused, self.panel.visible, modal_open)
    }

    /// Which focus commands the palette offers on this page.
    pub fn palette_focus(&self, scope: Option<&Scope>) -> crate::palette_results::TerminalFocus {
        use crate::palette_results::TerminalFocus as F;
        match (scope, self.focused && self.panel.visible) {
            (None, _) => F::Unavailable,
            (Some(_), true) => F::Terminal,
            (Some(_), false) => F::App,
        }
    }

    /// The panel is on screen for this page.
    pub fn panel_showing(&self, scope: Option<&Scope>) -> bool {
        scope.is_some() && self.panel.visible
    }

    /// The panel for the current strip, if it is showing.
    pub fn panel_view(&self, scope: Option<&Scope>) -> Option<Element<'_, Message>> {
        let scope = scope?;
        if !self.panel.visible {
            return None;
        }
        let layout = self.panel.layout;
        let focused_id = self.focused_terminal();
        let owned = self.owned_by(scope);
        let offer: Vec<(TerminalId, String)> = panel::unshown(&owned, &self.shown, layout)
            .into_iter()
            .filter_map(|id| Some((id, self.roster.get(id)?.name.clone())))
            .collect();
        let too_small = self.cell.is_some_and(|c| {
            let (cols, rows) = self.grid(c);
            panel::too_small(cols, rows)
        });
        let slots = (0..layout.slots())
            .map(|slot| {
                let id = self.shown[slot];
                PaneView {
                    slot,
                    pane: id.map(|_| &self.panes[slot]),
                    focused: slot == self.panel.focused_pane,
                    too_small,
                    offer: if id.is_none() { offer.clone() } else { Vec::new() },
                    notice: id.and_then(|id| {
                        let dead = self.terminals.frame(id).is_some_and(|f| f.dead);
                        let exited = self.roster.get(id).is_some_and(|r| r.shell) && self.terminals.is_exited(id);
                        panel::pane_notice(exited, dead).map(|n| (id, n))
                    }),
                }
            })
            .collect();
        let items = self
            .strip(scope)
            .into_iter()
            .map(|r| StripItem {
                id: r.id,
                name: r.name.clone(),
                active: Some(r.id) == focused_id,
                exited: r.shell && self.terminals.is_exited(r.id),
                need: self.attention.get(&r.id).copied(),
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
            .or_else(|| owned.is_empty().then(|| "none open".to_string()));
        Some(terminal_panel::view(PanelView {
            layout,
            slots,
            items,
            dir_label: self.dir_for(scope).zip(self.dir_is_own(scope)).map(|(d, own)| {
                let name =
                    d.file_name().map_or_else(|| d.display().to_string(), |n| n.to_string_lossy().into_owned());
                match (&scope.project, own) {
                    (Some(_), true) => format!("Project folder: {name}"),
                    (Some(_), false) => format!("Team folder: {name}"),
                    (None, _) => format!("Team folder: {name}"),
                }
            }),
            can_clear_dir: self.dir_is_own(scope) == Some(true),
            note,
            dock: self.dock,
            height: panel::panel_height(self.window.height, self.panel.height),
            width: self.panel_width(),
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
                    Some(Message::Terminal(TerminalMessage::DragMove(position.x, position.y)))
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

    /// Width of the panel docked right.
    fn panel_width(&self) -> f32 {
        panel::panel_width(self.window.width, self.sidebar, self.right_inset, self.width)
    }

    /// Whether a left press at window point (`x`, `y`) is outside the panel.
    fn press_leaves(&self, x: f32, y: f32) -> bool {
        match self.dock {
            Dock::Bottom => panel::press_leaves_panel(self.window.height, self.sidebar, self.panel.height, x, y),
            Dock::Right => panel::press_leaves_right_panel(
                self.window.width,
                self.window.height,
                self.right_inset,
                self.panel_width(),
                self.body_size().1,
                x,
                y,
            ),
        }
    }

    /// Docked right: the panel body (w, h), measured once laid out. Before
    /// that, a guess: the panel's width less its handle, and the window less
    /// the title, tab and header bars, the strip and the status bar.
    fn body_size(&self) -> (f32, f32) {
        match self.body {
            Some(b) => (b.width, b.height),
            None => (
                self.panel_width() - panel::HANDLE_WIDTH,
                (self.window.height - 110.0 - panel::STRIP_HEIGHT - panel::STATUS_HEIGHT).max(0.0),
            ),
        }
    }

    pub fn dock(&self) -> Dock {
        self.dock
    }

    /// Dock the panel. Every pane changes size, so each Terminal is resized.
    pub fn set_dock(&mut self, dock: Dock) {
        if self.dock != dock {
            self.dock = dock;
            self.body = None;
            self.drag = None;
            self.sent_size.clear();
        }
    }

    /// The requested right-docked width, for the app to store.
    pub fn width(&self) -> Option<f32> {
        self.width
    }

    pub fn set_width(&mut self, width: Option<f32>) {
        self.width = width;
    }

    /// The app sidebar collapsed or expanded: the panel's room changes, so
    /// the right-docked body is re-measured and each pane resized.
    pub fn set_sidebar_width(&mut self, width: f32) {
        if self.sidebar != width {
            self.sidebar = width;
            self.body = None;
        }
    }

    /// Width a right-docked panel takes from the page now (its edge handle
    /// included), or 0 when it isn't showing there.
    pub fn right_width(&self, scope: Option<&Scope>) -> f32 {
        if self.dock == Dock::Right && self.panel_showing(scope) {
            self.panel_width()
        } else {
            0.0
        }
    }

    /// The records the strip of `scope` lists. A removed team's pinned panel
    /// lists all of the team's, its projects' included: their project Tabs
    /// closed with the team, so it is the only way left to reach them.
    fn strip<'a>(&'a self, scope: &'a Scope) -> Vec<&'a super::roster::Record> {
        if self.pinned && scope.project.is_none() {
            self.roster.of_team(&scope.team).collect()
        } else {
            self.roster.of_scope(scope).collect()
        }
    }

    fn owned_by(&self, scope: &Scope) -> Vec<TerminalId> {
        self.strip(scope).into_iter().map(|r| r.id).collect()
    }

    /// Where new shells in the strip of `scope` start: the project's folder,
    /// else the team's, else none (home).
    fn dir_for(&self, scope: &Scope) -> Option<&std::path::PathBuf> {
        scope
            .project
            .as_ref()
            .and_then(|p| self.project_dirs.get(p))
            .or_else(|| self.team_dirs.get(&scope.team))
    }

    /// Strip changes since the last call, for the app to store.
    pub fn take_record_changes(&mut self) -> Vec<crate::db::terminal_records::Change> {
        self.roster.take_changes()
    }

    /// A strip name is being edited.
    pub fn is_renaming(&self) -> bool {
        self.renaming.is_some()
    }

    pub fn set_team_dirs(&mut self, dirs: HashMap<String, std::path::PathBuf>) {
        self.team_dirs = dirs;
    }

    pub fn set_project_dirs(&mut self, dirs: HashMap<String, std::path::PathBuf>) {
        self.project_dirs = dirs;
    }

    /// New shells in the strip of `scope` start in `dir`; running shells
    /// don't move. A project's folder is its own: clearing it falls back to
    /// the team's, and setting the team's leaves it alone. Records that were
    /// started in a strip's old folder follow it, so their next shell
    /// (Restart, or the next launch) starts in the new one: that includes the
    /// team's projects that use the team's folder.
    pub fn set_dir(&mut self, scope: &Scope, dir: Option<std::path::PathBuf>) {
        let scopes = self.roster.scopes_of_team(&scope.team);
        let before: Vec<Option<std::path::PathBuf>> = scopes.iter().map(|s| self.dir_for(s).cloned()).collect();
        let (dirs, key) = match &scope.project {
            Some(p) => (&mut self.project_dirs, p),
            None => (&mut self.team_dirs, &scope.team),
        };
        match dir {
            Some(d) => dirs.insert(key.clone(), d),
            None => dirs.remove(key),
        };
        for (s, old) in scopes.iter().zip(before) {
            let new = self.dir_for(s).cloned();
            if new != old {
                self.roster.retarget(s, old.as_deref(), new.as_deref());
            }
        }
    }

    /// Whether the strip of `scope` uses its own folder (true) or, on a
    /// project's strip, the team's (false). `None` when no folder applies.
    fn dir_is_own(&self, scope: &Scope) -> Option<bool> {
        match &scope.project {
            Some(p) if self.project_dirs.contains_key(p) => Some(true),
            Some(_) => self.team_dirs.contains_key(&scope.team).then_some(false),
            None => self.team_dirs.contains_key(&scope.team).then_some(true),
        }
    }

    /// Messages for the app to show as toasts since the last call: a shell
    /// that could not start in its folder and started in home instead.
    pub fn take_notices(&mut self) -> Vec<String> {
        std::mem::take(&mut self.notices)
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

    /// The names of the busy Terminals in one strip, for the Tab-close ask.
    pub fn busy_names(&self, scope: &Scope) -> Vec<String> {
        self.roster
            .of_scope(scope)
            .filter(|r| self.terminals.is_busy(r.id))
            .map(|r| r.name.clone())
            .collect()
    }

    /// Stop every Terminal in one strip at once, busy ones included: the
    /// Tab that showed them is closing.
    pub fn close_scope(&mut self, scope: &Scope) {
        for id in self.roster.ids_of_scope(scope) {
            self.close(id);
        }
    }

    /// Stop a Terminal now. It leaves the strip at once; its shell is
    /// reaped in the background.
    fn close(&mut self, id: TerminalId) {
        self.roster.remove(id);
        self.attention.remove(&id);
        self.sent_visible.remove(&id);
        self.sent_size.remove(&id);
        if self.renaming.as_ref().is_some_and(|(r, _)| *r == id) {
            self.renaming = None;
        }
        for p in self.panel.panes.iter_mut().filter(|p| **p == Some(id)) {
            *p = None;
        }
        if let Some(closing) = self.terminals.close(id) {
            self.closing.retain(|c| !c.is_done());
            self.closing.push(closing);
        }
        for pane in &mut self.panes {
            pane.reset();
        }
        self.shown = self.panel.panes;
    }

    /// A click on `team`'s sidebar row has just focused its Tab: show that
    /// Tab's panel if the team's own strip has an open Terminal (a project's
    /// don't show there). The keyboard stays with the app.
    pub fn reveal_for_click(&mut self, team: &str) {
        let scope = Scope::team(team);
        let open = self
            .roster
            .of_scope(&scope)
            .filter(|r| r.shell && !self.terminals.is_exited(r.id))
            .count();
        self.panel.visible = panel::visible_after_click(open, self.panel.visible);
    }

    /// The page is a removed team's, whose panel can't be hidden.
    pub fn set_pinned(&mut self, pinned: bool) {
        self.pinned = pinned;
    }

    /// Whether the team has any Terminal, in its own strip or a project's,
    /// running or stopped.
    pub fn has_terminals(&self, team: &str) -> bool {
        self.roster.of_team(team).next().is_some()
    }

    /// How many of the team's Terminals, its projects' included, have a live shell.
    pub fn open_count(&self, team: &str) -> usize {
        self.terminals.open_count(team)
    }

    /// Paste `text` into the focused pane's Terminal (no Enter) and give it the
    /// keyboard. False when the panel is hidden or the pane is empty or exited.
    pub fn type_into_focused(&mut self, text: String) -> bool {
        let Some(id) = self.focused_terminal().filter(|&id| self.panel.visible && !self.terminals.is_exited(id))
        else {
            return false;
        };
        self.terminals.paste(id, text);
        self.focused = true;
        true
    }

    /// Fold one attention signal from `id`'s program into its badge, and
    /// queue a banner when one is due.
    fn take_signal(&mut self, id: TerminalId, signal: &super::Signal) {
        let Some(name) = self.roster.get(id).map(|r| r.name.clone()) else { return };
        let watching = self.watching(id);
        let (need, alert) = attention::apply(self.attention.get(&id).copied(), signal, watching, &name);
        match need {
            Some(n) => self.attention.insert(id, n),
            None => self.attention.remove(&id),
        };
        if let Some(Alert { title, body }) = alert {
            self.alerts.push(TerminalAlert { id, title, body });
        }
    }

    /// The person can see `id` now: it is in a pane of the panel on screen
    /// and the window has focus.
    fn watching(&self, id: TerminalId) -> bool {
        self.window_focused && self.on_screen && self.shown[..self.panel.layout.slots()].contains(&Some(id))
    }

    /// The app window gained or lost focus.
    pub fn set_window_focused(&mut self, focused: bool) {
        self.window_focused = focused;
    }

    /// Banners queued since the last call, oldest first.
    pub fn take_alerts(&mut self) -> Vec<TerminalAlert> {
        std::mem::take(&mut self.alerts)
    }

    /// The most pressing need among the Terminals of the strip of `scope`.
    pub fn scope_need(&self, scope: &Scope) -> Option<Need> {
        attention::strongest(self.roster.of_scope(scope).filter_map(|r| self.attention.get(&r.id).copied()))
    }

    /// The most pressing need among all of a team's Terminals, its projects' included.
    pub fn team_need(&self, team: &str) -> Option<Need> {
        attention::strongest(self.roster.of_team(team).filter_map(|r| self.attention.get(&r.id).copied()))
    }

    /// The strip `id` is in, for the app to open its Tab.
    pub fn scope_of(&self, id: TerminalId) -> Option<Scope> {
        let r = self.roster.get(id)?;
        Some(Scope { team: r.team.clone(), project: r.project.clone() })
    }

    /// Bring `id` to the person: show the panel, focus the pane showing it
    /// (or put it in the focused pane) and give it the keyboard. A banner
    /// click calls this once its Tab is open. Nothing happens unless the
    /// strip of `scope` has it.
    pub fn reveal(&mut self, id: TerminalId, scope: Option<&Scope>) {
        if !scope.is_some_and(|s| self.owned_by(s).contains(&id)) {
            return;
        }
        self.renaming = None;
        self.panel.visible = true;
        match self.shown[..self.panel.layout.slots()].iter().position(|p| *p == Some(id)) {
            Some(slot) => self.focus_slot(slot),
            None => self.place(self.panel.focused_pane, id),
        }
        self.focused = true;
        self.attention.remove(&id);
    }

    fn focused_terminal(&self) -> Option<TerminalId> {
        self.shown[self.panel.focused_pane.min(self.panel.layout.slots() - 1)]
    }

    fn focus_slot(&mut self, slot: usize) {
        self.panel.focused_pane = slot.min(self.panel.layout.slots() - 1);
    }

    /// Put `id` in `slot` of the panel (moving it out of any other pane).
    fn place(&mut self, slot: usize, id: TerminalId) {
        panel::assign(&mut self.panel.panes, slot, id);
        self.shown = self.panel.panes;
        self.panes[slot].reset();
    }

    /// Size in cells of one pane; every pane of a layout is the same size.
    fn grid(&self, cell: (f32, f32)) -> (u16, u16) {
        let (w, h) = match self.dock {
            Dock::Bottom => panel::pane_size(
                self.window.width,
                self.sidebar,
                self.window.height,
                self.panel.height,
                self.panel.layout,
            ),
            Dock::Right => panel::split_body(self.body_size(), self.panel.layout, Dock::Right),
        };
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
                for p in self.panel.panes.iter_mut().filter(|p| **p == Some(id)) {
                    *p = Some(new);
                }
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
        self.attention.remove(&id);
        if let Some(old) = self.terminals.close(id) {
            self.closing.retain(|c| !c.is_done());
            self.closing.push(old);
        }
        self.sent_visible.remove(&id);
        self.sent_size.remove(&id);
        for p in self.panel.panes.iter_mut().filter(|p| **p == Some(id)) {
            *p = Some(new);
        }
        self.shown = self.panel.panes;
        self.focused = true;
    }

    /// Start a shell for `team`; shows the error in the strip on failure.
    fn spawn(&mut self, team: &str, cwd: Option<std::path::PathBuf>) -> Option<TerminalId> {
        let font = self.font;
        let cell = *self.cell.get_or_insert_with(|| terminal_panel::measure_cell(font));
        for pane in &mut self.panes {
            pane.set_cell(cell);
        }
        let (cols, rows) = panel::spawn_grid(self.grid(cell));
        if let Some(note) = cwd.as_deref().and_then(unusable_dir_notice) {
            if !self.notices.contains(&note) {
                self.notices.push(note);
            }
        }
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

    /// Start a Terminal in the strip of `scope`, in pane `slot`.
    fn create(&mut self, scope: &Scope, slot: usize) {
        let cwd = self.dir_for(scope).cloned();
        if let Some(id) = self.spawn(&scope.team, cwd.clone()) {
            self.roster.add(id, scope, cwd);
            self.focus_slot(slot);
            self.place(self.panel.focused_pane, id);
        }
    }

    fn refresh_frame(&mut self) {
        let ids: Vec<TerminalId> = self.shown[..self.panel.layout.slots()].iter().flatten().copied().collect();
        for id in ids {
            self.refresh_frame_of(id);
        }
    }

    fn refresh_frame_of(&mut self, id: TerminalId) {
        let Some(slot) = self.shown[..self.panel.layout.slots()].iter().position(|p| *p == Some(id)) else {
            return;
        };
        if let Some(frame) = self.terminals.frame(id) {
            self.panes[slot].show(frame);
        }
    }
}

/// The strip the current page shows, before the Tab set exists: a project
/// page shows the project's, any other team page the team's own. Cross-team
/// pages have no panel (their Terminals keep running).
pub fn panel_scope(view: &crate::app::View, selected_team_id: Option<&str>) -> Option<Scope> {
    use crate::app::View;
    let team = selected_team_id?;
    match view {
        View::MyIssues | View::Inbox | View::Dashboard(_) => None,
        View::ProjectDetail(project) => Some(Scope::project(team, project.clone())),
        _ => Some(Scope::team(team)),
    }
}

/// The notice for a shell whose folder can't be used: the engine starts it in
/// home instead (`engine::spawn`). `None` when the folder is fine. A folder in
/// Documents, Desktop or Downloads can look missing when macOS has not let
/// JAIT read it.
fn unusable_dir_notice(dir: &std::path::Path) -> Option<String> {
    if dir.is_dir() {
        return None;
    }
    Some(format!(
        "Can't open {}, so the Terminal started in your home folder. If the folder exists, \
         allow JAIT under System Settings > Privacy & Security > Files and Folders.",
        crate::terminal::display_path(dir)
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::terminal_records::Stored;

    fn record(key: &str, team: &str) -> Stored {
        Stored { key: key.into(), team_id: team.into(), project_id: None, name: key.into(), cwd: None }
    }

    fn project_record(key: &str, team: &str, project: &str) -> Stored {
        Stored { project_id: Some(project.into()), ..record(key, team) }
    }

    fn a() -> Scope {
        Scope::team("a")
    }

    fn panel(layout: &str, panes: [Option<&str>; 4]) -> StoredPanel {
        StoredPanel {
            visible: true,
            height: Some(250.0),
            layout: layout.into(),
            panes: panes.map(|k| k.map(str::to_string)),
            focused_pane: 0,
        }
    }

    #[test]
    fn a_stored_panel_comes_back_by_key_and_drops_terminals_that_are_gone() {
        let mut t = TerminalState::new();
        t.restore_records(vec![record("k1", "a"), record("k2", "a")]);
        t.restore_panel(&panel("columns", [Some("k2"), Some("closed"), None, None]));
        let back = t.stored_panel();
        assert_eq!(back.panes, [Some("k2".to_string()), None, None, None]);
        assert_eq!((back.visible, back.height, back.layout.as_str()), (true, Some(250.0), "columns"));
    }

    #[test]
    fn two_tabs_keep_separate_panels_over_one_shared_terminal() {
        let mut t = TerminalState::new();
        t.restore_records(vec![record("k1", "a")]);
        let team_tab = panel("grid", [Some("k1"), None, None, None]);
        let project_tab = StoredPanel { visible: false, height: Some(180.0), ..panel("single", [Some("k1"), None, None, None]) };
        // Park the team Tab's panel, show the project Tab's, switch back.
        t.restore_panel(&team_tab);
        let parked_team = t.stored_panel();
        t.restore_panel(&project_tab);
        let parked_project = t.stored_panel();
        t.restore_panel(&parked_team);
        assert_eq!(t.stored_panel(), parked_team);
        assert_eq!((parked_team.layout.as_str(), parked_team.visible), ("grid", true));
        assert_eq!((parked_project.layout.as_str(), parked_project.visible), ("single", false));
        assert_eq!(parked_team.panes[0], parked_project.panes[0], "one Terminal, two Tabs");
    }

    #[test]
    fn a_parked_panel_follows_a_terminal_whose_id_changed() {
        // Restart and a restored shell starting both give a record a new id.
        let mut t = TerminalState::new();
        t.restore_records(vec![record("k1", "a")]);
        let parked = panel("single", [Some("k1"), None, None, None]);
        let old = t.roster.id_of_key("k1").unwrap();
        t.roster.replace_id(old, TerminalId(42));
        t.restore_panel(&parked);
        assert_eq!(t.panel.panes[0], Some(TerminalId(42)));
    }

    #[test]
    fn cancelling_a_rename_keeps_the_old_name_and_closes_the_field() {
        let mut t = TerminalState::new();
        t.restore_records(vec![record("k1", "a")]);
        let id = t.roster.id_of_key("k1").unwrap();
        let _ = t.update(TerminalMessage::RenameStart(id), Some(&a()));
        let _ = t.update(TerminalMessage::RenameInput("changed".into()), Some(&a()));
        assert!(t.is_renaming());
        let _ = t.update(TerminalMessage::RenameCancel, Some(&a()));
        assert!(!t.is_renaming());
        assert_eq!(t.roster.get(id).unwrap().name, "k1");
        assert!(t.focused, "the keyboard goes back to the shell");
    }

    #[test]
    fn clearing_a_teams_folder_sends_new_shells_home_and_leaves_other_teams() {
        let mut t = TerminalState::new();
        t.set_dir(&a(), Some("/tmp".into()));
        t.set_dir(&Scope::team("b"), Some("/var".into()));
        t.set_dir(&a(), None);
        assert!(!t.team_dirs.contains_key("a"));
        assert!(t.team_dirs.contains_key("b"));
        // Clearing is its own message, so a cancelled picker never clears.
        let _ = t.update(TerminalMessage::DirPicked(None), Some(&Scope::team("b")));
        assert!(t.team_dirs.contains_key("b"));
    }

    #[test]
    fn a_new_folder_moves_the_records_that_followed_the_old_one() {
        let mut t = TerminalState::new();
        let p = Scope::project("a", "p");
        t.restore_records(vec![record("k1", "a"), project_record("k2", "a", "p"), record("k3", "b")]);
        let _ = t.take_record_changes();
        t.set_dir(&a(), Some("/work".into()));
        let cwd = |t: &TerminalState, k: &str| t.roster.get(t.roster.id_of_key(k).unwrap()).unwrap().cwd.clone();
        assert_eq!(cwd(&t, "k1"), Some("/work".into()));
        assert_eq!(cwd(&t, "k2"), Some("/work".into()), "the project used the team's folder");
        assert_eq!(cwd(&t, "k3"), None, "another team is untouched");
        t.set_dir(&p, Some("/work/api".into()));
        assert_eq!(cwd(&t, "k1"), Some("/work".into()));
        assert_eq!(cwd(&t, "k2"), Some("/work/api".into()));
        t.set_dir(&a(), None);
        assert_eq!(cwd(&t, "k1"), None);
        assert_eq!(cwd(&t, "k2"), Some("/work/api".into()), "the project keeps its own");
        assert_eq!(t.take_record_changes().len(), 4, "each move is stored");
    }

    #[test]
    fn the_folder_label_says_whose_folder_it_is() {
        let mut t = TerminalState::new();
        let p = Scope::project("a", "p");
        assert_eq!(t.dir_is_own(&p), None);
        t.set_dir(&a(), Some("/work".into()));
        assert_eq!((t.dir_is_own(&a()), t.dir_is_own(&p)), (Some(true), Some(false)));
        t.set_dir(&p, Some("/work/api".into()));
        assert_eq!(t.dir_is_own(&p), Some(true));
    }

    #[test]
    fn a_folder_that_is_gone_gets_a_notice() {
        assert!(unusable_dir_notice(&std::env::temp_dir()).is_none());
        let note = unusable_dir_notice(std::path::Path::new("/no/such/mtasks-dir")).unwrap();
        assert!(note.contains("/no/such/mtasks-dir"), "{note}");
    }

    #[test]
    fn a_projects_folder_overrides_its_teams_and_clearing_it_falls_back() {
        let mut t = TerminalState::new();
        let p = Scope::project("a", "p");
        t.set_dir(&a(), Some("/work".into()));
        assert_eq!(t.dir_for(&p), Some(&"/work".into()), "the team's until the project has one");
        t.set_dir(&p, Some("/work/api".into()));
        assert_eq!(t.dir_for(&p), Some(&"/work/api".into()));
        assert_eq!(t.dir_for(&a()), Some(&"/work".into()), "the team's own is untouched");
        t.set_dir(&p, None);
        assert_eq!(t.dir_for(&p), Some(&"/work".into()));
    }

    #[test]
    fn submitting_a_rename_still_changes_the_name() {
        let mut t = TerminalState::new();
        t.restore_records(vec![record("k1", "a")]);
        let id = t.roster.id_of_key("k1").unwrap();
        let _ = t.update(TerminalMessage::RenameStart(id), Some(&a()));
        let _ = t.update(TerminalMessage::RenameInput("changed".into()), Some(&a()));
        let _ = t.update(TerminalMessage::RenameSubmit, Some(&a()));
        assert_eq!(t.roster.get(id).unwrap().name, "changed");
    }

    #[test]
    fn cmd_j_on_a_page_without_a_panel_does_nothing() {
        let mut t = TerminalState::new();
        t.restore_records(vec![record("k1", "a")]);
        let _ = t.update(TerminalMessage::ToggleFocus, None);
        assert!(!t.stored_panel().visible);
        assert!(!t.takes_keys(false));
        assert_eq!(t.roster.ids_of_team("a").len(), 1, "no Terminal was created");
    }

    #[test]
    fn a_page_without_a_panel_shows_none_and_hides_every_terminal() {
        let mut t = TerminalState::new();
        t.restore_records(vec![record("k1", "a")]);
        t.restore_panel(&panel("single", [Some("k1"), None, None, None]));
        t.sync(None);
        assert!(!t.panel_showing(None));
        assert!(t.panel_view(None).is_none());
        assert!(!t.takes_keys(false));
    }

    #[test]
    fn a_team_and_each_of_its_projects_list_their_own_terminals() {
        let mut t = TerminalState::new();
        t.restore_records(vec![
            record("k1", "a"),
            project_record("k2", "a", "p"),
            project_record("k3", "a", "q"),
            record("k4", "b"),
        ]);
        let keys = |s: &Scope| -> Vec<String> {
            t.owned_by(s).into_iter().filter_map(|id| t.roster.key_of(id)).collect()
        };
        assert_eq!(keys(&a()), ["k1"]);
        assert_eq!(keys(&Scope::project("a", "p")), ["k2"]);
        assert_eq!(keys(&Scope::project("a", "q")), ["k3"]);
        assert!(t.has_terminals("a"));
    }

    #[test]
    fn a_new_terminal_on_a_project_strip_belongs_to_the_project() {
        let mut t = TerminalState::new();
        let p = Scope::project("a", "p");
        t.roster.add(TerminalId(1), &p, None);
        assert_eq!(t.owned_by(&p), [TerminalId(1)]);
        assert!(t.owned_by(&a()).is_empty());
    }

    #[test]
    fn a_removed_teams_pinned_panel_lists_its_projects_terminals_too() {
        let mut t = TerminalState::new();
        t.restore_records(vec![record("k1", "gone"), project_record("k2", "gone", "p")]);
        t.set_pinned(true);
        assert_eq!(t.owned_by(&Scope::team("gone")).len(), 2);
    }

    #[test]
    fn a_click_on_a_team_with_no_open_terminals_leaves_the_panel_alone() {
        let mut t = TerminalState::new();
        // Restored and not started: in the strip, but not open.
        t.restore_records(vec![record("k1", "a")]);
        t.reveal_for_click("a");
        assert!(!t.stored_panel().visible);
    }

    fn status(state: super::super::ProgramState) -> super::super::Signal {
        super::super::Signal::Status { state, app: String::new(), title: String::new(), message: String::new() }
    }

    #[test]
    fn a_hidden_terminal_that_needs_the_person_is_flagged_until_seen() {
        use super::super::{BlockKind, ProgramState};
        let mut t = TerminalState::new();
        let id = TerminalId(1);
        t.roster.add(id, &a(), None);
        // The panel is hidden: the person can't see the prompt.
        t.take_signal(id, &status(ProgramState::Blocked(BlockKind::Permission)));
        let blocked = Some(Need::Blocked(BlockKind::Permission));
        assert_eq!((t.scope_need(&a()), t.team_need("a")), (blocked, blocked));
        assert!(t.scope_need(&Scope::project("a", "p")).is_none(), "a project's Tab is not flagged");
        let alerts = t.take_alerts();
        assert_eq!(alerts.len(), 1);
        assert_eq!((alerts[0].id, alerts[0].body.as_str()), (id, "Needs your permission"));
        // A banner click shows it, focused, and the flag goes.
        t.reveal(id, Some(&a()));
        t.sync(Some(&a()));
        assert!(t.stored_panel().visible && t.focused);
        assert_eq!(t.focused_terminal(), Some(id));
        assert!(t.scope_need(&a()).is_none());
    }

    #[test]
    fn a_terminal_on_screen_is_flagged_only_while_the_window_is_in_the_background() {
        use super::super::ProgramState;
        let mut t = TerminalState::new();
        let id = TerminalId(1);
        t.roster.add(id, &a(), None);
        t.reveal(id, Some(&a()));
        t.sync(Some(&a()));
        t.take_signal(id, &status(ProgramState::Done));
        assert!(t.scope_need(&a()).is_none(), "the person is looking at it");
        assert!(t.take_alerts().is_empty());
        t.set_window_focused(false);
        t.take_signal(id, &status(ProgramState::Done));
        assert_eq!(t.scope_need(&a()), Some(Need::Done));
        assert_eq!(t.take_alerts().len(), 1);
        // Back to the window: on screen, so seen.
        t.set_window_focused(true);
        t.sync(Some(&a()));
        assert!(t.scope_need(&a()).is_none());
    }

    #[test]
    fn closing_a_flagged_terminal_drops_its_flag() {
        let mut t = TerminalState::new();
        let id = TerminalId(1);
        t.roster.add(id, &a(), None);
        t.take_signal(id, &super::super::Signal::Bell);
        assert_eq!(t.team_need("a"), Some(Need::Bell));
        let _ = t.update(TerminalMessage::CloseConfirmed(id), Some(&a()));
        assert!(t.team_need("a").is_none());
    }

    #[test]
    fn revealing_a_terminal_from_another_strip_does_nothing() {
        let mut t = TerminalState::new();
        let id = TerminalId(1);
        t.roster.add(id, &Scope::team("b"), None);
        t.reveal(id, Some(&a()));
        assert!(!t.stored_panel().visible);
    }

    #[test]
    fn a_pinned_panel_shows_and_cannot_be_hidden_or_grown() {
        let mut t = TerminalState::new();
        t.restore_records(vec![record("k1", "gone")]);
        assert!(t.has_terminals("gone") && !t.has_terminals("other"));
        t.set_pinned(true);
        t.sync(Some(&Scope::team("gone")));
        assert!(t.stored_panel().visible, "shown although it was hidden");
        let _ = t.update(TerminalMessage::Hide, Some(&Scope::team("gone")));
        let _ = t.update(TerminalMessage::New, Some(&Scope::team("gone")));
        assert!(t.stored_panel().visible);
        assert_eq!(t.roster.ids_of_team("gone").len(), 1, "no new Terminal for a removed team");
    }
}
