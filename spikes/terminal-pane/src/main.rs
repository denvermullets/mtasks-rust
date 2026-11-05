//! PROTOTYPE (JRST-328): throwaway. Runs a real shell in a docked Iced 0.13 pane using
//! libghostty-vt + portable-pty. Answers: does this engine feel right in this app?
//! Run: scripts in README. Nothing here is production code.
mod term;

use iced::advanced::text::{self as adv_text, Paragraph as _};
use iced::alignment::{Horizontal, Vertical};
use iced::futures::{SinkExt, StreamExt};
use iced::keyboard::{self, key::{Code, Physical}};
use iced::mouse::{self, Cursor};
use iced::widget::canvas::{self, Cache, Canvas, Geometry, Text};
use iced::widget::{button, checkbox, column, container, pick_list, row, scrollable, slider, text};
use iced::{event, font, window, Color, Element, Event, Font, Length, Pixels, Point, Rectangle, Renderer, Size, Subscription, Task, Theme};
use libghostty_vt::{key as gkey, mouse as gmouse};
use std::sync::atomic::Ordering::*;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Instant;
use term::*;

const FONTS: [&str; 6] = ["JetBrains Mono", "IBM Plex Mono", "Fira Code", "SUSE Mono", "Roboto Mono", "Atkinson Hyperlegible Mono"];
const TOOLBAR_H: f32 = 44.0;
const STATUS_H: f32 = 26.0;

#[derive(Debug, Clone, Copy, PartialEq)]
struct ThemeDef { name: &'static str, dark: bool, fg: Rgb, bg: Rgb, cursor: Rgb, ansi: [Rgb; 16] }
impl std::fmt::Display for ThemeDef { fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result { f.write_str(self.name) } }
const fn h(x: u32) -> Rgb { [(x >> 16) as u8, (x >> 8) as u8, x as u8] }
const THEMES: [ThemeDef; 3] = [
    ThemeDef { name: "Tokyo-ish dark", dark: true, fg: h(0xc0caf5), bg: h(0x1a1b26), cursor: h(0xc0caf5), ansi: [h(0x15161e), h(0xf7768e), h(0x9ece6a), h(0xe0af68), h(0x7aa2f7), h(0xbb9af7), h(0x7dcfff), h(0xa9b1d6), h(0x414868), h(0xf7768e), h(0x9ece6a), h(0xe0af68), h(0x7aa2f7), h(0xbb9af7), h(0x7dcfff), h(0xc0caf5)] },
    ThemeDef { name: "Paper light", dark: false, fg: h(0x383a42), bg: h(0xfafafa), cursor: h(0x526eff), ansi: [h(0x383a42), h(0xe45649), h(0x50a14f), h(0xc18401), h(0x4078f2), h(0xa626a4), h(0x0184bc), h(0xa0a1a7), h(0x696c77), h(0xe45649), h(0x50a14f), h(0xc18401), h(0x4078f2), h(0xa626a4), h(0x0184bc), h(0xffffff)] },
    ThemeDef { name: "Ember dark", dark: true, fg: h(0xe6d5c3), bg: h(0x241a14), cursor: h(0xffa657), ansi: [h(0x2d221b), h(0xe5534b), h(0x8fa854), h(0xd9a441), h(0x6f9fb8), h(0xc97fa8), h(0x77b5a0), h(0xd8c8b5), h(0x5b4a3e), h(0xff6f5e), h(0xaec66b), h(0xf0bc55), h(0x89b8d1), h(0xe29ac1), h(0x90cfb9), h(0xfff1e0)] },
];
fn term_theme(t: &ThemeDef) -> TermTheme { TermTheme { fg: t.fg, bg: t.bg, cursor: t.cursor, ansi: t.ansi } }
fn col(c: Rgb) -> Color { Color::from_rgb8(c[0], c[1], c[2]) }

static WAKE: OnceLock<Mutex<Option<iced::futures::channel::mpsc::UnboundedReceiver<()>>>> = OnceLock::new();
fn env_f32(k: &str) -> Option<f32> { std::env::var(k).ok()?.parse().ok() }

#[derive(Debug, Clone)]
enum Message {
    FrameReady,
    Tick,
    Win(Size),
    KeyDown(Physical, keyboard::Modifiers, Option<String>),
    Pasted(Option<String>),
    PickTheme(ThemeDef),
    PickFont(&'static str),
    Size(f32),
    PanelH(f32),
    OptionAlt(bool),
    Heavy(bool),
    FocusTerm(bool),
    MouseEv(MouseKind, f32, f32),
    WheelEv(f32, f32, f32),
    Mods(keyboard::Modifiers),
    ScrollMult(f32),
    ScrollTest,
    Quit,
}

/// The canvas. One Cache per terminal row, so only changed rows are re-laid out.
struct Pane { frame: Arc<Frame>, caches: Vec<Cache>, last: Vec<Arc<RowData>>, bg: Cache, cursor: Cache, cell: (f32, f32), font: &'static str, size: f32, focused: bool, scroll_mult: f32, wheel_discrete: std::cell::Cell<bool>, draws: std::cell::Cell<u64>, draw_us: std::cell::Cell<u64> }

impl canvas::Program<Message> for Pane {
    type State = ();
    fn update(&self, _s: &mut (), ev: canvas::Event, b: Rectangle, c: Cursor) -> (canvas::event::Status, Option<Message>) {
        let Some(p) = c.position_in(b) else { return (canvas::event::Status::Ignored, None) };
        let m = match ev {
            canvas::Event::Mouse(mouse::Event::ButtonPressed(btn)) => mbtn(btn).map(|x| Message::MouseEv(MouseKind::Press(x), p.x, p.y)),
            canvas::Event::Mouse(mouse::Event::ButtonReleased(btn)) => mbtn(btn).map(|x| Message::MouseEv(MouseKind::Release(x), p.x, p.y)),
            canvas::Event::Mouse(mouse::Event::CursorMoved { .. }) => Some(Message::MouseEv(MouseKind::Move, p.x, p.y)),
            canvas::Event::Mouse(mouse::Event::WheelScrolled { delta }) => {
                let (lines, disc) = match delta { mouse::ScrollDelta::Lines { y, .. } => (y * 3.0, true), mouse::ScrollDelta::Pixels { y, .. } => (y / self.cell.1 * self.scroll_mult, false) };
                self.wheel_discrete.set(disc);
                Some(Message::WheelEv(lines, p.x, p.y))
            }
            _ => None,
        };
        (if m.is_some() { canvas::event::Status::Captured } else { canvas::event::Status::Ignored }, m)
    }
    fn draw(&self, _s: &(), r: &Renderer, _t: &Theme, b: Rectangle, _c: Cursor) -> Vec<Geometry> {
        let t0 = Instant::now();
        let (cw, ch) = self.cell;
        let f = &self.frame;
        let mut out = Vec::with_capacity(f.rows.len() + 2);
        out.push(self.bg.draw(r, b.size(), |fr| fr.fill_rectangle(Point::ORIGIN, b.size(), col(f.bg))));
        let base = Font::with_name(self.font);
        for (y, data) in f.rows.iter().enumerate() {
            let Some(cache) = self.caches.get(y) else { break };
            let py = y as f32 * ch;
            out.push(cache.draw(r, b.size(), |fr| {
                for g in &data.bgs { fr.fill_rectangle(Point::new(g.col as f32 * cw, py), Size::new(g.len as f32 * cw, ch), col(g.color)); }
                for run in &data.runs { draw_run(fr, run, base, self.size, cw, ch, py, None, f.bg); }
            }));
        }
        out.push(self.cursor.draw(r, b.size(), |fr| {
            let Some(c) = f.cursor else { return };
            let (px, py) = (c.x as f32 * cw, c.y as f32 * ch);
            let color = col(c.color);
            match (self.focused, c.shape) {
                (false, _) => fr.stroke(&canvas::Path::rectangle(Point::new(px + 0.5, py + 0.5), Size::new(cw - 1.0, ch - 1.0)), canvas::Stroke::default().with_color(color).with_width(1.0)),
                (_, CursorShape::Bar) => fr.fill_rectangle(Point::new(px, py), Size::new(2.0, ch), color),
                (_, CursorShape::Underline) => fr.fill_rectangle(Point::new(px, py + ch - 2.0), Size::new(cw, 2.0), color),
                (_, CursorShape::Block) => {
                    fr.fill_rectangle(Point::new(px, py), Size::new(cw, ch), color);
                    if let Some(data) = f.rows.get(c.y as usize) {
                        if let Some(run) = data.runs.iter().find(|r| c.x >= r.col && c.x < r.col + r.cells) {
                            if let Some(chr) = if run.ascii { run.text.chars().nth((c.x - run.col) as usize) } else { run.text.chars().next() } {
                                let one = Run { col: c.x, text: chr.to_string(), cells: 1, ..run.clone() };
                                draw_run(fr, &one, base, self.size, cw, ch, py, Some(f.bg), f.bg);
                            }
                        }
                    }
                }
            }
        }));
        self.draws.set(self.draws.get() + 1);
        self.draw_us.set(self.draw_us.get() + t0.elapsed().as_micros() as u64);
        out
    }
    fn mouse_interaction(&self, _s: &(), b: Rectangle, c: Cursor) -> mouse::Interaction {
        if c.is_over(b) { mouse::Interaction::Text } else { mouse::Interaction::default() }
    }
}

#[allow(clippy::too_many_arguments)]
fn draw_run(fr: &mut canvas::Frame, run: &Run, base: Font, size: f32, cw: f32, ch: f32, py: f32, over: Option<Rgb>, bg: Rgb) {
    let x = run.col as f32 * cw;
    let mut c = col(over.unwrap_or(run.fg));
    if run.faint { let b = col(bg); c = Color::from_rgb((c.r + b.r) / 2.0, (c.g + b.g) / 2.0, (c.b + b.b) / 2.0); }
    let font = Font { weight: if run.bold { font::Weight::Bold } else { font::Weight::Normal }, style: if run.italic { font::Style::Italic } else { font::Style::Normal }, ..base };
    if !run.text.trim_end().is_empty() {
        fr.fill_text(Text {
            content: run.text.clone(), position: Point::new(x, py), color: c, size: Pixels(size),
            line_height: iced::widget::text::LineHeight::Absolute(Pixels(ch)), font,
            horizontal_alignment: Horizontal::Left, vertical_alignment: Vertical::Top,
            // Basic = no ligatures, no fallback, cheap. Advanced only when a font fallback may be needed.
            shaping: if run.ascii { iced::widget::text::Shaping::Basic } else { iced::widget::text::Shaping::Advanced },
        });
    }
    let w = run.cells as f32 * cw;
    if run.underline { fr.fill_rectangle(Point::new(x, py + ch - 2.0), Size::new(w, 1.0), c); }
    if run.strike { fr.fill_rectangle(Point::new(x, py + ch / 2.0), Size::new(w, 1.0), c); }
}

fn mbtn(b: mouse::Button) -> Option<gmouse::Button> {
    match b { mouse::Button::Left => Some(gmouse::Button::Left), mouse::Button::Right => Some(gmouse::Button::Right), mouse::Button::Middle => Some(gmouse::Button::Middle), _ => None }
}

fn measure(fam: &'static str, size: f32) -> (f32, f32) {
    type P = <Renderer as adv_text::Renderer>::Paragraph;
    let ch = (size * 1.25).ceil();
    let n = 100.0;
    let p = P::with_text(adv_text::Text {
        content: &"M".repeat(n as usize), bounds: Size::INFINITY, size: Pixels(size),
        line_height: iced::widget::text::LineHeight::Absolute(Pixels(ch)), font: Font::with_name(fam),
        horizontal_alignment: Horizontal::Left, vertical_alignment: Vertical::Top,
        shaping: iced::widget::text::Shaping::Basic, wrapping: iced::widget::text::Wrapping::None,
    });
    (p.min_bounds().width / n, ch)
}

struct App {
    term: TermHandle, pane: Pane,
    theme: ThemeDef, win: Size, panel_h: f32, grid: (u16, u16),
    option_alt: bool, heavy: bool, focus_term: bool, mods: keyboard::Modifiers,
    last: Instant, last_bytes: u64, last_frames: u64, last_frame_us: u64, last_vt_us: u64, last_draws: u64, last_draw_us: u64, msgs: u64, last_msgs: u64, wheel_ev: u64, wheel_lines: f32, scroll_dir: f32, scroll_n: u32,
    status: String, started: Instant, autorun: Option<String>, exit_after: Option<f32>,
}

impl App {
    fn new() -> (Self, Task<Message>) {
        let (tx, rx) = iced::futures::channel::mpsc::unbounded();
        WAKE.set(Mutex::new(Some(rx))).ok();
        let theme = THEMES[0];
        let font = FONTS.iter().copied().find(|f| Some(*f) == std::env::var("SPIKE_FONT").ok().as_deref()).unwrap_or(FONTS[0]);
        let size = env_f32("SPIKE_FONT_SIZE").unwrap_or(13.0);
        let term = term::spawn(80, 24, term_theme(&theme), tx);
        let pane = Pane { frame: Arc::new(Frame::default()), caches: vec![], last: vec![], bg: Cache::new(), cursor: Cache::new(), cell: measure(font, size), font, size, focused: true, scroll_mult: env_f32("SPIKE_SCROLL_MULT").unwrap_or(1.0), wheel_discrete: false.into(), draws: 0.into(), draw_us: 0.into() };
        let win = Size::new(env_f32("SPIKE_W").unwrap_or(1200.0), env_f32("SPIKE_H").unwrap_or(800.0));
        let panel_h = env_f32("SPIKE_PANEL").unwrap_or(420.0);
        (App { term, pane, theme, win, panel_h, grid: (0, 0), option_alt: true, heavy: std::env::var("SPIKE_HEAVY").is_ok(), focus_term: true, mods: Default::default(),
            last: Instant::now(), last_bytes: 0, last_frames: 0, last_frame_us: 0, last_vt_us: 0, last_draws: 0, last_draw_us: 0, msgs: 0, last_msgs: 0, wheel_ev: 0, wheel_lines: 0.0, scroll_dir: 2.0, scroll_n: 0,
            status: String::new(), started: Instant::now(), autorun: std::env::var("SPIKE_AUTORUN").ok(), exit_after: env_f32("SPIKE_EXIT_AFTER") }, Task::none())
    }

    fn regrid(&mut self) {
        let (cw, ch) = self.pane.cell;
        let w = self.win.width;
        let h = self.panel_h.min(self.win.height - TOOLBAR_H - STATUS_H);
        let g = (((w / cw).floor() as u16).max(2), ((h / ch).floor() as u16).max(1));
        if g != self.grid {
            self.grid = g;
            let _ = self.term.tx.send(Cmd::Resize { cols: g.0, rows: g.1, cell_w: cw, cell_h: ch });
        }
    }
    fn relayout_font(&mut self) {
        self.pane.cell = measure(self.pane.font, self.pane.size);
        self.grid = (0, 0); // force: cell px changed even if the grid didn't
        self.regrid();
        self.clear_all();
    }
    fn clear_all(&mut self) {
        for c in &self.pane.caches { c.clear(); }
        self.pane.bg.clear();
        self.pane.cursor.clear();
    }

    fn update(&mut self, m: Message) -> Task<Message> {
        self.msgs += 1;
        match m {
            Message::FrameReady => {
                self.term.notified.store(false, Release);
                let f = self.term.frame.lock().unwrap().clone();
                if self.pane.caches.len() != f.rows.len() { self.pane.caches.resize_with(f.rows.len(), Cache::new); self.pane.last.clear(); }
                for (y, r) in f.rows.iter().enumerate() {
                    if self.pane.last.get(y).map_or(true, |l| !Arc::ptr_eq(l, r)) { self.pane.caches[y].clear(); }
                }
                if f.bg != self.pane.frame.bg { self.pane.bg.clear(); }
                self.pane.cursor.clear();
                self.pane.last = f.rows.clone();
                self.pane.frame = f;
            }
            Message::Tick => {
                let s = self.term.stats.clone();
                let dt = self.last.elapsed().as_secs_f32();
                let (b, fr, fu, vu) = (s.bytes.load(Relaxed), s.frames.load(Relaxed), s.frame_us.load(Relaxed), s.vt_us.load(Relaxed));
                let (dw, du) = (self.pane.draws.get(), self.pane.draw_us.get());
                let dfr = fr - self.last_frames; let ddw = dw - self.last_draws;
                self.status = format!(
                    "{}x{} cell {:.2}x{:.0} | in {:.1} MB/s | vt {:.0}ms/s | frames {:.0}/s @{}us | draws {:.0}/s @{}us | msgs {:.0}/s | sb {} | wheel {:.0}ev/s {:.0}rows/s {} | {}{}{}{}",
                    self.grid.0, self.grid.1, self.pane.cell.0, self.pane.cell.1,
                    (b - self.last_bytes) as f32 / dt / 1e6, (vu - self.last_vt_us) as f32 / dt / 1e3,
                    dfr as f32 / dt, if dfr > 0 { (fu - self.last_frame_us) / dfr } else { 0 },
                    ddw as f32 / dt, if ddw > 0 { (du - self.last_draw_us) / ddw } else { 0 },
                    (self.msgs - self.last_msgs) as f32 / dt, self.pane.frame.scrollback,
                    self.wheel_ev as f32 / dt, self.wheel_lines.abs() / dt, if self.pane.wheel_discrete.get() { "mouse" } else { "trackpad" },
                    if self.focus_term { "KEYS→TERM" } else { "KEYS→APP" },
                    if self.pane.frame.alt { " alt" } else { "" }, if self.pane.frame.tracking { " mouse" } else { "" }, if self.pane.frame.exited { " EXITED" } else { "" },
                );
                eprintln!("[{:6.1}s] {}", self.started.elapsed().as_secs_f32(), self.status);
                if let Ok(path) = std::env::var("SPIKE_DUMP") {
                    let mut out = String::new();
                    for r in &self.pane.frame.rows {
                        let mut line = String::new();
                        for run in &r.runs { while line.chars().count() < run.col as usize { line.push(' '); } line.push_str(&run.text); }
                        out.push_str(&line); out.push('\n');
                    }
                    let _ = std::fs::write(path, out);
                }
                (self.last, self.last_bytes, self.last_frames, self.last_frame_us, self.last_vt_us, self.last_draws, self.last_draw_us, self.last_msgs) = (Instant::now(), b, fr, fu, vu, dw, du, self.msgs);
                self.wheel_ev = 0; self.wheel_lines = 0.0;
                if self.started.elapsed().as_secs_f32() > 1.5 {
                    if let Some(a) = self.autorun.take() { let _ = self.term.tx.send(Cmd::Write(format!("{a}\n").into_bytes())); }
                }
                // Fonts load asynchronously at boot; the first measurement may have used a fallback.
                let m = measure(self.pane.font, self.pane.size);
                if m != self.pane.cell { self.relayout_font(); }
                if let Some(e) = self.exit_after { if self.started.elapsed().as_secs_f32() > e { return iced::exit(); } }
            }
            Message::Win(s) => { self.win = s; self.regrid(); }
            Message::Mods(m) => self.mods = m,
            Message::KeyDown(phys, mods, txt) => return self.key(phys, mods, txt),
            Message::Pasted(Some(s)) => { let _ = self.term.tx.send(Cmd::Paste(s)); }
            Message::Pasted(None) => {}
            Message::PickTheme(t) => { self.theme = t; let _ = self.term.tx.send(Cmd::Theme(term_theme(&t))); }
            Message::PickFont(f) => { self.pane.font = f; self.relayout_font(); }
            Message::Size(s) => { self.pane.size = s.clamp(8.0, 32.0); self.relayout_font(); }
            Message::PanelH(p) => { self.panel_h = p; self.regrid(); self.clear_all(); }
            Message::OptionAlt(v) => self.option_alt = v,
            Message::Heavy(v) => self.heavy = v,
            Message::FocusTerm(v) => { self.focus_term = v; self.pane.focused = v; self.pane.cursor.clear(); }
            Message::MouseEv(k, x, y) => {
                if matches!(k, MouseKind::Press(_)) && !self.focus_term { return self.update(Message::FocusTerm(true)); }
                let _ = self.term.tx.send(Cmd::Mouse { kind: k, x, y, mods: gmods(self.mods) });
            }
            Message::ScrollMult(v) => self.pane.scroll_mult = v,
            Message::ScrollTest => {
                // Synthetic: scrolls up and down through history, about 250 rows/s.
                if self.started.elapsed().as_secs_f32() > 5.0 {
                    self.scroll_n += 1;
                    if self.scroll_n % 120 == 0 { self.scroll_dir = -self.scroll_dir; }
                    self.wheel_ev += 1; self.wheel_lines += self.scroll_dir;
                    let _ = self.term.tx.send(Cmd::Wheel { lines: self.scroll_dir, x: 10.0, y: 10.0, mods: gkey::Mods::empty() });
                }
            }
            Message::WheelEv(l, x, y) => { self.wheel_ev += 1; self.wheel_lines += l; let _ = self.term.tx.send(Cmd::Wheel { lines: l, x, y, mods: gmods(self.mods) }); }
            Message::Quit => return iced::exit(),
        }
        Task::none()
    }

    /// Map's keyboard rule: only a small Cmd set stays with the app; everything else goes to the terminal.
    fn key(&mut self, phys: Physical, mods: keyboard::Modifiers, txt: Option<String>) -> Task<Message> {
        let Physical::Code(code) = phys else { return Task::none() };
        if mods.logo() {
            return match code {
                Code::KeyJ => self.update(Message::FocusTerm(!self.focus_term)),
                Code::KeyQ => iced::exit(),
                Code::KeyV if self.focus_term => iced::clipboard::read().map(Message::Pasted),
                Code::Equal => self.update(Message::Size(self.pane.size + 1.0)),
                Code::Minus => self.update(Message::Size(self.pane.size - 1.0)),
                // Text-editing Cmd keys become the shell's control characters.
                Code::Backspace if self.focus_term => { let _ = self.term.tx.send(Cmd::Write(vec![0x15])); Task::none() }
                Code::ArrowLeft if self.focus_term => { let _ = self.term.tx.send(Cmd::Write(vec![0x01])); Task::none() }
                Code::ArrowRight if self.focus_term => { let _ = self.term.tx.send(Cmd::Write(vec![0x05])); Task::none() }
                _ => Task::none(),
            };
        }
        if !self.focus_term { return Task::none(); }
        if mods.alt() && self.option_alt {
            match code {
                Code::ArrowLeft => { let _ = self.term.tx.send(Cmd::Write(b"\x1bb".to_vec())); return Task::none(); }
                Code::ArrowRight => { let _ = self.term.tx.send(Cmd::Write(b"\x1bf".to_vec())); return Task::none(); }
                _ => {}
            }
        }
        let Some((k, unshifted)) = map_code(code) else { return Task::none() };
        // Control chars and macOS function-key PUA text must not reach the encoder as text.
        let text = txt.filter(|t| t.chars().all(|c| !c.is_control() && !('\u{F700}'..='\u{F8FF}').contains(&c)));
        let _ = self.term.tx.send(Cmd::Key(KeyInput { key: k, mods: gmods(mods), unshifted, text, option_as_alt: self.option_alt }));
        Task::none()
    }

    fn view(&self) -> Element<'_, Message> {
        let toolbar = row![
            pick_list(THEMES, Some(self.theme), Message::PickTheme),
            pick_list(FONTS, Some(self.pane.font), Message::PickFont),
            button("A-").on_press(Message::Size(self.pane.size - 1.0)),
            text(format!("{:.0}px", self.pane.size)),
            button("A+").on_press(Message::Size(self.pane.size + 1.0)),
            text("panel"),
            slider(120.0..=(self.win.height - TOOLBAR_H - STATUS_H).max(121.0), self.panel_h, Message::PanelH).width(160),
            text("scroll"),
            slider(1.0..=6.0, self.pane.scroll_mult, Message::ScrollMult).step(0.5).width(70),
            text(format!("{:.1}x", self.pane.scroll_mult)),
            checkbox("Option=Alt", self.option_alt).on_toggle(Message::OptionAlt),
            checkbox("Heavy app view", self.heavy).on_toggle(Message::Heavy),
            checkbox("Keys→terminal (Cmd+J)", self.focus_term).on_toggle(Message::FocusTerm),
            button("Quit").on_press(Message::Quit),
        ].spacing(10).padding(6).align_y(iced::Alignment::Center).height(TOOLBAR_H);

        // Stand-in for the app's board. "Heavy" approximates the cost of rebuilding the real App view per message.
        let app: Element<_> = if self.heavy {
            let mut c = column![].spacing(1);
            for i in 0..6000 { c = c.push(text(format!("JRST-{i} fake issue row standing in for a real board card")).size(12)); }
            scrollable(c).height(Length::Fill).width(Length::Fill).into()
        } else {
            container(text("Fake app area (toggle Heavy app view to stress view rebuilds)").size(13)).padding(10).height(Length::Fill).width(Length::Fill).into()
        };

        let term = Canvas::new(&self.pane).width(Length::Fill).height(self.panel_h.min(self.win.height - TOOLBAR_H - STATUS_H));
        let status = container(text(&self.status).size(11).font(Font::MONOSPACE)).padding(4).height(STATUS_H);
        column![toolbar, app, term, status].into()
    }

    fn subscription(&self) -> Subscription<Message> {
        let frames = Subscription::run(|| iced::stream::channel(8, |mut out| async move {
            let mut rx = WAKE.get().unwrap().lock().unwrap().take().unwrap();
            while rx.next().await.is_some() { let _ = out.send(Message::FrameReady).await; }
            std::future::pending::<()>().await
        }));
        let ev = event::listen_with(|e, _status, _id| match e {
            Event::Keyboard(keyboard::Event::KeyPressed { physical_key, modifiers, text, .. }) => Some(Message::KeyDown(physical_key, modifiers, text.map(|t| t.to_string()))),
            Event::Keyboard(keyboard::Event::ModifiersChanged(m)) => Some(Message::Mods(m)),
            Event::Window(window::Event::Opened { size, .. }) | Event::Window(window::Event::Resized(size)) => Some(Message::Win(size)),
            _ => None,
        });
        let mut subs = vec![frames, ev, iced::time::every(std::time::Duration::from_secs(1)).map(|_| Message::Tick)];
        if std::env::var("SPIKE_SCROLLTEST").is_ok() { subs.push(iced::time::every(std::time::Duration::from_millis(8)).map(|_| Message::ScrollTest)); }
        Subscription::batch(subs)
    }
    fn theme(&self) -> Theme { if self.theme.dark { Theme::Dark } else { Theme::Light } }
}

fn gmods(m: keyboard::Modifiers) -> gkey::Mods {
    let mut g = gkey::Mods::empty();
    if m.shift() { g |= gkey::Mods::SHIFT; }
    if m.alt() { g |= gkey::Mods::ALT; }
    if m.control() { g |= gkey::Mods::CTRL; }
    if m.logo() { g |= gkey::Mods::SUPER; }
    g
}

fn map_code(c: Code) -> Option<(gkey::Key, char)> {
    use gkey::Key as K;
    macro_rules! t { ($($a:ident => $k:ident, $ch:expr;)*) => { match c { $(Code::$a => Some((K::$k, $ch)),)* _ => None } } }
    t! {
        KeyA=>A,'a'; KeyB=>B,'b'; KeyC=>C,'c'; KeyD=>D,'d'; KeyE=>E,'e'; KeyF=>F,'f'; KeyG=>G,'g'; KeyH=>H,'h'; KeyI=>I,'i'; KeyJ=>J,'j'; KeyK=>K,'k'; KeyL=>L,'l'; KeyM=>M,'m';
        KeyN=>N,'n'; KeyO=>O,'o'; KeyP=>P,'p'; KeyQ=>Q,'q'; KeyR=>R,'r'; KeyS=>S,'s'; KeyT=>T,'t'; KeyU=>U,'u'; KeyV=>V,'v'; KeyW=>W,'w'; KeyX=>X,'x'; KeyY=>Y,'y'; KeyZ=>Z,'z';
        Digit0=>Digit0,'0'; Digit1=>Digit1,'1'; Digit2=>Digit2,'2'; Digit3=>Digit3,'3'; Digit4=>Digit4,'4'; Digit5=>Digit5,'5'; Digit6=>Digit6,'6'; Digit7=>Digit7,'7'; Digit8=>Digit8,'8'; Digit9=>Digit9,'9';
        Backquote=>Backquote,'`'; Backslash=>Backslash,'\\'; BracketLeft=>BracketLeft,'['; BracketRight=>BracketRight,']'; Comma=>Comma,','; Equal=>Equal,'='; Minus=>Minus,'-'; Period=>Period,'.'; Quote=>Quote,'\''; Semicolon=>Semicolon,';'; Slash=>Slash,'/'; Space=>Space,' ';
        Enter=>Enter,'\0'; Tab=>Tab,'\0'; Backspace=>Backspace,'\0'; Escape=>Escape,'\0'; ArrowUp=>ArrowUp,'\0'; ArrowDown=>ArrowDown,'\0'; ArrowLeft=>ArrowLeft,'\0'; ArrowRight=>ArrowRight,'\0';
        Home=>Home,'\0'; End=>End,'\0'; PageUp=>PageUp,'\0'; PageDown=>PageDown,'\0'; Insert=>Insert,'\0'; Delete=>Delete,'\0';
        F1=>F1,'\0'; F2=>F2,'\0'; F3=>F3,'\0'; F4=>F4,'\0'; F5=>F5,'\0'; F6=>F6,'\0'; F7=>F7,'\0'; F8=>F8,'\0'; F9=>F9,'\0'; F10=>F10,'\0'; F11=>F11,'\0'; F12=>F12,'\0';
    }
}

fn main() -> iced::Result {
    let mut app = iced::application("JAIT terminal spike (PROTOTYPE, JRST-328)", App::update, App::view)
        .subscription(App::subscription)
        .theme(App::theme)
        .window_size(Size::new(env_f32("SPIKE_W").unwrap_or(1200.0), env_f32("SPIKE_H").unwrap_or(800.0)));
    for b in [
        &include_bytes!("../../../assets/fonts/JetBrainsMono-Regular.ttf")[..], &include_bytes!("../../../assets/fonts/JetBrainsMono-Bold.ttf")[..], &include_bytes!("../../../assets/fonts/JetBrainsMono-Italic.ttf")[..], &include_bytes!("../../../assets/fonts/JetBrainsMono-BoldItalic.ttf")[..],
        &include_bytes!("../../../assets/fonts/IBMPlexMono-Regular.ttf")[..], &include_bytes!("../../../assets/fonts/IBMPlexMono-Bold.ttf")[..],
        &include_bytes!("../../../assets/fonts/FiraCode-Regular.ttf")[..], &include_bytes!("../../../assets/fonts/FiraCode-Bold.ttf")[..],
        &include_bytes!("../../../assets/fonts/SUSEMono-Regular.ttf")[..], &include_bytes!("../../../assets/fonts/SUSEMono-Bold.ttf")[..],
        &include_bytes!("../../../assets/fonts/RobotoMono-Regular.ttf")[..], &include_bytes!("../../../assets/fonts/RobotoMono-Bold.ttf")[..],
        &include_bytes!("../../../assets/fonts/AtkinsonHyperlegibleMono-Regular.ttf")[..], &include_bytes!("../../../assets/fonts/AtkinsonHyperlegibleMono-Bold.ttf")[..],
    ] { app = app.font(b); }
    app.run_with(App::new)
}
