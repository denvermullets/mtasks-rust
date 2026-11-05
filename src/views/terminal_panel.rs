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
use iced::mouse::{self, Cursor};
use iced::widget::canvas::{self, Cache, Canvas, Geometry, Text};
use iced::widget::{button, column, container, row, text, Space};
use iced::{font, Color, Element, Font, Length, Pixels, Point, Rectangle, Renderer, Size, Theme};

use crate::app::Message;
use crate::terminal::panel::STRIP_HEIGHT;
use crate::terminal::{CursorShape, Frame, Rgb, Run, RowData, TerminalMessage};
use crate::theme as t;

pub const FONT_SIZE: f32 = 13.0;

/// The Terminal font: a bundled monospace family and a size, both from config.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TermFont {
    pub family: &'static str,
    pub size: f32,
}

impl Default for TermFont {
    fn default() -> Self {
        TermFont {
            family: crate::fonts::terminal_family(crate::fonts::DEFAULT_TERMINAL_FONT_KEY),
            size: FONT_SIZE,
        }
    }
}

impl TermFont {
    /// The font a config key names, at `size`. An unknown key gives the default family.
    pub fn from_key(key: &str, size: f32) -> Self {
        TermFont { family: crate::fonts::terminal_family(key), size: crate::fonts::clamp_terminal_size(size) }
    }

    /// Height of one cell: 1.25 em, rounded up.
    fn line_height(&self) -> f32 {
        (self.size * 1.25).ceil()
    }
}

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

/// What the canvas draws: the latest frame plus a cache per row.
pub struct Pane {
    frame: Arc<Frame>,
    caches: Vec<Cache>,
    last: Vec<Arc<RowData>>,
    bg: Cache,
    cursor: Cache,
    cell: (f32, f32),
    font: TermFont,
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
            font: TermFont::default(),
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

    /// A new font or size: the glyphs change, so every row redraws. The cell
    /// size follows through `set_cell`.
    pub fn set_font(&mut self, font: TermFont) {
        if font != self.font {
            self.font = font;
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
    type State = ();

    fn update(
        &self,
        _state: &mut (),
        event: canvas::Event,
        bounds: Rectangle,
        cursor: Cursor,
    ) -> (canvas::event::Status, Option<Message>) {
        if cursor.position_in(bounds).is_some() {
            if let canvas::Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)) = event {
                return (
                    canvas::event::Status::Captured,
                    Some(Message::Terminal(TerminalMessage::Focus)),
                );
            }
        }
        (canvas::event::Status::Ignored, None)
    }

    fn draw(
        &self,
        _state: &(),
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
        let base = Font::with_name(self.font.family);
        let font_size = self.font.size;
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
                    draw_run(fr, run, base, font_size, cw, ch, py, None, f.bg);
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
                            draw_run(fr, &one, base, font_size, cw, ch, py, Some(f.bg), f.bg);
                        }
                    }
                }
            }
        }));
        out
    }

    fn mouse_interaction(&self, _s: &(), bounds: Rectangle, cursor: Cursor) -> mouse::Interaction {
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
    font_size: f32,
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
            size: Pixels(font_size),
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
pub fn measure_cell(font: TermFont) -> (f32, f32) {
    type P = <Renderer as adv_text::Renderer>::Paragraph;
    let ch = font.line_height();
    let n = 100.0;
    let p = P::with_text(adv_text::Text {
        content: &"M".repeat(n as usize),
        bounds: Size::INFINITY,
        size: Pixels(font.size),
        line_height: iced::widget::text::LineHeight::Absolute(Pixels(ch)),
        font: Font::with_name(font.family),
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
