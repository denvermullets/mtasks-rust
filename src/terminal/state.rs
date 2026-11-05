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
use super::links::Link;
use super::panel::{self, Layout, PanelState, Toggle, MAX_PANES};
use super::roster::Roster;
use super::{
    active_term_theme, Closing, MouseAction, MouseInput, SpawnConfig, TerminalId, Terminals,
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
    /// The pointer pressed the strip.
    DragStart,
    /// The pointer moved (window y) while dragging.
    DragMove(f32),
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
    /// Focus the next or previous pane (Cmd+] and Cmd+[, JRST-405).
    CyclePane(i32),
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
    panes: [Pane; MAX_PANES],
    /// What each pane shows now: `panel.panes` reconciled with the team.
    shown: panel::Panes,
    sent_visible: HashMap<TerminalId, bool>,
    sent_size: HashMap<TerminalId, (u16, u16)>,
    theme_key: &'static str,
    error: Option<String>,
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
            panes: std::array::from_fn(Pane::for_slot),
            shown: [None; MAX_PANES],
            sent_visible: HashMap::new(),
            sent_size: HashMap::new(),
            theme_key: t::tok().key,
            error: None,
        }
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
                            self.create(team, self.panel.focused_pane);
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
                    let slot = panel::slot_for_new(&self.shown, self.panel.layout, self.panel.focused_pane);
                    self.create(team, slot);
                }
            }
            TerminalMessage::NewIn(slot) => {
                if let Some(team) = team {
                    self.focused = true;
                    self.create(team, slot);
                }
            }
            TerminalMessage::Select(id) => {
                self.renaming = None;
                if team.is_some_and(|t| self.owned_by(t).contains(&id)) {
                    self.place(self.panel.focused_pane, id);
                    self.focused = true;
                }
            }
            TerminalMessage::Pick(slot, id) => {
                self.renaming = None;
                if team.is_some_and(|t| self.owned_by(t).contains(&id)) {
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
                        if let Some(id) = self.focused_terminal() {
                            self.terminals.send_key(id, key);
                        }
                    }
                    Route::Ignore => {}
                }
            }
            TerminalMessage::Key(..) => {}
            TerminalMessage::Pasted(Some(text)) => {
                if let Some(id) = self.focused_terminal() {
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
        let layout = self.panel.layout;
        let shown = match team {
            Some(t) => panel::reconcile(&self.panel.panes, &self.owned_by(t), layout),
            None => [None; MAX_PANES],
        };
        for slot in 0..MAX_PANES {
            if shown[slot] != self.shown[slot] {
                self.panes[slot].reset();
            }
        }
        self.shown = shown;
        if team.is_some() {
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
        let layout = self.panel.layout;
        let focused_id = self.focused_terminal();
        let owned = self.owned_by(team);
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
                        panel::pane_notice(self.terminals.is_exited(id), dead).map(|n| (id, n))
                    }),
                }
            })
            .collect();
        let items = self
            .roster
            .of_team(team)
            .map(|r| StripItem {
                id: r.id,
                name: r.name.clone(),
                active: Some(r.id) == focused_id,
                exited: self.terminals.is_exited(r.id),
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
            dir_label: self.team_dirs.get(team).map(|d| {
                d.file_name().map_or_else(|| d.display().to_string(), |n| n.to_string_lossy().into_owned())
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

    /// How many of the team's Terminals have a live shell.
    pub fn open_count(&self, team: &str) -> usize {
        self.terminals.open_count(team)
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
        let (w, h) = panel::pane_size(
            self.window.width,
            self.window.height,
            self.panel.height,
            self.panel.layout,
        );
        panel::grid_for(w, h, cell.0, cell.1)
    }

    fn restart(&mut self, id: TerminalId) {
        let Some(rec) = self.roster.get(id).cloned() else { return };
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

    /// Start a Terminal for `team` in pane `slot`.
    fn create(&mut self, team: &str, slot: usize) {
        let cwd = self.team_dirs.get(team).cloned();
        if let Some(id) = self.spawn(team, cwd.clone()) {
            self.roster.add(id, team, cwd);
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
