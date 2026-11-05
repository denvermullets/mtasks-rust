//! PROTOTYPE (JRST-328): one Terminal = one owner thread (libghostty types are !Send),
//! one PTY reader thread, one PTY writer thread. The UI only ever sees plain-data Frames.
use iced::futures::channel::mpsc::UnboundedSender;
use libghostty_vt::render::{CellIterator, CursorVisualStyle, Dirty, RowIterator};
use libghostty_vt::screen::CellWide;
use libghostty_vt::style::{PaletteIndex, RgbColor};
use libghostty_vt::terminal::{Mode, ScrollViewport};
use libghostty_vt::{key, mouse, paste, RenderState, Terminal};
use portable_pty::{native_pty_system, CommandBuilder, PtySize};
use std::io::{Read, Write};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering::*};
use std::sync::mpsc::{channel, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub type Rgb = [u8; 3];
fn rgb(c: RgbColor) -> Rgb { [c.r, c.g, c.b] }

#[derive(Debug, Clone, PartialEq)]
pub struct Run { pub col: u16, pub text: String, pub cells: u16, pub fg: Rgb, pub bold: bool, pub italic: bool, pub faint: bool, pub underline: bool, pub strike: bool, pub ascii: bool }
#[derive(Debug, Clone, PartialEq)]
pub struct BgRun { pub col: u16, pub len: u16, pub color: Rgb }
#[derive(Debug, Default, PartialEq)]
pub struct RowData { pub runs: Vec<Run>, pub bgs: Vec<BgRun> }
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum CursorShape { Block, Bar, Underline }
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CursorData { pub x: u16, pub y: u16, pub shape: CursorShape, pub color: Rgb }
#[derive(Debug, Default)]
pub struct Frame { pub cols: u16, pub rows: Vec<Arc<RowData>>, pub bg: Rgb, pub fg: Rgb, pub cursor: Option<CursorData>, pub title: String, pub scrollback: usize, pub exited: bool, pub tracking: bool, pub alt: bool }

#[derive(Debug, Clone)]
pub struct TermTheme { pub fg: Rgb, pub bg: Rgb, pub cursor: Rgb, pub ansi: [Rgb; 16] }

#[derive(Debug, Clone)]
pub struct KeyInput { pub key: key::Key, pub mods: key::Mods, pub unshifted: char, pub text: Option<String>, pub option_as_alt: bool }
#[derive(Debug, Clone, Copy)]
pub enum MouseKind { Press(mouse::Button), Release(mouse::Button), Move }

pub enum Cmd {
    Output(Vec<u8>),
    Exited,
    Key(KeyInput),
    Paste(String),
    Write(Vec<u8>),
    Resize { cols: u16, rows: u16, cell_w: f32, cell_h: f32 },
    Theme(TermTheme),
    Mouse { kind: MouseKind, x: f32, y: f32, mods: key::Mods },
    Wheel { lines: f32, x: f32, y: f32, mods: key::Mods },
}

#[derive(Default)]
pub struct Stats { pub bytes: AtomicU64, pub frames: AtomicU64, pub frame_us: AtomicU64, pub vt_us: AtomicU64, pub max_input_lat_us: AtomicU64 }

pub struct TermHandle { pub tx: Sender<Cmd>, pub frame: Arc<Mutex<Arc<Frame>>>, pub notified: Arc<AtomicBool>, pub stats: Arc<Stats> }

pub fn keylog() -> bool { std::env::var_os("SPIKE_KEYTEST").is_some() }
const MAX_INFLIGHT: u64 = 4 << 20; // back-pressure: at most 4MB of unprocessed output

pub fn spawn(cols: u16, rows: u16, theme: TermTheme, wake: UnboundedSender<()>) -> TermHandle {
    let (tx, rx) = channel();
    let frame = Arc::new(Mutex::new(Arc::new(Frame::default())));
    let notified = Arc::new(AtomicBool::new(false));
    let stats = Arc::new(Stats::default());
    let h = TermHandle { tx: tx.clone(), frame: frame.clone(), notified: notified.clone(), stats: stats.clone() };
    std::thread::Builder::new().name("term-owner".into()).spawn(move || {
        owner(cols, rows, theme, tx, rx, frame, notified, stats, wake);
    }).unwrap();
    h
}

#[allow(clippy::too_many_arguments)]
fn owner(cols: u16, rows: u16, theme: TermTheme, tx: Sender<Cmd>, rx: Receiver<Cmd>, shared: Arc<Mutex<Arc<Frame>>>, notified: Arc<AtomicBool>, stats: Arc<Stats>, wake: UnboundedSender<()>) {
    // --- PTY ---
    let pty = native_pty_system().openpty(PtySize { rows, cols, pixel_width: 0, pixel_height: 0 }).unwrap();
    if let Some(fd) = pty.master.as_raw_fd() {
        unsafe { // research: portable-pty does not set IUTF8
            let mut t: libc::termios = std::mem::zeroed();
            if libc::tcgetattr(fd, &mut t) == 0 { t.c_iflag |= libc::IUTF8; libc::tcsetattr(fd, libc::TCSANOW, &t); }
        }
    }
    let mut cmd = CommandBuilder::new_default_prog();
    cmd.env("TERM", "xterm-256color");
    cmd.env("COLORTERM", "truecolor");
    cmd.env("TERM_PROGRAM", "jait-spike");
    if let Some(home) = std::env::var_os("HOME") { cmd.cwd(home); }
    let _child = pty.slave.spawn_command(cmd).unwrap();
    drop(pty.slave);
    let master = pty.master;

    let inflight = Arc::new(AtomicU64::new(0));
    {
        let mut reader = master.try_clone_reader().unwrap();
        let (tx, inflight) = (tx.clone(), inflight.clone());
        std::thread::Builder::new().name("term-read".into()).spawn(move || {
            let mut buf = vec![0u8; 64 * 1024];
            loop {
                while inflight.load(Relaxed) > MAX_INFLIGHT { std::thread::sleep(Duration::from_micros(500)); }
                match reader.read(&mut buf) {
                    Ok(0) | Err(_) => { let _ = tx.send(Cmd::Exited); break; }
                    Ok(n) => { inflight.fetch_add(n as u64, Relaxed); if tx.send(Cmd::Output(buf[..n].to_vec())).is_err() { break; } }
                }
            }
        }).unwrap();
    }
    // Writes never happen on the owner thread: a child that stops reading must not stall emulation.
    let (wtx, wrx) = channel::<Vec<u8>>();
    {
        let mut writer = master.take_writer().unwrap();
        std::thread::Builder::new().name("term-write".into()).spawn(move || {
            while let Ok(b) = wrx.recv() { if writer.write_all(&b).is_err() { break; } let _ = writer.flush(); }
        }).unwrap();
    }

    // --- Terminal ---
    let size = Arc::new(Mutex::new((cols, rows, 8u32, 16u32)));
    let mut term: Terminal<'static, 'static> = Terminal::new(cols, rows).unwrap();
    term.set_scrollback_max_lines(Some(10_000)).unwrap();
    // Both limits apply; the default byte limit otherwise caps history at a few hundred rows.
    term.set_scrollback_max_bytes(Some(64 * 1024 * 1024)).unwrap();
    term.set_terminfo_name("xterm-256color").unwrap();
    {
        let w = wtx.clone();
        term.on_pty_write(move |_t, d| { let _ = w.send(d.to_vec()); }).unwrap();
        let s = size.clone();
        term.on_size(move |_t| {
            let (columns, rows, cell_width, cell_height) = *s.lock().unwrap();
            Some(libghostty_vt::terminal::SizeReportSize { rows, columns, cell_width, cell_height })
        }).unwrap();
        term.on_device_attributes(|_t| {
            use libghostty_vt::terminal::*;
            Some(DeviceAttributes {
                primary: PrimaryDeviceAttributes::new(ConformanceLevel::VT220, &[DeviceAttributeFeature::COLUMNS_132, DeviceAttributeFeature::SELECTIVE_ERASE, DeviceAttributeFeature::ANSI_COLOR]),
                secondary: SecondaryDeviceAttributes { device_type: DeviceType::VT220, firmware_version: 1, rom_cartridge: 0 },
                tertiary: Default::default(),
            })
        }).unwrap();
        term.on_xtversion(|_t| Some("jait-spike")).unwrap();
    }
    apply_theme(&mut term, &theme);

    let mut rs = RenderState::new().unwrap();
    let mut row_it = RowIterator::new().unwrap();
    let mut cell_it = CellIterator::new().unwrap();
    let mut key_enc = key::Encoder::new().unwrap();
    let mut key_ev = key::Event::new().unwrap();
    let mut mouse_enc = mouse::Encoder::new().unwrap();
    let mut mouse_ev = mouse::Event::new().unwrap();
    let mut held: Option<mouse::Button> = None;
    let mut wheel_acc = 0f32;
    let mut cell_px = (8f32, 16f32);

    let mut key_t: Option<Instant> = None;
    let mut key_chunks = 0u32;
    let mut prev: Vec<Arc<RowData>> = vec![];
    let mut exited = false;
    let mut force_full = true;
    let mut dirty = true;
    let mut last_frame = Instant::now() - Duration::from_secs(1);
    const FRAME: Duration = Duration::from_millis(16);

    loop {
        // Block until there's work, or until the pending frame is due.
        let first = if dirty {
            match rx.recv_timeout(FRAME.saturating_sub(last_frame.elapsed())) {
                Ok(c) => Some(c),
                Err(RecvTimeoutError::Timeout) => None,
                Err(RecvTimeoutError::Disconnected) => return,
            }
        } else {
            match rx.recv() { Ok(c) => Some(c), Err(_) => return }
        };
        let mut next = first;
        let batch_start = Instant::now();
        while let Some(c) = next.take() {
            match c {
                Cmd::Output(b) => {
                    if let Some(k) = key_t { key_chunks += 1; if key_chunks <= 4 { eprintln!("KEY +{:.1}ms output chunk {} ({} bytes)", k.elapsed().as_secs_f32() * 1e3, key_chunks, b.len()); } }
                    let t = Instant::now();
                    term.vt_write(&b);
                    stats.vt_us.fetch_add(t.elapsed().as_micros() as u64, Relaxed);
                    stats.bytes.fetch_add(b.len() as u64, Relaxed);
                    inflight.fetch_sub(b.len() as u64, Relaxed);
                }
                Cmd::Exited => exited = true,
                Cmd::Write(b) => { term.scroll_viewport(ScrollViewport::Bottom); let _ = wtx.send(b); }
                Cmd::Key(k) => {
                    if keylog() { key_t = Some(Instant::now()); key_chunks = 0; }
                    term.scroll_viewport(ScrollViewport::Bottom);
                    key_ev.set_action(key::Action::Press).set_key(k.key).set_mods(k.mods)
                        .set_consumed_mods(if k.unshifted != '\0' && k.mods.contains(key::Mods::SHIFT) { key::Mods::SHIFT } else { key::Mods::empty() })
                        .set_unshifted_codepoint(k.unshifted)
                        .set_utf8(k.text.clone());
                    key_enc.set_options_from_terminal(&term);
                    key_enc.set_macos_option_as_alt(if k.option_as_alt { key::OptionAsAlt::True } else { key::OptionAsAlt::False });
                    let mut out = Vec::with_capacity(32);
                    if key_enc.encode_to_vec(&key_ev, &mut out).is_ok() && !out.is_empty() {
                        let _ = wtx.send(out);
                    }
                }
                Cmd::Paste(s) => {
                    let bracketed = term.mode(Mode::BRACKETED_PASTE).unwrap_or(false);
                    let mut data = s.into_bytes();
                    let mut buf = vec![0u8; data.len() * 2 + 32];
                    if let Ok(n) = paste::encode(&mut data, bracketed, &mut buf) { buf.truncate(n); let _ = wtx.send(buf); }
                }
                Cmd::Resize { cols, rows, cell_w, cell_h } => {
                    cell_px = (cell_w, cell_h);
                    let (pw, ph) = (cell_w.round() as u32, cell_h.round() as u32);
                    let _ = term.resize(cols, rows, pw, ph);
                    let _ = master.resize(PtySize { rows, cols, pixel_width: cols * pw as u16, pixel_height: rows * ph as u16 });
                    *size.lock().unwrap() = (cols, rows, pw, ph);
                    force_full = true;
                }
                Cmd::Theme(t) => { apply_theme(&mut term, &t); force_full = true; }
                Cmd::Mouse { kind, x, y, mods } => {
                    if term.is_mouse_tracking().unwrap_or(false) {
                        let (action, button) = match kind {
                            MouseKind::Press(b) => { held = Some(b); (mouse::Action::Press, Some(b)) }
                            MouseKind::Release(b) => { held = None; (mouse::Action::Release, Some(b)) }
                            MouseKind::Move => (mouse::Action::Motion, held),
                        };
                        mouse_ev.set_action(action).set_button(button).set_mods(mods).set_position(mouse::Position { x, y });
                        sync_mouse(&mut mouse_enc, &term, cell_px, held.is_some());
                        let mut out = vec![];
                        if mouse_enc.encode_to_vec(&mouse_ev, &mut out).is_ok() && !out.is_empty() { let _ = wtx.send(out); }
                    }
                }
                Cmd::Wheel { lines, x, y, mods } => {
                    wheel_acc += lines;
                    let n = wheel_acc.trunc() as i32;
                    wheel_acc -= n as f32;
                    if n != 0 {
                        if term.is_mouse_tracking().unwrap_or(false) {
                            sync_mouse(&mut mouse_enc, &term, cell_px, false);
                            let btn = if n > 0 { mouse::Button::Four } else { mouse::Button::Five };
                            mouse_ev.set_button(Some(btn)).set_mods(mods).set_position(mouse::Position { x, y });
                            let mut out = vec![];
                            for _ in 0..n.abs() {
                                mouse_ev.set_action(mouse::Action::Press);
                                let _ = mouse_enc.encode_to_vec(&mouse_ev, &mut out);
                                mouse_ev.set_action(mouse::Action::Release);
                                let _ = mouse_enc.encode_to_vec(&mouse_ev, &mut out);
                            }
                            if !out.is_empty() { let _ = wtx.send(out); }
                        } else if term.mode(Mode::ALT_SCREEN_SAVE).unwrap_or(false) || term.mode(Mode::ALT_SCREEN).unwrap_or(false) {
                            let seq: &[u8] = if n > 0 { b"\x1bOA" } else { b"\x1bOB" };
                            let _ = wtx.send(seq.repeat(n.unsigned_abs() as usize));
                        } else {
                            term.scroll_viewport(ScrollViewport::Delta(-(n as isize)));
                        }
                    }
                }
            }
            dirty = true;
            // Drain what's queued, but keep the frame deadline honest under a flood.
            if batch_start.elapsed() < FRAME { next = rx.try_recv().ok(); }
        }

        if dirty && last_frame.elapsed() >= FRAME {
            let t = Instant::now();
            let frame = build_frame(&term, &mut rs, &mut row_it, &mut cell_it, &mut prev, force_full, exited);
            if let Some(f) = frame {
                last_frame = Instant::now(); // only a real frame starts the 16ms budget
                *shared.lock().unwrap() = Arc::new(f);
                stats.frames.fetch_add(1, Relaxed);
                stats.frame_us.fetch_add(t.elapsed().as_micros() as u64, Relaxed);
                if let Some(k) = key_t { if key_chunks > 0 { eprintln!("KEY +{:.1}ms frame published", k.elapsed().as_secs_f32() * 1e3); key_t = None; } }
                if !notified.swap(true, AcqRel) { let _ = wake.unbounded_send(()); }
            }
            force_full = false;
            dirty = false;
        }
    }
}

fn sync_mouse(e: &mut mouse::Encoder<'_>, term: &Terminal<'_, '_>, cell: (f32, f32), any: bool) {
    let (cols, rows) = (term.cols().unwrap_or(80) as f32, term.rows().unwrap_or(24) as f32);
    e.set_options_from_terminal(term)
        .set_size(mouse::EncoderSize {
            screen_width: (cols * cell.0) as u32, screen_height: (rows * cell.1) as u32,
            cell_width: cell.0.round() as u32, cell_height: cell.1.round() as u32,
            padding_top: 0, padding_bottom: 0, padding_left: 0, padding_right: 0,
        })
        .set_any_button_pressed(any)
        .set_track_last_cell(true);
}

fn apply_theme(term: &mut Terminal<'_, '_>, t: &TermTheme) {
    let c = |c: Rgb| RgbColor { r: c[0], g: c[1], b: c[2] };
    term.set_default_fg_color(Some(c(t.fg))).unwrap();
    term.set_default_bg_color(Some(c(t.bg))).unwrap();
    term.set_default_cursor_color(Some(c(t.cursor))).unwrap();
    let mut p = term.default_color_palette().unwrap();
    for (i, col) in t.ansi.iter().enumerate() { p.set(PaletteIndex(i as u8), c(*col)); }
    term.set_default_color_palette(Some(p)).unwrap();
}

fn build_frame(term: &Terminal<'static, 'static>, rs: &mut RenderState<'static>, row_it: &mut RowIterator<'static>, cell_it: &mut CellIterator<'static>, prev: &mut Vec<Arc<RowData>>, force_full: bool, exited: bool) -> Option<Frame> {
    let snap = rs.update(term).ok()?;
    let dirty = snap.dirty().ok()?;
    if dirty == Dirty::Clean && !force_full { return None; }
    let full = force_full || dirty == Dirty::Full;
    let colors = snap.colors().ok()?;
    let (cols, nrows) = (snap.cols().ok()?, snap.rows().ok()? as usize);

    let mut rows = Vec::with_capacity(nrows);
    let mut iter = row_it.update(&snap).ok()?;
    let mut y = 0usize;
    let mut text = String::new();
    while let Some(row) = iter.next() {
        let clean = !full && !row.dirty().unwrap_or(true) && y < prev.len();
        if clean {
            rows.push(prev[y].clone());
        } else {
            let mut data = RowData::default();
            let mut cells = cell_it.update(row).ok()?;
            let mut col = 0u16;
            while let Some(cell) = cells.next() {
                let this = col;
                col += 1;
                let raw = cell.raw_cell().ok()?;
                let wide = raw.wide().unwrap_or(CellWide::Narrow);
                if matches!(wide, CellWide::SpacerTail | CellWide::SpacerHead) { continue; }
                let width = if wide == CellWide::Wide { 2 } else { 1 };
                let style = if raw.has_styling().unwrap_or(false) { cell.style().ok() } else { None };
                let mut fg = cell.fg_color().ok().flatten().map(rgb);
                let mut bg = cell.bg_color().ok().flatten().map(rgb);
                let (mut bold, mut italic, mut faint, mut underline, mut strike, mut invisible) = (false, false, false, false, false, false);
                if let Some(s) = style {
                    bold = s.bold; italic = s.italic; faint = s.faint; strike = s.strikethrough; invisible = s.invisible;
                    underline = s.underline != libghostty_vt::style::Underline::None;
                    if s.inverse {
                        let (f, b) = (fg.unwrap_or(rgb(colors.foreground)), bg.unwrap_or(rgb(colors.background)));
                        fg = Some(b); bg = Some(f);
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
                let fg = fg.unwrap_or(rgb(colors.foreground));
                let blank = text.is_empty() || invisible;
                if blank && !underline && !strike {
                    // Plain blank: extends a same-style ASCII run only if more text follows; trimmed below.
                    if let Some(r) = data.runs.last_mut() {
                        if r.ascii && r.col + r.cells == this { r.text.push(' '); r.cells += 1; }
                    }
                    continue;
                }
                let ascii = !text.is_empty() && text.is_ascii() || blank;
                let piece: &str = if blank { " " } else { &text };
                let same = |r: &Run| r.ascii && ascii && r.col + r.cells == this && r.fg == fg && r.bold == bold && r.italic == italic && r.faint == faint && r.underline == underline && r.strike == strike;
                match data.runs.last_mut() {
                    Some(r) if same(r) => { r.text.push_str(piece); r.cells += 1; }
                    last => {
                        if let Some(r) = last { let t = r.text.trim_end_matches(' ').len(); if !(r.underline || r.strike) { r.cells -= (r.text.len() - t) as u16; r.text.truncate(t); } }
                        data.runs.push(Run { col: this, text: piece.to_string(), cells: width, fg, bold, italic, faint, underline, strike, ascii });
                    }
                }
            }
            if let Some(r) = data.runs.last_mut() { let t = r.text.trim_end_matches(' ').len(); if !(r.underline || r.strike) && r.ascii { r.cells -= (r.text.len() - t) as u16; r.text.truncate(t); } }
            rows.push(Arc::new(data));
        }
        let _ = row.set_dirty(false);
        y += 1;
    }
    let cursor = if snap.cursor_visible().unwrap_or(false) {
        snap.cursor_viewport().ok().flatten().map(|c| CursorData {
            x: c.x, y: c.y,
            shape: match snap.cursor_visual_style() { Ok(CursorVisualStyle::Bar) => CursorShape::Bar, Ok(CursorVisualStyle::Underline) => CursorShape::Underline, _ => CursorShape::Block },
            color: colors.cursor.map(rgb).unwrap_or(rgb(colors.foreground)),
        })
    } else { None };
    let _ = snap.set_dirty(Dirty::Clean);
    *prev = rows.clone();
    Some(Frame {
        cols, rows, bg: rgb(colors.background), fg: rgb(colors.foreground), cursor,
        title: String::from_utf8_lossy(term.title().unwrap_or(b"")).into_owned(),
        scrollback: term.scrollback_rows().unwrap_or(0), exited,
        tracking: term.is_mouse_tracking().unwrap_or(false),
        alt: term.mode(Mode::ALT_SCREEN_SAVE).unwrap_or(false),
    })
}
