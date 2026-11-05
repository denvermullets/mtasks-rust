//! The docked Terminal panel: a strip, and a canvas that draws one pane.
//!
//! API: `terminal_panel::view(PanelView { .. })` builds the panel;
//! `Pane` holds what the canvas draws and its per-row caches. Drawing is ported
//! from the JRST-328 spike. Every row has its own `Cache`, so a frame only
//! re-lays-out the rows whose `Arc<RowData>` changed. ASCII runs use Basic
//! shaping (no ligatures, no fallback); other runs use Advanced shaping.

use std::sync::Arc;

use iced::advanced::text::{self as adv_text, Paragraph as _};
use iced::alignment::{Horizontal, Vertical};
use iced::keyboard;
use iced::mouse::{self, Cursor};
use iced::widget::canvas::{self, Cache, Canvas, Geometry, Text};
use iced::widget::{button, column, container, row, text, Space};
use iced::{font, Color, Element, Font, Length, Pixels, Point, Rectangle, Renderer, Size, Theme};

use crate::app::Message;
use crate::terminal::panel::STRIP_HEIGHT;
use crate::terminal::{
    pointer, CursorShape, Frame, KeyMods, MouseAction, MouseButton, MouseInput, Rgb, RowData, Run,
    TerminalMessage, WheelInput,
};
use crate::theme as t;

/// Family name inside the bundled JetBrains Mono files.
pub const FONT_FAMILY: &str = "JetBrains Mono";
pub const FONT_SIZE: f32 = 13.0;

fn col(c: Rgb) -> Color {
    Color::from_rgb8(c[0], c[1], c[2])
}

/// Indexes of rows whose data differs between two frames. A different row
/// count changes every row.
pub fn changed_rows(prev: &[Arc<RowData>], next: &[Arc<RowData>]) -> Vec<usize> {
    if prev.len() != next.len() {
        return (0..next.len()).collect();
    }
    (0..next.len())
        .filter(|&y| !Arc::ptr_eq(&prev[y], &next[y]))
        .collect()
}

/// What the canvas remembers between events: modifiers, whether a button is held
/// over the pane, and the last cell it reported a move for.
#[derive(Default)]
pub struct PaneState {
    mods: keyboard::Modifiers,
    pressed: bool,
    last_cell: (i32, i32),
}

fn map_button(b: mouse::Button) -> Option<MouseButton> {
    match b {
        mouse::Button::Left => Some(MouseButton::Left),
        mouse::Button::Middle => Some(MouseButton::Middle),
        mouse::Button::Right => Some(MouseButton::Right),
        _ => None,
    }
}

fn key_mods(m: keyboard::Modifiers) -> KeyMods {
    KeyMods { shift: m.shift(), alt: m.alt(), ctrl: m.control(), logo: m.logo() }
}

/// What the canvas draws: the latest frame plus a cache per row.
pub struct Pane {
    frame: Arc<Frame>,
    caches: Vec<Cache>,
    last: Vec<Arc<RowData>>,
    bg: Cache,
    cursor: Cache,
    cell: (f32, f32),
    focused: bool,
}

impl Pane {
    pub fn new() -> Self {
        Pane {
            frame: Arc::new(Frame::default()),
            caches: Vec::new(),
            last: Vec::new(),
            bg: Cache::new(),
            cursor: Cache::new(),
            cell: (FONT_SIZE * 0.6, (FONT_SIZE * 1.25).ceil()),
            focused: false,
        }
    }

    /// Take a new frame, clearing only the caches of rows that changed.
    pub fn show(&mut self, frame: Arc<Frame>) {
        let changed = changed_rows(&self.last, &frame.rows);
        if self.caches.len() != frame.rows.len() {
            self.caches.resize_with(frame.rows.len(), Cache::new);
        }
        for y in changed {
            self.caches[y].clear();
        }
        if frame.bg != self.frame.bg {
            self.bg.clear();
        }
        self.cursor.clear();
        self.last = frame.rows.clone();
        self.frame = frame;
    }

    /// Forget everything (another Terminal, a theme change, a new cell size).
    pub fn reset(&mut self) {
        for c in &self.caches {
            c.clear();
        }
        self.bg.clear();
        self.cursor.clear();
        self.last.clear();
    }

    pub fn set_cell(&mut self, cell: (f32, f32)) {
        if cell != self.cell {
            self.cell = cell;
            self.reset();
        }
    }

    pub fn set_focused(&mut self, focused: bool) {
        if focused != self.focused {
            self.focused = focused;
            self.cursor.clear();
        }
    }

    #[cfg(test)]
    pub fn frame(&self) -> &Arc<Frame> {
        &self.frame
    }
}

impl Default for Pane {
    fn default() -> Self {
        Self::new()
    }
}

impl canvas::Program<Message> for Pane {
    type State = PaneState;

    fn update(
        &self,
        state: &mut PaneState,
        event: canvas::Event,
        bounds: Rectangle,
        cursor: Cursor,
    ) -> (canvas::event::Status, Option<Message>) {
        use canvas::event::Status::{Captured, Ignored};
        if let canvas::Event::Keyboard(keyboard::Event::ModifiersChanged(m)) = event {
            state.mods = m;
            return (Ignored, None);
        }
        let canvas::Event::Mouse(ev) = event else {
            return (Ignored, None);
        };
        let send = |input: MouseInput| Some(Message::Terminal(TerminalMessage::Mouse(input)));
        let at = |p: Point| {
            let (c, r) = pointer::cell_at(p.x - bounds.x, p.y - bounds.y, self.cell.0, self.cell.1);
            (c, r)
        };
        let mods = key_mods(state.mods);
        match ev {
            mouse::Event::ButtonPressed(b) => {
                let Some(p) = cursor.position_in(bounds) else {
                    return (Ignored, None);
                };
                let Some(button) = map_button(b) else {
                    return (Ignored, None);
                };
                state.pressed = true;
                let (col, row) = at(bounds.position() + iced::Vector::new(p.x, p.y));
                (
                    Captured,
                    send(MouseInput { action: MouseAction::Press(button), col, row, mods }),
                )
            }
            mouse::Event::ButtonReleased(b) => {
                let Some(button) = map_button(b) else {
                    return (Ignored, None);
                };
                if !std::mem::take(&mut state.pressed) {
                    return (Ignored, None);
                }
                let (col, row) = cursor.position().map(at).unwrap_or(state.last_cell);
                (
                    Captured,
                    send(MouseInput { action: MouseAction::Release(button), col, row, mods }),
                )
            }
            mouse::Event::CursorMoved { position } => {
                // Moves matter only to a program tracking the mouse; send one per cell.
                if !self.frame.tracking || !(state.pressed || cursor.is_over(bounds)) {
                    return (Ignored, None);
                }
                let cell = at(position);
                if cell == state.last_cell {
                    return (Ignored, None);
                }
                state.last_cell = cell;
                (
                    Ignored,
                    send(MouseInput { action: MouseAction::Move, col: cell.0, row: cell.1, mods }),
                )
            }
            mouse::Event::WheelScrolled { delta } => {
                let Some(p) = cursor.position_in(bounds) else {
                    return (Ignored, None);
                };
                let (y, pixels) = match delta {
                    mouse::ScrollDelta::Lines { y, .. } => (y, false),
                    mouse::ScrollDelta::Pixels { y, .. } => (y, true),
                };
                let (col, row) = pointer::cell_at(p.x, p.y, self.cell.0, self.cell.1);
                let rows = pointer::wheel_rows(y, pixels, self.cell.1);
                (Captured, Some(Message::Terminal(TerminalMessage::Wheel(WheelInput { rows, col, row, mods }))))
            }
            _ => (Ignored, None),
        }
    }

    fn draw(
        &self,
        _state: &PaneState,
        renderer: &Renderer,
        _theme: &Theme,
        bounds: Rectangle,
        _cursor: Cursor,
    ) -> Vec<Geometry> {
        let (cw, ch) = self.cell;
        let f = &self.frame;
        let size = bounds.size();
        let mut out = Vec::with_capacity(f.rows.len() + 2);
        out.push(
            self.bg
                .draw(renderer, size, |fr| fr.fill_rectangle(Point::ORIGIN, size, col(f.bg))),
        );
        let base = Font::with_name(FONT_FAMILY);
        for (y, data) in f.rows.iter().enumerate() {
            let Some(cache) = self.caches.get(y) else {
                break;
            };
            let py = y as f32 * ch;
            out.push(cache.draw(renderer, size, |fr| {
                for g in &data.bgs {
                    fr.fill_rectangle(
                        Point::new(g.col as f32 * cw, py),
                        Size::new(g.len as f32 * cw, ch),
                        col(g.color),
                    );
                }
                for run in &data.runs {
                    draw_run(fr, run, base, cw, ch, py, None, f.bg);
                }
            }));
        }
        out.push(self.cursor.draw(renderer, size, |fr| {
            let Some(c) = f.cursor else {
                return;
            };
            let (px, py) = (c.x as f32 * cw, c.y as f32 * ch);
            let color = col(c.color);
            match (self.focused, c.shape) {
                (false, _) => fr.stroke(
                    &canvas::Path::rectangle(
                        Point::new(px + 0.5, py + 0.5),
                        Size::new(cw - 1.0, ch - 1.0),
                    ),
                    canvas::Stroke::default().with_color(color).with_width(1.0),
                ),
                (_, CursorShape::Bar) => {
                    fr.fill_rectangle(Point::new(px, py), Size::new(2.0, ch), color)
                }
                (_, CursorShape::Underline) => {
                    fr.fill_rectangle(Point::new(px, py + ch - 2.0), Size::new(cw, 2.0), color)
                }
                (_, CursorShape::Block) => {
                    fr.fill_rectangle(Point::new(px, py), Size::new(cw, ch), color);
                    // Redraw the covered character in the background colour.
                    let covered = f.rows.get(c.y as usize).and_then(|data| {
                        data.runs
                            .iter()
                            .find(|r| c.x >= r.col && c.x < r.col + r.cells)
                    });
                    if let Some(run) = covered {
                        let chr = if run.ascii {
                            run.text.chars().nth((c.x - run.col) as usize)
                        } else {
                            run.text.chars().next()
                        };
                        if let Some(chr) = chr {
                            let one = Run {
                                col: c.x,
                                text: chr.to_string(),
                                cells: 1,
                                ..run.clone()
                            };
                            draw_run(fr, &one, base, cw, ch, py, Some(f.bg), f.bg);
                        }
                    }
                }
            }
        }));
        out
    }

    fn mouse_interaction(&self, _s: &PaneState, bounds: Rectangle, cursor: Cursor) -> mouse::Interaction {
        if cursor.is_over(bounds) {
            mouse::Interaction::Text
        } else {
            mouse::Interaction::default()
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn draw_run(
    fr: &mut canvas::Frame,
    run: &Run,
    base: Font,
    cw: f32,
    ch: f32,
    py: f32,
    over: Option<Rgb>,
    bg: Rgb,
) {
    let x = run.col as f32 * cw;
    let mut c = col(over.unwrap_or(run.fg));
    if run.faint {
        let b = col(bg);
        c = Color::from_rgb((c.r + b.r) / 2.0, (c.g + b.g) / 2.0, (c.b + b.b) / 2.0);
    }
    let font = Font {
        weight: if run.bold { font::Weight::Bold } else { font::Weight::Normal },
        style: if run.italic { font::Style::Italic } else { font::Style::Normal },
        ..base
    };
    if !run.text.trim_end().is_empty() {
        fr.fill_text(Text {
            content: run.text.clone(),
            position: Point::new(x, py),
            color: c,
            size: Pixels(FONT_SIZE),
            line_height: iced::widget::text::LineHeight::Absolute(Pixels(ch)),
            font,
            horizontal_alignment: Horizontal::Left,
            vertical_alignment: Vertical::Top,
            shaping: if run.ascii {
                iced::widget::text::Shaping::Basic
            } else {
                iced::widget::text::Shaping::Advanced
            },
        });
    }
    let w = run.cells as f32 * cw;
    if run.underline {
        fr.fill_rectangle(Point::new(x, py + ch - 2.0), Size::new(w, 1.0), c);
    }
    if run.strike {
        fr.fill_rectangle(Point::new(x, py + ch / 2.0), Size::new(w, 1.0), c);
    }
}

/// Pixel size of one cell for the Terminal font: width of an "M", and a line
/// height of 1.25 em. Needs the font loaded, so call it after startup.
pub fn measure_cell() -> (f32, f32) {
    type P = <Renderer as adv_text::Renderer>::Paragraph;
    let ch = (FONT_SIZE * 1.25).ceil();
    let n = 100.0;
    let p = P::with_text(adv_text::Text {
        content: &"M".repeat(n as usize),
        bounds: Size::INFINITY,
        size: Pixels(FONT_SIZE),
        line_height: iced::widget::text::LineHeight::Absolute(Pixels(ch)),
        font: Font::with_name(FONT_FAMILY),
        horizontal_alignment: Horizontal::Left,
        vertical_alignment: Vertical::Top,
        shaping: iced::widget::text::Shaping::Basic,
        wrapping: iced::widget::text::Wrapping::None,
    });
    (p.min_bounds().width / n, ch)
}

/// Everything the panel view needs, as plain borrowed data.
pub struct PanelView<'a> {
    /// The pane to draw, or `None` for a team with no Terminal yet.
    pub pane: Option<&'a Pane>,
    pub title: String,
    /// A line for the strip, such as an exited notice or a spawn error.
    pub note: Option<String>,
    pub height: f32,
}

pub fn view<'a>(p: PanelView<'a>) -> Element<'a, Message> {
    let mut strip = row![text(p.title).size(12).color(t::text_secondary())]
        .spacing(12)
        .align_y(iced::Alignment::Center);
    if let Some(note) = p.note {
        strip = strip.push(text(note).size(12).color(t::text_muted()));
    }
    strip = strip
        .push(Space::with_width(Length::Fill))
        .push(text("Cmd+J").size(11).color(t::text_muted()));
    let strip = container(strip)
        .padding([0, 10])
        .height(STRIP_HEIGHT)
        .center_y(STRIP_HEIGHT)
        .width(Length::Fill)
        .style(|_theme: &Theme| container::Style {
            background: Some(iced::Background::Color(t::bg_menu())),
            border: iced::Border { color: t::border(), width: 1.0, radius: 0.0.into() },
            ..Default::default()
        });

    let body: Element<'a, Message> = match p.pane {
        Some(pane) => Canvas::new(pane).width(Length::Fill).height(Length::Fill).into(),
        None => container(
            button(text("New Terminal").size(13))
                .padding([6, 14])
                .on_press(Message::Terminal(TerminalMessage::New)),
        )
        .center_x(Length::Fill)
        .center_y(Length::Fill)
        .into(),
    };
    column![strip, body]
        .width(Length::Fill)
        .height(Length::Fixed(p.height))
        .into()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row_of(text: &str) -> Arc<RowData> {
        Arc::new(RowData {
            runs: vec![Run {
                col: 0,
                text: text.into(),
                cells: text.len() as u16,
                fg: [1, 2, 3],
                bold: false,
                italic: false,
                faint: false,
                underline: false,
                strike: false,
                ascii: true,
            }],
            bgs: vec![],
        })
    }

    #[test]
    fn only_rows_with_new_data_count_as_changed() {
        let (a, b, c) = (row_of("a"), row_of("b"), row_of("c"));
        let prev = vec![a.clone(), b.clone(), c.clone()];
        let next = vec![a.clone(), row_of("b2"), c.clone()];
        assert_eq!(changed_rows(&prev, &next), vec![1]);
        assert_eq!(changed_rows(&prev, &prev.clone()), Vec::<usize>::new());
    }

    #[test]
    fn a_new_row_count_changes_every_row() {
        let a = row_of("a");
        assert_eq!(changed_rows(&[a.clone()], &[a.clone(), a.clone()]), vec![0, 1]);
        assert_eq!(changed_rows(&[], &[a]), vec![0]);
    }

    #[test]
    fn showing_a_frame_keeps_one_cache_per_row() {
        let mut pane = Pane::new();
        let frame = Frame { cols: 80, rows: vec![row_of("x"); 24], ..Default::default() };
        pane.show(Arc::new(frame));
        assert_eq!(pane.caches.len(), 24);
        assert_eq!(pane.frame().rows.len(), 24);
    }
}
