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
use super::{active_term_theme, MouseAction, MouseInput, SpawnConfig, TerminalId, Terminals};
use crate::app::Message;
use crate::theme as t;
use crate::views::terminal_panel::{self, Pane, PanelView};

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
    /// A pointer event over the pane.
    Mouse(MouseInput),
    /// "New Terminal" on an empty panel.
    New,
    Key(Physical, keyboard::Modifiers, Option<String>),
    Pasted(Option<String>),
}

static WAKE_RX: OnceLock<Mutex<Option<UnboundedReceiver<()>>>> = OnceLock::new();

pub struct TerminalState {
    terminals: Terminals,
    owners: HashMap<TerminalId, String>,
    names: HashMap<TerminalId, String>,
    panel: PanelState,
    focused: bool,
    window: Size,
    cell: Option<(f32, f32)>,
    pane: Pane,
    shown: Option<TerminalId>,
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
            owners: HashMap::new(),
            names: HashMap::new(),
            panel: PanelState::default(),
            focused: false,
            window: Size::new(1100.0, 700.0),
            cell: None,
            pane: Pane::new(),
            shown: None,
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
                            self.create(team);
                        }
                    }
                    Toggle::SetFocus(f) => self.focused = f,
                }
            }
            TerminalMessage::Focus => self.focused = true,
            TerminalMessage::Mouse(m) => {
                if matches!(m.action, MouseAction::Press(_)) {
                    self.focused = true;
                }
                if let Some(id) = self.shown {
                    self.terminals.mouse(id, m);
                }
            }
            TerminalMessage::New => {
                if let Some(team) = team {
                    self.panel.visible = true;
                    self.focused = true;
                    self.create(team);
                }
            }
            TerminalMessage::Key(Physical::Code(code), mods, text) => {
                match input::route(code, mods, text.as_deref()) {
                    Route::ToggleFocus => return self.update(TerminalMessage::ToggleFocus, team),
                    Route::Palette => return Task::done(Message::OpenCommandPalette),
                    Route::Paste => {
                        return iced::clipboard::read()
                            .map(|s| Message::Terminal(TerminalMessage::Pasted(s)))
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
        self.pane.set_focused(self.focused);
    }

    /// Whether key presses go to the Terminal instead of the app's shortcuts.
    pub fn takes_keys(&self, modal_open: bool) -> bool {
        self.focused && self.panel.visible && !modal_open
    }

    /// The panel for the current team, if it is showing.
    pub fn panel_view(&self, team: Option<&str>) -> Option<Element<'_, Message>> {
        let team = team?;
        if !self.panel.visible {
            return None;
        }
        let pane = self.shown.map(|_| &self.pane);
        let title = self
            .shown
            .and_then(|id| self.names.get(&id).cloned())
            .unwrap_or_else(|| "Terminals".to_string());
        let note = self
            .error
            .clone()
            .or_else(|| self.pane_exited().then(|| "shell exited".to_string()))
            .or_else(|| (pane.is_none() && self.owned_by(team).is_empty()).then(|| "none open".to_string()));
        Some(terminal_panel::view(PanelView {
            pane,
            title,
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
        if self.takes_keys(modal_open) {
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

    fn owned_by(&self, team: &str) -> Vec<TerminalId> {
        self.terminals
            .ids()
            .into_iter()
            .filter(|id| self.owners.get(id).is_some_and(|o| o == team))
            .collect()
    }

    fn pane_exited(&self) -> bool {
        self.shown.is_some_and(|id| self.terminals.is_exited(id))
    }

    fn grid(&self, cell: (f32, f32)) -> (u16, u16) {
        let (w, h) = panel::pane_pixels(self.window.width, self.window.height, self.panel.height);
        panel::grid_for(w, h, cell.0, cell.1)
    }

    fn create(&mut self, team: &str) {
        let cell = *self.cell.get_or_insert_with(terminal_panel::measure_cell);
        self.pane.set_cell(cell);
        let (cols, rows) = self.grid(cell);
        let config = SpawnConfig {
            team: team.to_string(),
            cols,
            rows,
            theme: active_term_theme(),
            cwd: None,
            program: None,
        };
        match self.terminals.spawn(config) {
            Ok(id) => {
                self.error = None;
                self.owners.insert(id, team.to_string());
                let n = self.owned_by(team).len();
                self.names.insert(id, format!("Terminal {n}"));
                self.sent_size.insert(id, (cols, rows));
                self.panel.pane = Some(id);
            }
            Err(e) => self.error = Some(format!("could not start a shell: {e}")),
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
