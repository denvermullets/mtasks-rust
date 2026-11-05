//! Terminals: real shells with a plain-data API (ADR 0002).
//!
//! `Terminals` is the set the app holds, keyed by a local `TerminalId`. Each
//! Terminal runs three plain OS threads (owner, PTY reader, PTY writer) and
//! publishes its latest `Frame` to a shared slot; nothing runs on tokio. Only
//! `engine.rs` touches `libghostty-vt` and `portable-pty`, and no type from
//! either crate appears here.
//!
//! The UI learns about new frames through one shared "wake pending" flag: the
//! notify callback fires once, and fires again only after `ack_wake`, so a slow
//! UI drops frames instead of queueing messages.
// Parts of the API (paste, close, pid, ...) wait for later tickets.
#![allow(dead_code)]

mod engine;
mod input;
mod palette;
pub mod panel;
pub mod pointer;
mod state;
mod types;

pub use palette::{active_term_theme, term_theme};
pub use state::{panel_team, TerminalMessage, TerminalState};
pub use types::*;

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;

/// The shared "wake pending" flag. Owner threads call `fire`; the app calls `ack`.
pub(crate) struct Wake {
    pending: AtomicBool,
    notify: Box<dyn Fn() + Send + Sync>,
}

impl Wake {
    /// Notify the UI unless a notification it hasn't taken yet is outstanding.
    pub(crate) fn fire(&self) {
        if !self.pending.swap(true, Ordering::AcqRel) {
            (self.notify)();
        }
    }

    fn ack(&self) {
        self.pending.store(false, Ordering::Release);
    }
}

struct Entry {
    team: String,
    engine: engine::Engine,
}

/// A close in progress. `close` returns at once; `wait` blocks until the
/// process is reaped and all three threads have exited.
pub struct Closing {
    handle: JoinHandle<CloseReport>,
}

impl Closing {
    pub fn is_done(&self) -> bool {
        self.handle.is_finished()
    }

    pub fn wait(self) -> CloseReport {
        self.handle.join().unwrap_or(CloseReport {
            escalated_to_sigkill: false,
            threads_exited: 0,
            reaped: false,
        })
    }
}

pub struct Terminals {
    next_id: u64,
    entries: HashMap<TerminalId, Entry>,
    wake: Arc<Wake>,
}

impl Terminals {
    /// `notify` is called from an owner thread when a frame is ready and no
    /// earlier wake is still pending. It must be cheap and must not block.
    pub fn new(notify: impl Fn() + Send + Sync + 'static) -> Self {
        Terminals {
            next_id: 1,
            entries: HashMap::new(),
            wake: Arc::new(Wake {
                pending: AtomicBool::new(false),
                notify: Box::new(notify),
            }),
        }
    }

    /// The app has taken the wake; the next published frame may notify again.
    pub fn ack_wake(&self) {
        self.wake.ack();
    }

    /// Start a shell in a new Terminal.
    pub fn spawn(&mut self, config: SpawnConfig) -> std::io::Result<TerminalId> {
        let team = config.team.clone();
        let engine = engine::Engine::spawn(config, self.wake.clone())?;
        let id = TerminalId(self.next_id);
        self.next_id += 1;
        self.entries.insert(id, Entry { team, engine });
        Ok(id)
    }

    pub fn send_key(&self, id: TerminalId, key: KeyInput) {
        if let Some(e) = self.entries.get(&id) {
            e.engine.send(engine::Cmd::Key(key));
        }
    }

    /// A pointer event. It goes to the program only while the program tracks the mouse.
    pub fn mouse(&self, id: TerminalId, event: MouseInput) {
        if let Some(e) = self.entries.get(&id) {
            e.engine.send(engine::Cmd::Mouse(event));
        }
    }

    /// The text under the current selection, or `None` when nothing is selected.
    /// Soft-wrapped lines are joined and trailing blanks trimmed, as when copying
    /// from any terminal. This is what Cmd+C (JRST-386) copies. It asks the
    /// Terminal's owner thread, so it waits briefly (at most half a second).
    pub fn selected_text(&self, id: TerminalId) -> Option<String> {
        let e = self.entries.get(&id)?;
        let (tx, rx) = std::sync::mpsc::channel();
        e.engine.send(engine::Cmd::SelectedText(tx));
        rx.recv_timeout(std::time::Duration::from_millis(500)).ok().flatten()
    }

    /// Wheel or trackpad movement: goes to a program that tracks the mouse, as
    /// arrow keys to a full-screen program that doesn't, and otherwise scrolls
    /// back through output.
    pub fn wheel(&self, id: TerminalId, event: WheelInput) {
        if let Some(e) = self.entries.get(&id) {
            e.engine.send(engine::Cmd::Wheel(event));
        }
    }

    pub fn paste(&self, id: TerminalId, text: String) {
        if let Some(e) = self.entries.get(&id) {
            e.engine.send(engine::Cmd::Paste(text));
        }
    }

    /// Resize the grid and the PTY to the pane showing this Terminal.
    pub fn resize(&self, id: TerminalId, cols: u16, rows: u16, cell_w: f32, cell_h: f32) {
        if let Some(e) = self.entries.get(&id) {
            e.engine.send(engine::Cmd::Resize {
                cols,
                rows,
                cell_w,
                cell_h,
            });
        }
    }

    pub fn set_theme(&self, id: TerminalId, theme: TermTheme) {
        if let Some(e) = self.entries.get(&id) {
            e.engine.send(engine::Cmd::Theme(theme));
        }
    }

    /// A hidden Terminal keeps parsing but builds no frames and sends no wakes
    /// (except for its shell exiting). Showing it builds a full frame at once.
    pub fn set_visible(&self, id: TerminalId, visible: bool) {
        if let Some(e) = self.entries.get(&id) {
            e.engine.send(engine::Cmd::Visible(visible));
        }
    }

    /// The latest published frame.
    pub fn frame(&self, id: TerminalId) -> Option<Arc<Frame>> {
        self.entries.get(&id).map(|e| e.engine.frame())
    }

    /// The shell's process id, while the Terminal exists.
    pub fn pid(&self, id: TerminalId) -> Option<u32> {
        self.entries.get(&id).map(|e| e.engine.pid())
    }

    pub fn is_exited(&self, id: TerminalId) -> bool {
        self.entries.get(&id).is_none_or(|e| e.engine.exited())
    }

    /// How many of the team's Terminals have a live shell.
    pub fn open_count(&self, team: &str) -> usize {
        self.entries
            .values()
            .filter(|e| e.team == team && !e.engine.exited())
            .count()
    }

    pub fn ids(&self) -> Vec<TerminalId> {
        let mut ids: Vec<_> = self.entries.keys().copied().collect();
        ids.sort();
        ids
    }

    /// Stop a Terminal: SIGHUP to the shell and its foreground process group,
    /// SIGKILL after 2 seconds, reap the child, and end all three threads.
    /// Returns at once; the Terminal is gone from the set immediately.
    pub fn close(&mut self, id: TerminalId) -> Option<Closing> {
        let entry = self.entries.remove(&id)?;
        let handle = std::thread::Builder::new()
            .name("term-close".into())
            .spawn(move || entry.engine.shutdown())
            .ok()?;
        Some(Closing { handle })
    }

    /// Test hook: make the owner thread panic.
    #[cfg(test)]
    fn crash_owner(&self, id: TerminalId) {
        if let Some(e) = self.entries.get(&id) {
            e.engine.send(engine::Cmd::Panic);
        }
    }
}

#[cfg(test)]
mod tests;
