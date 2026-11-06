//! The one file that touches `libghostty-vt` and `portable-pty` (ADR 0002).
//!
//! One Terminal = three plain OS threads:
//! - owner: holds the `!Send` libghostty types, applies commands, publishes frames;
//! - PTY reader: polls the PTY, with a 4 MB limit on output the owner hasn't parsed;
//! - PTY writer: writes to the PTY, so a child that stops reading can't stall emulation.
//!
//! Ported from the JRST-328 spike. Nothing here runs on tokio.
use super::palette::{min_contrast, CONTRAST_FLOOR};
use super::*;
use libghostty_vt::render::{CellIterator, CursorVisualStyle, Dirty, RowIterator};
use libghostty_vt::screen::CellWide;
use libghostty_vt::style::{PaletteIndex, RgbColor};
use libghostty_vt::terminal::{ColorScheme, Mode, ScrollViewport};
use libghostty_vt::fmt::Format;
use libghostty_vt::selection::gesture::{
    Autoscroll, AutoscrollTickEvent, DragEvent, Geometry as SelectionGeometry, Gesture,
    PressEvent, ReleaseEvent,
};
use libghostty_vt::selection::FormatOptions;
use libghostty_vt::terminal::{Point, PointCoordinate};
use libghostty_vt::{key, mouse, paste, RenderState, Terminal};
use portable_pty::{native_pty_system, Child, CommandBuilder, MasterPty, PtySize};
use std::io::{Read, Write};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering::*};
use std::sync::mpsc::{channel, Receiver, RecvTimeoutError, Sender};
use std::sync::Mutex;
use std::time::{Duration, Instant};

fn rgb(c: RgbColor) -> Rgb {
    [c.r, c.g, c.b]
}

pub(super) enum Cmd {
    Output(Vec<u8>),
    Exited,
    Key(KeyInput),
    Paste(String),
    Mouse(MouseInput),
    Wheel(WheelInput),
    SelectedText(Sender<Option<String>>),
    Resize { cols: u16, rows: u16, cell_w: f32, cell_h: f32 },
    Theme(TermTheme),
    Visible(bool),
    Shutdown,
    #[cfg(test)]
    Panic,
    #[cfg(test)]
    ScrollTop,
}

/// What the owner thread shares with the API side.
struct Shared {
    frame: Mutex<Arc<Frame>>,
    exited: AtomicBool,
}

pub(super) struct Engine {
    tx: Sender<Cmd>,
    shared: Arc<Shared>,
    pid: u32,
    child: Box<dyn Child + Send + Sync>,
    master: Arc<Mutex<Box<dyn MasterPty + Send>>>,
    /// Lets `shutdown` read the master even when the reader thread is gone.
    drain_fd: OwnedFd,
    /// Writing a byte here wakes the reader out of `poll` so it can exit.
    stop_w: OwnedFd,
    stop: Arc<AtomicBool>,
    threads: Vec<std::thread::JoinHandle<()>>,
}

/// Back-pressure: at most 4 MB of output the owner hasn't parsed yet.
const MAX_INFLIGHT: u64 = 4 << 20;
/// How long a shell gets to obey SIGHUP before SIGKILL.
const CLOSE_GRACE: Duration = Duration::from_secs(2);
/// How long to keep waiting for the shell to be reaped after SIGKILL.
const KILL_GRACE: Duration = Duration::from_secs(5);
const OPENPTY_RETRIES: u32 = 8;
const FRAME: Duration = Duration::from_millis(16);

fn dup_cloexec(fd: RawFd) -> std::io::Result<OwnedFd> {
    let n = unsafe { libc::fcntl(fd, libc::F_DUPFD_CLOEXEC, 3) };
    if n < 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(unsafe { OwnedFd::from_raw_fd(n) })
}

/// `open("/dev/ptmx")` fails now and then with errno -6 (not a real errno) when other
/// threads or processes open PTYs at the same moment. The same call succeeds a moment
/// later, so retry a few times before reporting an error.
fn open_pty_with_retry(size: PtySize) -> Result<portable_pty::PtyPair, anyhow_like::Error> {
    let mut attempt = 0;
    loop {
        match native_pty_system().openpty(size) {
            Ok(pair) => return Ok(pair),
            Err(e) if attempt < OPENPTY_RETRIES => {
                attempt += 1;
                std::thread::sleep(Duration::from_millis(5 * attempt as u64));
                drop(e);
            }
            Err(e) => return Err(e.into()),
        }
    }
}

/// Throw away whatever the shell has written that nobody has read. Never blocks.
fn drain_master(fd: RawFd) {
    let mut buf = [0u8; 16 * 1024];
    loop {
        let mut p = libc::pollfd { fd, events: libc::POLLIN, revents: 0 };
        if unsafe { libc::poll(&mut p, 1, 0) } <= 0 || p.revents & libc::POLLIN == 0 {
            return;
        }
        if unsafe { libc::read(fd, buf.as_mut_ptr().cast(), buf.len()) } <= 0 {
            return;
        }
    }
}

fn cloexec_pipe() -> std::io::Result<(OwnedFd, OwnedFd)> {
    let mut fds = [0 as libc::c_int; 2];
    if unsafe { libc::pipe(fds.as_mut_ptr()) } < 0 {
        return Err(std::io::Error::last_os_error());
    }
    for fd in fds {
        unsafe { libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC) };
    }
    Ok(unsafe { (OwnedFd::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1])) })
}

/// Owns a freshly started shell until `Engine` takes it over. Any failure
/// between the two drops the guard, which kills and reaps the shell so a failed
/// spawn leaves no orphan.
struct ChildGuard {
    child: Option<Box<dyn Child + Send + Sync>>,
    /// Its own copy of the master, so the guard can drain output while it reaps.
    drain: OwnedFd,
}

impl ChildGuard {
    fn child(&self) -> &(dyn Child + Send + Sync) {
        self.child.as_deref().expect("guard holds the child until disarmed")
    }

    /// Hand the shell over: it is `Engine`'s to shut down now.
    fn disarm(mut self) -> Box<dyn Child + Send + Sync> {
        self.child.take().expect("guard holds the child until disarmed")
    }
}

impl Drop for ChildGuard {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            if let Some(pid) = child.process_id() {
                // The shell leads its own session; take its foreground jobs too.
                unsafe { libc::killpg(pid as libc::pid_t, libc::SIGKILL) };
            }
            let _ = child.kill();
            // Never a blocking wait: macOS holds an exiting process until its
            // PTY output drains, and nothing else reads the master yet.
            let deadline = Instant::now() + KILL_GRACE;
            loop {
                drain_master(self.drain.as_raw_fd());
                match child.try_wait() {
                    Ok(Some(_)) | Err(_) => break,
                    Ok(None) if Instant::now() >= deadline => break,
                    Ok(None) => std::thread::sleep(Duration::from_millis(10)),
                }
            }
        }
    }
}

/// Test seam: lets a test fail the setup that follows a successful start.
#[cfg(test)]
pub(super) mod seam {
    use std::cell::Cell;
    thread_local! {
        pub static FAIL_AFTER_START: Cell<bool> = const { Cell::new(false) };
        pub static LAST_PID: Cell<u32> = const { Cell::new(0) };
        /// Lets the shell write before the injected failure, so its output sits unread.
        pub static DELAY_AFTER_START_MS: Cell<u64> = const { Cell::new(0) };
    }
}

fn after_start(pid: u32) -> std::io::Result<()> {
    #[cfg(test)]
    {
        seam::LAST_PID.with(|p| p.set(pid));
        let delay = seam::DELAY_AFTER_START_MS.with(|d| d.get());
        if delay > 0 {
            std::thread::sleep(Duration::from_millis(delay));
        }
        if seam::FAIL_AFTER_START.with(|f| f.get()) {
            return Err(std::io::Error::other("injected failure after the shell started"));
        }
    }
    let _ = pid;
    Ok(())
}

impl Engine {
    pub(super) fn spawn(config: SpawnConfig, wake: Arc<Wake>) -> std::io::Result<Engine> {
        let io = |e: anyhow_like::Error| std::io::Error::other(e.0);
        let size = PtySize { rows: config.rows, cols: config.cols, pixel_width: 0, pixel_height: 0 };
        let pty = open_pty_with_retry(size).map_err(|e| io(e.into()))?;
        let master_fd = pty.master.as_raw_fd().ok_or_else(|| std::io::Error::other("no pty fd"))?;
        // portable-pty doesn't set IUTF8; without it the line editor erases bytes, not characters.
        unsafe {
            let mut t: libc::termios = std::mem::zeroed();
            if libc::tcgetattr(master_fd, &mut t) == 0 {
                t.c_iflag |= libc::IUTF8;
                libc::tcsetattr(master_fd, libc::TCSANOW, &t);
            }
        }
        let mut cmd = match &config.program {
            Some((prog, args)) => {
                let mut c = CommandBuilder::new(prog);
                c.args(args);
                c
            }
            None => CommandBuilder::new_default_prog(),
        };
        cmd.env("TERM", "xterm-256color");
        cmd.env("COLORTERM", "truecolor");
        cmd.env("TERM_PROGRAM", "jait");
        let home = std::env::var_os("HOME").map(std::path::PathBuf::from);
        match config.cwd.filter(|p| p.is_dir()).or(home) {
            Some(dir) => cmd.cwd(dir),
            None => {}
        }
        let guard_fd = dup_cloexec(master_fd)?;
        let child = ChildGuard {
            child: Some(pty.slave.spawn_command(cmd).map_err(|e| io(e.into()))?),
            drain: guard_fd,
        };
        drop(pty.slave);
        let pid = child.child().process_id().unwrap_or(0);
        after_start(pid)?;
        let master = Arc::new(Mutex::new(pty.master));

        let read_fd = dup_cloexec(master_fd)?;
        let write_fd = dup_cloexec(master_fd)?;
        let drain_fd = dup_cloexec(master_fd)?;
        let (stop_r, stop_w) = cloexec_pipe()?;
        let stop = Arc::new(AtomicBool::new(false));
        let shared = Arc::new(Shared {
            frame: Mutex::new(Arc::new(Frame::default())),
            exited: AtomicBool::new(false),
        });
        let inflight = Arc::new(AtomicU64::new(0));
        let (tx, rx) = channel::<Cmd>();
        let (wtx, wrx) = channel::<Vec<u8>>();
        let mut threads = Vec::with_capacity(3);

        threads.push(
            std::thread::Builder::new().name("term-write".into()).spawn(move || {
                let mut file = std::fs::File::from(write_fd);
                while let Ok(b) = wrx.recv() {
                    if file.write_all(&b).is_err() {
                        break;
                    }
                }
            })?,
        );
        {
            let (tx, inflight, stop) = (tx.clone(), inflight.clone(), stop.clone());
            threads.push(
                std::thread::Builder::new()
                    .name("term-read".into())
                    .spawn(move || reader(read_fd, stop_r, tx, inflight, stop))?,
            );
        }
        {
            let (shared, master, theme) = (shared.clone(), master.clone(), config.theme);
            let (cols, rows) = (config.cols, config.rows);
            threads.push(std::thread::Builder::new().name("term-owner".into()).spawn(move || {
                let guard = shared.clone();
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    owner(cols, rows, theme, rx, wtx, master, shared, inflight, wake.clone());
                }));
                if result.is_err() {
                    // Only this Terminal dies: publish a dead frame and let `close` clean up.
                    let mut f = guard.frame.lock().unwrap_or_else(|p| p.into_inner());
                    let mut dead = (**f).clone();
                    dead.dead = true;
                    dead.exited = true;
                    *f = Arc::new(dead);
                    drop(f);
                    guard.exited.store(true, Release);
                    wake.fire();
                }
            })?);
        }
        Ok(Engine { tx, shared, pid, child: child.disarm(), master, drain_fd, stop_w, stop, threads })
    }

    pub(super) fn send(&self, cmd: Cmd) {
        let _ = self.tx.send(cmd);
    }

    pub(super) fn frame(&self) -> Arc<Frame> {
        self.shared.frame.lock().unwrap_or_else(|p| p.into_inner()).clone()
    }

    pub(super) fn pid(&self) -> u32 {
        self.pid
    }

    /// The PTY's foreground process group isn't the shell's own.
    pub(super) fn busy(&self) -> bool {
        let fg = self.master.lock().ok().and_then(|m| m.process_group_leader());
        fg.is_some_and(|fg| fg != self.pid as libc::pid_t)
    }

    pub(super) fn exited(&self) -> bool {
        self.shared.exited.load(Acquire)
    }

    /// SIGHUP the shell and its foreground process group, SIGKILL them if the shell
    /// is still there after 2 seconds, reap it, and end all three threads.
    pub(super) fn shutdown(mut self) -> CloseReport {
        let fg = self.master.lock().ok().and_then(|m| m.process_group_leader());
        let pid = self.pid as libc::pid_t;
        let signal_all = |sig: libc::c_int| unsafe {
            if let Some(fg) = fg {
                libc::killpg(fg, sig);
            }
            libc::killpg(pid, sig);
        };
        signal_all(libc::SIGHUP);
        signal_all(libc::SIGCONT);
        let mut deadline = Instant::now() + CLOSE_GRACE;
        let mut escalated = false;
        let mut reaped = true;
        loop {
            // macOS keeps an exiting process in the kernel until the PTY's output
            // queue drains into the master, so keep reading while we wait.
            drain_master(self.drain_fd.as_raw_fd());
            match self.child.try_wait() {
                Ok(Some(_)) | Err(_) => break,
                Ok(None) => {}
            }
            if Instant::now() >= deadline {
                if !escalated {
                    escalated = true;
                    signal_all(libc::SIGKILL);
                    unsafe { libc::kill(pid, libc::SIGKILL) };
                    deadline = Instant::now() + KILL_GRACE;
                } else {
                    // Never block the app on a process the kernel won't release.
                    reaped = false;
                    break;
                }
            }
            std::thread::sleep(Duration::from_millis(10));
        }

        // Dropping isn't enough: the owner and reader hold channel senders, so tell
        // each thread to stop and join them.
        self.stop.store(true, Release);
        let _ = self.tx.send(Cmd::Shutdown);
        unsafe { libc::write(self.stop_w.as_raw_fd(), [1u8].as_ptr().cast(), 1) };
        drop(self.tx);
        let threads_exited = self.threads.drain(..).filter_map(|t| t.join().ok()).count();
        CloseReport { escalated_to_sigkill: escalated, threads_exited, reaped }
    }
}

mod anyhow_like {
    /// portable-pty errors are `anyhow::Error`; keep that crate out of our imports.
    pub struct Error(pub String);
    impl<E: std::fmt::Display> From<E> for Error {
        fn from(e: E) -> Self {
            Error(e.to_string())
        }
    }
}

fn reader(fd: OwnedFd, stop_r: OwnedFd, tx: Sender<Cmd>, inflight: Arc<AtomicU64>, stop: Arc<AtomicBool>) {
    let mut file = std::fs::File::from(fd);
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        while inflight.load(Relaxed) > MAX_INFLIGHT {
            if stop.load(Acquire) {
                return;
            }
            std::thread::sleep(Duration::from_micros(500));
        }
        let mut fds = [
            libc::pollfd { fd: file.as_raw_fd(), events: libc::POLLIN, revents: 0 },
            libc::pollfd { fd: stop_r.as_raw_fd(), events: libc::POLLIN, revents: 0 },
        ];
        let n = unsafe { libc::poll(fds.as_mut_ptr(), 2, -1) };
        if n < 0 {
            if std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            let _ = tx.send(Cmd::Exited);
            return;
        }
        if fds[1].revents != 0 || stop.load(Acquire) {
            return;
        }
        if fds[0].revents == 0 {
            continue;
        }
        match file.read(&mut buf) {
            Ok(0) | Err(_) => {
                let _ = tx.send(Cmd::Exited);
                return;
            }
            Ok(n) => {
                inflight.fetch_add(n as u64, Relaxed);
                if tx.send(Cmd::Output(buf[..n].to_vec())).is_err() {
                    return;
                }
            }
        }
    }
}

fn map_key(k: TermKey) -> (key::Key, char) {
    use key::Key as K;
    let ch = |c: char| -> (K, char) {
        let lower = c.to_ascii_lowercase();
        let k = match lower {
            'a'..='z' => [
                K::A, K::B, K::C, K::D, K::E, K::F, K::G, K::H, K::I, K::J, K::K, K::L, K::M,
                K::N, K::O, K::P, K::Q, K::R, K::S, K::T, K::U, K::V, K::W, K::X, K::Y, K::Z,
            ][(lower as u8 - b'a') as usize],
            '0'..='9' => [
                K::Digit0, K::Digit1, K::Digit2, K::Digit3, K::Digit4, K::Digit5, K::Digit6,
                K::Digit7, K::Digit8, K::Digit9,
            ][(lower as u8 - b'0') as usize],
            '`' => K::Backquote,
            '\\' => K::Backslash,
            '[' => K::BracketLeft,
            ']' => K::BracketRight,
            ',' => K::Comma,
            '=' => K::Equal,
            '-' => K::Minus,
            '.' => K::Period,
            '\'' => K::Quote,
            ';' => K::Semicolon,
            '/' => K::Slash,
            _ => K::Unidentified,
        };
        (k, lower)
    };
    match k {
        TermKey::Char(c) => ch(c),
        TermKey::Space => (K::Space, ' '),
        TermKey::Enter => (K::Enter, '\0'),
        TermKey::Escape => (K::Escape, '\0'),
        TermKey::Backspace => (K::Backspace, '\0'),
        TermKey::Tab => (K::Tab, '\0'),
        TermKey::Delete => (K::Delete, '\0'),
        TermKey::Insert => (K::Insert, '\0'),
        TermKey::Home => (K::Home, '\0'),
        TermKey::End => (K::End, '\0'),
        TermKey::PageUp => (K::PageUp, '\0'),
        TermKey::PageDown => (K::PageDown, '\0'),
        TermKey::ArrowUp => (K::ArrowUp, '\0'),
        TermKey::ArrowDown => (K::ArrowDown, '\0'),
        TermKey::ArrowLeft => (K::ArrowLeft, '\0'),
        TermKey::ArrowRight => (K::ArrowRight, '\0'),
        TermKey::F(n) => (
            [K::F1, K::F2, K::F3, K::F4, K::F5, K::F6, K::F7, K::F8, K::F9, K::F10, K::F11, K::F12]
                .get((n as usize).wrapping_sub(1))
                .copied()
                .unwrap_or(K::Unidentified),
            '\0',
        ),
    }
}

/// How often a drag held outside the pane scrolls it by a row.
const AUTOSCROLL: Duration = Duration::from_millis(50);
/// Two presses this close in time (and space) count as a double click.
const REPEAT_CLICK: Duration = Duration::from_millis(500);

/// Mouse-driven selection, using the engine's selection gestures: a click clears,
/// a drag selects, a double click selects a word and a triple click a line. The
/// engine owns the selection; it follows the text as output scrolls.
struct Selector {
    gesture: Gesture<'static>,
    press: PressEvent<'static>,
    drag: DragEvent<'static>,
    release: ReleaseEvent<'static>,
    tick: AutoscrollTickEvent<'static>,
    start: Instant,
    active: bool,
    scroll: Autoscroll,
    /// Where the pointer last was, for the timed scroll.
    pos: (i32, i32),
    /// Cell the drag began on.
    anchor: (i32, i32),
    last_tick: Instant,
}

impl Selector {
    fn new() -> Self {
        Selector {
            gesture: Gesture::new().unwrap(),
            press: PressEvent::new().unwrap(),
            drag: DragEvent::new().unwrap(),
            release: ReleaseEvent::new().unwrap(),
            tick: AutoscrollTickEvent::new().unwrap(),
            start: Instant::now(),
            active: false,
            scroll: Autoscroll::None,
            pos: (0, 0),
            anchor: (0, 0),
            last_tick: Instant::now(),
        }
    }

    /// A drag is being held outside the pane, so the viewport needs timed scrolling.
    fn autoscrolling(&self) -> bool {
        self.active && self.scroll != Autoscroll::None
    }

    fn geometry(sz: (u16, u16, u32, u32)) -> SelectionGeometry {
        SelectionGeometry {
            columns: sz.0 as u32,
            cell_width: sz.2,
            padding_left: 0,
            screen_height: sz.1 as u32 * sz.3,
        }
    }

    fn coord(sz: (u16, u16, u32, u32), col: i32, row: i32) -> PointCoordinate {
        PointCoordinate {
            x: col.clamp(0, sz.0 as i32 - 1) as u16,
            y: row.clamp(0, sz.1 as i32 - 1) as u32,
        }
    }

    /// Surface position inside a cell, `frac` of the way across it; outside the
    /// pane for a drag that left it. The engine includes a cell in a selection only
    /// when the pointer is on the near side of its edge, so the start of a drag sits
    /// at the cell's left (frac 0.25) and an end past the start at its right (0.75).
    fn pixel(sz: (u16, u16, u32, u32), col: i32, row: i32, frac: f64) -> (f64, f64) {
        ((col as f64 + frac) * sz.2 as f64, (row as f64 + 0.5) * sz.3 as f64)
    }

    /// Which side of its cell the pointer should count as, given where the drag began.
    /// Still on the anchor cell it is where the press was, so a click whose pointer
    /// jitters within the cell selects nothing rather than that one cell.
    fn end_frac(&self, col: i32, row: i32) -> f64 {
        if (row, col) == (self.anchor.1, self.anchor.0) {
            0.25
        } else if (row, col) > (self.anchor.1, self.anchor.0) {
            0.75
        } else {
            0.25
        }
    }

    fn handle(&mut self, term: &Terminal<'static, 'static>, m: MouseInput, sz: (u16, u16, u32, u32)) {
        let frac = match m.action {
            MouseAction::Press(_) => 0.25,
            _ => self.end_frac(m.col, m.row),
        };
        let (px, py) = Self::pixel(sz, m.col, m.row, frac);
        let at = Point::Viewport(Self::coord(sz, m.col, m.row));
        match m.action {
            MouseAction::Press(MouseButton::Left) => {
                let Ok(r) = term.grid_ref(at) else { return };
                let _ = self.press.set_position(px, py);
                let _ = self.press.set_repeat_distance(sz.2 as f64);
                let _ = self.press.set_time(self.start.elapsed());
                let _ = self.press.set_repeat_interval(REPEAT_CLICK);
                // A plain click yields no selection, which also clears the old one.
                let sel = self.press.apply(&mut self.gesture, term, r).ok().flatten();
                let _ = term.set_selection(sel.as_ref());
                self.active = true;
                self.anchor = (m.col, m.row);
                self.scroll = Autoscroll::None;
            }
            MouseAction::Move if self.active => {
                self.pos = (m.col, m.row);
                let Ok(r) = term.grid_ref(at) else { return };
                let _ = self.drag.set_position(px, py);
                let sel = self.drag.apply(&mut self.gesture, term, r, Self::geometry(sz)).ok().flatten();
                if sel.is_some() {
                    let _ = term.set_selection(sel.as_ref());
                }
                self.scroll = self.gesture.autoscroll(term).unwrap_or(Autoscroll::None);
                self.last_tick = Instant::now();
            }
            MouseAction::Release(MouseButton::Left) if self.active => {
                let r = term.grid_ref(at).ok();
                let _ = self.release.apply(&mut self.gesture, term, r);
                self.active = false;
                self.scroll = Autoscroll::None;
            }
            _ => {}
        }
    }

    /// Scroll a row toward where the held drag is and extend the selection.
    fn autoscroll(&mut self, term: &mut Terminal<'static, 'static>, sz: (u16, u16, u32, u32)) {
        let pos = self.pos;
        let delta = match self.scroll {
            Autoscroll::Up => -1,
            Autoscroll::Down => 1,
            Autoscroll::None => return,
            _ => return,
        };
        term.scroll_viewport(ScrollViewport::Delta(delta));
        let (px, py) = Self::pixel(sz, pos.0, pos.1, self.end_frac(pos.0, pos.1));
        let _ = self.tick.set_position(px, py);
        let sel = self
            .tick
            .apply(&mut self.gesture, term, Self::coord(sz, pos.0, pos.1), Self::geometry(sz))
            .ok()
            .flatten();
        if sel.is_some() {
            let _ = term.set_selection(sel.as_ref());
        }
        self.last_tick = Instant::now();
    }
}

/// Encoder options from the terminal's current mouse modes and cell size.
fn sync_mouse(
    e: &mut mouse::Encoder<'_>,
    term: &Terminal<'_, '_>,
    size: (u16, u16, u32, u32),
    any_pressed: bool,
) {
    let (cols, rows, cw, ch) = size;
    e.set_options_from_terminal(term)
        .set_size(mouse::EncoderSize {
            screen_width: cols as u32 * cw,
            screen_height: rows as u32 * ch,
            cell_width: cw,
            cell_height: ch,
            padding_top: 0,
            padding_bottom: 0,
            padding_left: 0,
            padding_right: 0,
        })
        .set_any_button_pressed(any_pressed)
        .set_track_last_cell(true);
}

fn map_button(b: MouseButton) -> mouse::Button {
    match b {
        MouseButton::Left => mouse::Button::Left,
        MouseButton::Middle => mouse::Button::Middle,
        MouseButton::Right => mouse::Button::Right,
    }
}

fn map_mods(m: KeyMods) -> key::Mods {
    let mut out = key::Mods::empty();
    if m.shift {
        out |= key::Mods::SHIFT;
    }
    if m.alt {
        out |= key::Mods::ALT;
    }
    if m.ctrl {
        out |= key::Mods::CTRL;
    }
    if m.logo {
        out |= key::Mods::SUPER;
    }
    out
}

#[allow(clippy::too_many_arguments)]
fn owner(
    cols: u16,
    rows: u16,
    theme: TermTheme,
    rx: Receiver<Cmd>,
    wtx: Sender<Vec<u8>>,
    master: Arc<Mutex<Box<dyn MasterPty + Send>>>,
    shared: Arc<Shared>,
    inflight: Arc<AtomicU64>,
    wake: Arc<Wake>,
) {
    let size = Arc::new(Mutex::new((cols, rows, 8u32, 16u32)));
    let light = Arc::new(AtomicBool::new(theme.light));
    let mut term: Terminal<'static, 'static> = Terminal::new(cols, rows).unwrap();
    term.set_scrollback_max_lines(Some(10_000)).unwrap();
    // Both limits apply; the default byte limit otherwise caps history at about 1,100 rows.
    term.set_scrollback_max_bytes(Some(64 * 1024 * 1024)).unwrap();
    term.set_terminfo_name("xterm-256color").unwrap();
    {
        let w = wtx.clone();
        term.on_pty_write(move |_t, d| {
            let _ = w.send(d.to_vec());
        })
        .unwrap();
        let s = size.clone();
        term.on_size(move |_t| {
            let (columns, rows, cell_width, cell_height) = *s.lock().unwrap();
            Some(libghostty_vt::terminal::SizeReportSize { rows, columns, cell_width, cell_height })
        })
        .unwrap();
        term.on_device_attributes(|_t| {
            use libghostty_vt::terminal::*;
            Some(DeviceAttributes {
                primary: PrimaryDeviceAttributes::new(
                    ConformanceLevel::VT220,
                    &[
                        DeviceAttributeFeature::COLUMNS_132,
                        DeviceAttributeFeature::SELECTIVE_ERASE,
                        DeviceAttributeFeature::ANSI_COLOR,
                    ],
                ),
                secondary: SecondaryDeviceAttributes {
                    device_type: DeviceType::VT220,
                    firmware_version: 1,
                    rom_cartridge: 0,
                },
                tertiary: Default::default(),
            })
        })
        .unwrap();
        term.on_xtversion(|_t| Some("jait")).unwrap();
        // `CSI ?996n`: answered from the current theme, which `Cmd::Theme` updates.
        let l = light.clone();
        term.on_color_scheme(move |_t| {
            Some(if l.load(Relaxed) { ColorScheme::Light } else { ColorScheme::Dark })
        })
        .unwrap();
    }
    apply_theme(&mut term, &theme);

    let mut rs = RenderState::new().unwrap();
    let mut row_it = RowIterator::new().unwrap();
    let mut cell_it = CellIterator::new().unwrap();
    let mut key_enc = key::Encoder::new().unwrap();
    let mut key_ev = key::Event::new().unwrap();
    let mut mouse_enc = mouse::Encoder::new().unwrap();
    let mut mouse_ev = mouse::Event::new().unwrap();
    let mut held: Option<MouseButton> = None;
    let mut grab: Option<pointer::PointerTarget> = None;
    let mut wheel_acc = pointer::WheelAccum::default();
    let mut selector = Selector::new();
    let mut published_modes = (false, false);
    let mut had_selection = false;
    let mut sel_bg = theme.selection;

    let mut prev: Vec<Arc<RowData>> = vec![];
    let mut exited = false;
    let mut visible = true;
    let mut force_full = true;
    let mut dirty = true;
    let mut last_frame = Instant::now() - Duration::from_secs(1);

    loop {
        // Block until there's work, or until the pending frame is due. A hidden
        // Terminal has no frame to wait for.
        let waiting_for_frame = dirty && visible;
        // A drag held outside the pane also wakes the loop to scroll it.
        let timeout = [
            waiting_for_frame.then(|| FRAME.saturating_sub(last_frame.elapsed())),
            selector.autoscrolling().then(|| AUTOSCROLL.saturating_sub(selector.last_tick.elapsed())),
        ]
        .into_iter()
        .flatten()
        .min();
        let first = if let Some(wait) = timeout {
            match rx.recv_timeout(wait) {
                Ok(c) => Some(c),
                Err(RecvTimeoutError::Timeout) => None,
                Err(RecvTimeoutError::Disconnected) => return,
            }
        } else {
            match rx.recv() {
                Ok(c) => Some(c),
                Err(_) => return,
            }
        };
        let mut next = first;
        let batch_start = Instant::now();
        while let Some(c) = next.take() {
            match c {
                Cmd::Shutdown => return,
                #[cfg(test)]
                Cmd::Panic => panic!("test: owner thread panic"),
                #[cfg(test)]
                Cmd::ScrollTop => {
                    term.scroll_viewport(ScrollViewport::Top);
                }
                Cmd::Output(b) => {
                    term.vt_write(&b);
                    inflight.fetch_sub(b.len() as u64, Relaxed);
                }
                Cmd::Exited => {
                    exited = true;
                    // An exit leaves the screen clean but the frame must say so.
                    force_full = true;
                    shared.exited.store(true, Release);
                    // A hidden Terminal still reports the one change that affects the screen.
                    if !visible {
                        wake.fire();
                    }
                }
                Cmd::Key(k) => {
                    term.scroll_viewport(ScrollViewport::Bottom);
                    let (code, unshifted) = map_key(k.key);
                    let mods = map_mods(k.mods);
                    key_ev
                        .set_action(key::Action::Press)
                        .set_key(code)
                        .set_mods(mods)
                        .set_consumed_mods(if unshifted != '\0' && mods.contains(key::Mods::SHIFT) {
                            key::Mods::SHIFT
                        } else {
                            key::Mods::empty()
                        })
                        .set_unshifted_codepoint(unshifted)
                        .set_utf8(k.text.clone());
                    key_enc.set_options_from_terminal(&term);
                    key_enc.set_macos_option_as_alt(if k.option_as_alt {
                        key::OptionAsAlt::True
                    } else {
                        key::OptionAsAlt::False
                    });
                    let mut out = Vec::with_capacity(32);
                    if key_enc.encode_to_vec(&key_ev, &mut out).is_ok() && !out.is_empty() {
                        let _ = wtx.send(out);
                    }
                }
                Cmd::Mouse(m) => {
                    // A gesture belongs to whoever it started with, even if Shift changes.
                    let target = match (m.action, grab) {
                        (MouseAction::Press(_), _) | (_, None) => {
                            pointer::route(term.is_mouse_tracking().unwrap_or(false), m.mods.shift)
                        }
                        (_, Some(t)) => t,
                    };
                    grab = match m.action {
                        MouseAction::Press(_) => Some(target),
                        MouseAction::Release(_) => None,
                        MouseAction::Move => grab,
                    };
                    if target == pointer::PointerTarget::Program {
                        let (action, button) = match m.action {
                            MouseAction::Press(b) => {
                                held = Some(b);
                                (mouse::Action::Press, Some(b))
                            }
                            MouseAction::Release(b) => {
                                held = None;
                                (mouse::Action::Release, Some(b))
                            }
                            MouseAction::Move => (mouse::Action::Motion, held),
                        };
                        let sz = *size.lock().unwrap();
                        // Report the middle of the cell, clamped to the grid.
                        let col = m.col.clamp(0, sz.0 as i32 - 1) as f32;
                        let row = m.row.clamp(0, sz.1 as i32 - 1) as f32;
                        mouse_ev
                            .set_action(action)
                            .set_button(button.map(map_button))
                            .set_mods(map_mods(m.mods))
                            .set_position(mouse::Position {
                                x: (col + 0.5) * sz.2 as f32,
                                y: (row + 0.5) * sz.3 as f32,
                            });
                        sync_mouse(&mut mouse_enc, &term, sz, held.is_some());
                        let mut out = Vec::new();
                        if mouse_enc.encode_to_vec(&mouse_ev, &mut out).is_ok() && !out.is_empty() {
                            let _ = wtx.send(out);
                        }
                    } else {
                        let sz = *size.lock().unwrap();
                        selector.handle(&term, m, sz);
                    }
                }
                Cmd::SelectedText(reply) => {
                    let options = FormatOptions::new()
                        .with_emit_format(Format::Plain)
                        .with_unwrap(true)
                        .with_trim(true);
                    let text = term
                        .format_selection_alloc(None, options)
                        .ok()
                        .flatten()
                        .map(|b| String::from_utf8_lossy(&b).into_owned())
                        .filter(|s| !s.is_empty());
                    let _ = reply.send(text);
                }
                Cmd::Wheel(w) => {
                    let n = wheel_acc.push(w.rows);
                    if n != 0 {
                        let tracking = term.is_mouse_tracking().unwrap_or(false);
                        let alt = term.mode(Mode::ALT_SCREEN_SAVE).unwrap_or(false)
                            || term.mode(Mode::ALT_SCREEN).unwrap_or(false)
                            || term.mode(Mode::ALT_SCREEN_LEGACY).unwrap_or(false);
                        match pointer::wheel_target(tracking, alt) {
                            pointer::WheelTarget::Program => {
                                let sz = *size.lock().unwrap();
                                sync_mouse(&mut mouse_enc, &term, sz, false);
                                let col = w.col.clamp(0, sz.0 as i32 - 1) as f32;
                                let row = w.row.clamp(0, sz.1 as i32 - 1) as f32;
                                let button =
                                    if n > 0 { mouse::Button::Four } else { mouse::Button::Five };
                                mouse_ev.set_button(Some(button)).set_mods(map_mods(w.mods)).set_position(
                                    mouse::Position {
                                        x: (col + 0.5) * sz.2 as f32,
                                        y: (row + 0.5) * sz.3 as f32,
                                    },
                                );
                                let mut out = Vec::new();
                                for _ in 0..n.unsigned_abs() {
                                    mouse_ev.set_action(mouse::Action::Press);
                                    let _ = mouse_enc.encode_to_vec(&mouse_ev, &mut out);
                                    mouse_ev.set_action(mouse::Action::Release);
                                    let _ = mouse_enc.encode_to_vec(&mouse_ev, &mut out);
                                }
                                if !out.is_empty() {
                                    let _ = wtx.send(out);
                                }
                            }
                            pointer::WheelTarget::Arrows => {
                                let app = term.mode(Mode::DECCKM).unwrap_or(false);
                                let seq: &[u8] = match (n > 0, app) {
                                    (true, true) => b"\x1bOA",
                                    (true, false) => b"\x1b[A",
                                    (false, true) => b"\x1bOB",
                                    (false, false) => b"\x1b[B",
                                };
                                let _ = wtx.send(seq.repeat(n.unsigned_abs() as usize));
                            }
                            pointer::WheelTarget::Scrollback => {
                                term.scroll_viewport(ScrollViewport::Delta(-(n as isize)));
                            }
                        }
                    }
                }
                Cmd::Paste(s) => {
                    term.scroll_viewport(ScrollViewport::Bottom);
                    let bracketed = term.mode(Mode::BRACKETED_PASTE).unwrap_or(false);
                    let mut data = s.into_bytes();
                    let mut buf = vec![0u8; data.len() * 2 + 32];
                    if let Ok(n) = paste::encode(&mut data, bracketed, &mut buf) {
                        buf.truncate(n);
                        let _ = wtx.send(buf);
                    }
                }
                Cmd::Resize { cols, rows, cell_w, cell_h } => {
                    let (pw, ph) = (cell_w.round() as u32, cell_h.round() as u32);
                    let _ = term.resize(cols, rows, pw, ph);
                    if let Ok(m) = master.lock() {
                        let _ = m.resize(PtySize {
                            rows,
                            cols,
                            pixel_width: cols.saturating_mul(pw as u16),
                            pixel_height: rows.saturating_mul(ph as u16),
                        });
                    }
                    *size.lock().unwrap() = (cols, rows, pw, ph);
                    force_full = true;
                }
                Cmd::Theme(t) => {
                    apply_theme(&mut term, &t);
                    sel_bg = t.selection;
                    light.store(t.light, Relaxed);
                    // Mode 2031: a program that asked is told when the scheme changes.
                    if term.mode(Mode::COLOR_SCHEME_REPORT).unwrap_or(false) {
                        let scheme = if t.light { ColorScheme::Light } else { ColorScheme::Dark };
                        let mut buf = [0u8; 16];
                        if let Ok(n) = scheme.encode_report(&mut buf) {
                            let _ = wtx.send(buf[..n].to_vec());
                        }
                    }
                    force_full = true;
                }
                Cmd::Visible(v) => {
                    if v && !visible {
                        // Build a full frame at once on becoming visible.
                        force_full = true;
                        last_frame = Instant::now() - FRAME;
                    }
                    visible = v;
                }
            }
            dirty = true;
            // Drain what's queued, but keep the frame deadline honest under a flood.
            if batch_start.elapsed() < FRAME {
                next = rx.try_recv().ok();
            }
        }

        // A program turning mouse tracking or the alternate screen on or off changes no
        // cell, so the render state stays clean; the frame still has to say so.
        let modes = (
            term.is_mouse_tracking().unwrap_or(false),
            term.mode(Mode::ALT_SCREEN_SAVE).unwrap_or(false),
        );
        if modes != published_modes {
            published_modes = modes;
            force_full = true;
        }

        if selector.autoscrolling() && selector.last_tick.elapsed() >= AUTOSCROLL {
            let sz = *size.lock().unwrap();
            selector.autoscroll(&mut term, sz);
            dirty = true;
        }

        if dirty && visible && last_frame.elapsed() >= FRAME {
            let frame = build_frame(
                &term,
                &mut rs,
                &mut row_it,
                &mut cell_it,
                &mut prev,
                force_full,
                exited,
                &mut had_selection,
                sel_bg,
            );
            if let Some(f) = frame {
                // Only a real frame starts the 16 ms budget (a spike bug restarted it on empty checks).
                last_frame = Instant::now();
                *shared.frame.lock().unwrap_or_else(|p| p.into_inner()) = Arc::new(f);
                wake.fire();
            }
            force_full = false;
            dirty = false;
        }
    }
}

/// Always on; tests switch it off to time what the floor costs.
fn floor_enabled() -> bool {
    #[cfg(test)]
    {
        FLOOR_OFF.load(Relaxed) == false
    }
    #[cfg(not(test))]
    {
        true
    }
}

#[cfg(test)]
static FLOOR_OFF: AtomicBool = AtomicBool::new(false);

fn apply_theme(term: &mut Terminal<'_, '_>, t: &TermTheme) {
    let c = |c: Rgb| RgbColor { r: c[0], g: c[1], b: c[2] };
    term.set_default_fg_color(Some(c(t.fg))).unwrap();
    term.set_default_bg_color(Some(c(t.bg))).unwrap();
    term.set_default_cursor_color(Some(c(t.cursor))).unwrap();
    let mut p = term.default_color_palette().unwrap();
    for (i, col) in t.ansi.iter().enumerate() {
        p.set(PaletteIndex(i as u8), c(*col));
    }
    term.set_default_color_palette(Some(p)).unwrap();
}

#[allow(clippy::too_many_arguments)]
fn build_frame(
    term: &Terminal<'static, 'static>,
    rs: &mut RenderState<'static>,
    row_it: &mut RowIterator<'static>,
    cell_it: &mut CellIterator<'static>,
    prev: &mut Vec<Arc<RowData>>,
    force_full: bool,
    exited: bool,
    had_selection: &mut bool,
    selection_bg: Rgb,
) -> Option<Frame> {
    let snap = rs.update(term).ok()?;
    let dirty = snap.dirty().ok()?;
    // Rows are cached by pointer, so while a selection exists (and on the frame
    // after it goes) every row is rebuilt: the selection can change without the
    // rows underneath it being marked dirty.
    let has_selection = term.selection().ok().flatten().is_some();
    let selection_in_play = has_selection || std::mem::replace(had_selection, has_selection);
    if dirty == Dirty::Clean && !force_full && !selection_in_play {
        return None;
    }
    let full = force_full || selection_in_play || dirty == Dirty::Full;
    let colors = snap.colors().ok()?;
    let (cols, nrows) = (snap.cols().ok()?, snap.rows().ok()? as usize);

    let mut rows = Vec::with_capacity(nrows);
    let mut iter = row_it.update(&snap).ok()?;
    let mut y = 0usize;
    let mut floored: Option<(Rgb, Rgb, Rgb)> = None;
    let mut text = String::new();
    while let Some(row) = iter.next() {
        let clean = !full && !row.dirty().unwrap_or(true) && y < prev.len();
        if clean {
            rows.push(prev[y].clone());
        } else {
            let mut data = RowData::default();
            data.selected = row.selection().ok().flatten().map(|s| (s.start_x, s.end_x));
            let raw_row = row.raw_row().ok();
            data.wrapped = raw_row.and_then(|r| r.is_wrapped().ok()).unwrap_or(false);
            // Only a row that says it has links is worth a lookup per cell.
            let row_has_links = raw_row.and_then(|r| r.has_hyperlink().ok()).unwrap_or(false);
            let mut cells = cell_it.update(row).ok()?;
            let mut col = 0u16;
            while let Some(cell) = cells.next() {
                let this = col;
                col += 1;
                let raw = cell.raw_cell().ok()?;
                let wide = raw.wide().unwrap_or(CellWide::Narrow);
                if matches!(wide, CellWide::SpacerTail | CellWide::SpacerHead) {
                    continue;
                }
                let width = if wide == CellWide::Wide { 2 } else { 1 };
                if row_has_links && raw.has_hyperlink().unwrap_or(false) {
                    let at = Point::Viewport(PointCoordinate { x: this, y: y as u32 });
                    let mut buf = [0u8; 2048];
                    let uri = term
                        .grid_ref(at)
                        .and_then(|g| g.hyperlink_uri(&mut buf))
                        .ok()
                        .and_then(|n| std::str::from_utf8(&buf[..n]).ok());
                    if let Some(uri) = uri.filter(|u| !u.is_empty()) {
                        match data.links.last_mut() {
                            Some(l) if l.uri == uri && l.col + l.len == this => l.len += width,
                            _ => data.links.push(LinkSpan { col: this, len: width, uri: uri.to_string() }),
                        }
                    }
                }
                let style = if raw.has_styling().unwrap_or(false) { cell.style().ok() } else { None };
                let mut fg = cell.fg_color().ok().flatten().map(rgb);
                let mut bg = cell.bg_color().ok().flatten().map(rgb);
                let (mut bold, mut italic, mut faint, mut underline, mut strike, mut invisible) =
                    (false, false, false, false, false, false);
                if let Some(s) = style {
                    bold = s.bold;
                    italic = s.italic;
                    faint = s.faint;
                    strike = s.strikethrough;
                    invisible = s.invisible;
                    underline = s.underline != libghostty_vt::style::Underline::None;
                    if s.inverse {
                        let (f, b) = (fg.unwrap_or(rgb(colors.foreground)), bg.unwrap_or(rgb(colors.background)));
                        fg = Some(b);
                        bg = Some(f);
                    }
                }
                if let Some(b) = bg {
                    match data.bgs.last_mut() {
                        Some(l) if l.color == b && l.col + l.len == this => l.len += width,
                        _ => data.bgs.push(BgRun { col: this, len: width, color: b }),
                    }
                }
                text.clear();
                let _ = cell.graphemes_utf8(&mut text);
                let mut fg = fg.unwrap_or(rgb(colors.foreground));
                let blank = text.is_empty() || invisible;
                if !blank && floor_enabled() {
                    // The contrast floor (JRST-393). Invisible text is blanked above and never floored.
                    let cell_bg = bg.unwrap_or(rgb(colors.background));
                    fg = match floored {
                        Some((f, b, out)) if f == fg && b == cell_bg => out,
                        _ => {
                            let out = min_contrast(fg, cell_bg, CONTRAST_FLOOR);
                            floored = Some((fg, cell_bg, out));
                            out
                        }
                    };
                }
                if blank && !underline && !strike {
                    // Plain blank: extends a same-style ASCII run only if more text follows; trimmed below.
                    if let Some(r) = data.runs.last_mut() {
                        if r.ascii && r.col + r.cells == this {
                            r.text.push(' ');
                            r.cells += 1;
                        }
                    }
                    continue;
                }
                let ascii = !text.is_empty() && text.is_ascii() || blank;
                let piece: &str = if blank { " " } else { &text };
                let same = |r: &Run| {
                    r.ascii
                        && ascii
                        && r.col + r.cells == this
                        && r.fg == fg
                        && r.bold == bold
                        && r.italic == italic
                        && r.faint == faint
                        && r.underline == underline
                        && r.strike == strike
                };
                match data.runs.last_mut() {
                    Some(r) if same(r) => {
                        r.text.push_str(piece);
                        r.cells += 1;
                    }
                    last => {
                        if let Some(r) = last {
                            let t = r.text.trim_end_matches(' ').len();
                            if !(r.underline || r.strike) {
                                r.cells -= (r.text.len() - t) as u16;
                                r.text.truncate(t);
                            }
                        }
                        data.runs.push(Run {
                            col: this,
                            text: piece.to_string(),
                            cells: width,
                            fg,
                            bold,
                            italic,
                            faint,
                            underline,
                            strike,
                            ascii,
                        });
                    }
                }
            }
            if let Some(r) = data.runs.last_mut() {
                let t = r.text.trim_end_matches(' ').len();
                if !(r.underline || r.strike) && r.ascii {
                    r.cells -= (r.text.len() - t) as u16;
                    r.text.truncate(t);
                }
            }
            rows.push(Arc::new(data));
        }
        let _ = row.set_dirty(false);
        y += 1;
    }
    let cursor = if snap.cursor_visible().unwrap_or(false) {
        snap.cursor_viewport().ok().flatten().map(|c| CursorData {
            x: c.x,
            y: c.y,
            shape: match snap.cursor_visual_style() {
                Ok(CursorVisualStyle::Bar) => CursorShape::Bar,
                Ok(CursorVisualStyle::Underline) => CursorShape::Underline,
                _ => CursorShape::Block,
            },
            color: colors.cursor.map(rgb).unwrap_or(rgb(colors.foreground)),
        })
    } else {
        None
    };
    let _ = snap.set_dirty(Dirty::Clean);
    *prev = rows.clone();
    Some(Frame {
        cols,
        rows,
        bg: rgb(colors.background),
        fg: rgb(colors.foreground),
        selection_bg,
        cursor,
        title: String::from_utf8_lossy(term.title().unwrap_or(b"")).into_owned(),
        scrollback: term.scrollback_rows().unwrap_or(0),
        exited,
        dead: false,
        tracking: term.is_mouse_tracking().unwrap_or(false),
        alt: term.mode(Mode::ALT_SCREEN_SAVE).unwrap_or(false),
    })
}

#[cfg(test)]
mod cost {
    use super::*;

    /// Time a full frame build of a 200x40 screen, floor off then on. Run with
    /// `cargo test --release floor_cost -- --ignored --nocapture`.
    fn timed(label: &str, fill: &dyn Fn(usize, usize) -> String) {
        let mut term: Terminal<'static, 'static> = Terminal::new(200, 40).unwrap();
        let mut out = String::new();
        for y in 0..40 {
            for x in 0..200 {
                out.push_str(&fill(x, y));
            }
            out.push_str("\x1b[0m\r\n");
        }
        term.vt_write(out.as_bytes());
        let mut rs = RenderState::new().unwrap();
        let mut row_it = RowIterator::new().unwrap();
        let mut cell_it = CellIterator::new().unwrap();
        for off in [true, false, true, false] {
            FLOOR_OFF.store(off, Relaxed);
            let mut prev = vec![];
            let n = 200;
            let t0 = Instant::now();
            for _ in 0..n {
                build_frame(&term, &mut rs, &mut row_it, &mut cell_it, &mut prev, true, false, &mut false, [0; 3])
                    .unwrap();
            }
            let per = t0.elapsed().as_secs_f64() * 1e6 / n as f64;
            println!("{label:28} floor {:3}: {per:8.1} us per full frame", if off { "off" } else { "on" });
        }
        FLOOR_OFF.store(false, Relaxed);
    }

    #[test]
    #[ignore]
    fn floor_cost() {
        timed("plain text (default colours)", &|x, _| format!("{}", (b'a' + (x % 26) as u8) as char));
        timed("ansi colour every 8 cells", &|x, y| {
            let c = 31 + ((x / 8 + y) % 7);
            format!("\x1b[{c}m{}", (b'a' + (x % 26) as u8) as char)
        });
        timed("truecolor, every cell differs", &|x, y| {
            format!("\x1b[38;2;{};{};{}m{}", (x * 7) % 256, (y * 11) % 256, (x * y) % 256, 'x')
        });
        timed("truecolor fg+bg, all differ", &|x, y| {
            format!("\x1b[38;2;{};{};{};48;2;{};{};{}mx", (x * 7) % 256, (y * 11) % 256, (x * y) % 256, (x * 3) % 256, (y * 5) % 256, (x + y) % 256)
        });
    }
}
